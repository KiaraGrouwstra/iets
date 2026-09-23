//! `ietsp fast-build` (also `nix-fast-build`): nix-fast-build's command line
//! over ietsp's own evaluator and build dispatcher, in one process.
//!
//! The evaluation walks the attribute tree on the main fiber and hands every
//! finished attribute to a dispatch thread through `jobs::stream`. The
//! dispatch thread filters, uploads and builds each batch on one connection
//! through `crate::build::Dispatcher`, so building starts while the rest of
//! the tree is still evaluating. See docs/fast-build.md.

pub mod flags;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::time::Instant;

use flags::{Args, EvalMode, Format, Reject};

use crate::build::render::{Mode, Style, actions_ci, is_tty};
use crate::build::results::{self, Record};
use crate::build::upload::StreamUpload;
use crate::build::{Dispatcher, Target};
use crate::builtins::derivation::DrvInfo;
use crate::config::Config;
use crate::error::{Error as CrateError, Result};
use crate::eval::{Evaluator, Known, Pos};
use crate::jobs::line::Line;
use crate::jobs::stream::{Batcher, Info, Stream};
use crate::parallel::shared::SharedMap;
use crate::remote::worker::{Options, Transport};
use crate::store::path::StorePath;

const NAME: &str = "nix-fast-build";

/// nixpkgs's check-meta wording for a `meta.platforms`/`badPlatforms` miss
/// (workers.py:26).
const UNSUPPORTED_MARKER: &str = "is not available on the requested hostPlatform";

/// What the dispatch thread needs; everything else stays with the evaluator.
struct Dispatch {
    transport: Transport,
    options: Options,
    style: Style,
    systems: Vec<String>,
    max_jobs: usize,
    skip_cached: bool,
    skip_unsupported: bool,
    out_link: Option<String>,
    retries: usize,
    fail_fast: bool,
    stream_json_lines: bool,
    drvs: SharedMap<StorePath, DrvInfo>,
    sources: SharedMap<StorePath, Known>,
}

/// What the run produced, for the result file and the exit code.
struct Report {
    records: Vec<Record>,
    failed: bool,
}

pub fn cmd_fast_build(argv: &[String]) -> i32 {
    let a = match flags::parse(argv) {
        Ok(a) => a,
        Err(Reject::Refuse(m)) => return CrateError::Unsupported(m).report(),
        Err(r) => {
            let native = crate::cli::help::command().is_some();
            match r {
                Reject::Flag(f) if native => crate::cli::help::unknown_flag(&f),
                Reject::Flag(f) => {
                    crate::cli::classic::fallback(NAME, argv, &format!("'{f}' is not implemented"))
                }
                Reject::Unknown(m) if native => crate::cli::help::arg_error(m),
                Reject::Unknown(m) | Reject::Refuse(m) => {
                    crate::cli::classic::fallback(NAME, argv, &m)
                }
            }
        }
    };
    match run(a) {
        Ok(code) => code,
        Err(e) => e.report(),
    }
}

fn run(a: Args) -> Result<i32> {
    let cfg = Config::load(&a.flags.overrides)?;
    let store = crate::settings::builder(a.store.as_deref(), &cfg);
    let mut transport = Transport::parse_checked(&store, &cfg.system())?.ok_or_else(|| {
        CrateError::Usage(format!(
            "--store '{store}': ietsp fast-build speaks the Nix worker protocol and grpc://: daemon, unix:///path, ssh-ng://[user@]host or grpc://host:port"
        ))
    })?;
    let mut options = Options::try_from(&cfg)?;
    let max_jobs = jobs(a.max_jobs, &mut transport, &mut options);
    // Every attribute gets a verdict unless --fail-fast says to stop.
    options.keep_going = !a.fail_fast;
    if !matches!(transport, Transport::Unix(_)) && !a.no_download {
        crate::log::line(
            "ietsp fast-build: --store is not the local daemon, so the outputs stay there; --out-link points at nothing local (docs/fast-build.md)",
        );
    }

    let systems = if a.systems.is_empty() {
        vec![cfg.system()]
    } else {
        a.systems.clone()
    };
    let style = Style {
        mode: if is_tty() && !a.stream_json_lines {
            Mode::Tty
        } else {
            Mode::Ci
        },
        color: is_tty() && std::env::var_os("NO_COLOR").is_none(),
        fold: actions_ci() && !a.no_fold,
        quiet: false,
    };

    // nix-fast-build hands nix-eval-jobs `--flake`, and `--impure` when impure
    let conf_pure = cfg.bool("pure-eval").ok().flatten() == Some(true);
    let pure = !a.impure && (a.mode == EvalMode::Flake || conf_pure);
    let mut ev = Evaluator::with(&cfg.system(), pure);
    // The uploader ships each job's closure from memory, so the ATerms stay.
    ev.drv.keep_aterm = true;
    crate::builtins::set_search_path(&mut ev, crate::cli::full_includes(&a.flags.includes));

    let d = Dispatch {
        transport,
        options,
        style,
        systems,
        max_jobs,
        skip_cached: a.skip_cached,
        skip_unsupported: a.skip_unsupported,
        out_link: a.out_link.clone(),
        retries: a.retries,
        fail_fast: a.fail_fast,
        stream_json_lines: a.stream_json_lines,
        drvs: ev.drv.drvs.clone(),
        sources: ev.known.clone(),
    };

    let (stream, rx) = Stream::new();
    let thread = std::thread::Builder::new()
        .name("fast-build".into())
        .stack_size(64 << 20)
        .spawn(move || dispatch(d, rx))
        .map_err(|e| CrateError::io("cannot spawn the dispatch thread", e))?;

    let workers = a
        .eval_workers
        .unwrap_or_else(crate::parallel::default_workers);
    let result_file = a.result_file.clone();
    let result_format = a.result_format;
    let suite = a.display_name();
    let eval_ok = crate::parallel::run(workers, ev, move |ev| walk(ev, a, stream));

    let report = thread
        .join()
        .map_err(|_| CrateError::Protocol("the fast-build dispatch thread panicked".into()))?;
    let mut report = report?;
    report.failed |= !eval_ok;

    if let Some(path) = &result_file {
        write_result_file(path, result_format, &suite, &report.records)?;
    }
    let code = i32::from(report.failed);
    if let Some(path) = results::step_summary_file() {
        write_step_summary(&path, &report.records, code);
    }
    summary(&report);
    Ok(code)
}

/// The eval side: the root, then the walk, every line into the stream.
/// `false` when the root itself failed to evaluate.
fn walk(ev: &mut Evaluator, a: Args, stream: Stream) -> bool {
    let cwd = crate::cli::cwd();
    let opts = crate::jobs::Opts {
        // nix-fast-build always passes --force-recurse (processes.py:103), so
        // `.#checks` reaches `checks.<system>.<name>` without the flake
        // marking every level `recurseForDerivations`.
        force_recurse: true,
        meta: false,
        show_input_drvs: false,
        no_instantiate: false,
        constituents: false,
        chunks: 1,
        chunk: 0,
        chunk_depth: 2,
        // Carries each job's source position, which the renderer shows.
        provenance: crate::provenance::enabled(),
    };
    let root = (|| -> std::result::Result<crate::runtime::Word, Box<crate::eval::Error>> {
        let args = crate::cli::auto_args(ev, &a.flags.args, &cwd)?;
        crate::jobs::configure(ev, opts, args, None);
        let input = match a.mode {
            EvalMode::Flake => crate::cli::Input::Flake(a.flake.clone()),
            EvalMode::File => crate::cli::Input::File(a.file.clone()),
        };
        let root = crate::cli::evaluate(
            ev,
            crate::cli::EvalArgs {
                input: Some(input),
                no_cache: true,
                flake_whole: true,
                ..crate::cli::parse_eval_args(&[])
            },
        )?;
        let root = ev.auto_call(root, args)?;
        // `-A attr` is the walk root, selected after the auto-call, the way
        // nix-fast-build's `--select 'root: root.attr'` is (processes.py:128).
        let root = match a.flags.attrs.last() {
            Some(attr) => {
                let path: Vec<String> = attr
                    .split('.')
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect();
                crate::jobs::find_along_attr_path(ev, root, &path)?
            }
            None => root,
        };
        let Some(select) = &a.select else {
            return Ok(root);
        };
        let f = ev.eval_string(select, &cwd)?;
        let v = ev.apply(f, &[root], Pos::NONE)?;
        ev.force_attrs(
            v,
            Pos::NONE,
            "'--select' must evaluate to an attrset (the traversal root)",
        )?;
        Ok(v)
    })();
    let root = match root {
        Ok(r) => r,
        Err(e) => {
            crate::cli::report(ev, &e);
            return false;
        }
    };
    let mut sink = stream.sink();
    crate::jobs::run(ev, root, &mut sink);
    true
}

/// The attributes per batch: the backend's build parallelism. Over `grpc://`
/// that is `max-builds`, as in nix-grpc-store; nix.conf max-jobs is about local
/// builds, so only an explicit `-j` lowers it.
fn jobs(j: Option<u64>, transport: &mut Transport, options: &mut Options) -> usize {
    // nix-fast-build's 0 means "unlimited"; the daemon wants a number.
    if let Some(j) = j.filter(|&j| j > 0) {
        options.max_jobs = j;
        if let Transport::Grpc(g) = transport {
            g.max_builds = g.max_builds.min(j as usize);
        }
    }
    match transport {
        Transport::Grpc(g) => g.max_builds,
        _ => options.max_jobs.max(1) as usize,
    }
}

/// The build side: batches in, verdicts out, one connection.
fn dispatch(d: Dispatch, rx: Receiver<Line>) -> Result<Report> {
    let mut batcher = Batcher::new(rx, d.max_jobs);
    let mut records: Vec<Record> = Vec::new();
    let mut failed = false;
    let mut dispatcher: Option<Dispatcher> = None;
    let mut up: Option<StreamUpload> = None;
    let mut resolver: Option<crate::jobs::cache::Resolver> = None;
    let mut stop = false;
    let t0 = Instant::now();
    let mut first_build: Option<std::time::Duration> = None;
    let mut n_cached = 0usize;
    let mut n_other_system = 0usize;

    while let Some(batch) = batcher.next_batch() {
        let mut targets: Vec<Target> = Vec::new();
        let mut pending: Vec<Record> = Vec::new();

        // Cache status first: a job the builder or a substituter already has
        // needs no upload at all. The check reads input derivations from
        // /nix/store, which an in-process evaluation has not written, so a
        // walk it cannot finish just means "build it".
        let mut lines = batch;
        // The cache-status probe needs `QuerySubstitutablePathInfos`, which is
        // a worker-protocol op; a `grpc://` store answers neither it nor a
        // second connection cheaply, so the batch is simply built.
        if d.skip_cached && !matches!(d.transport, Transport::Grpc(_)) {
            if resolver.is_none() {
                resolver = Some(crate::jobs::cache::Resolver::open(&d.transport)?);
            }
            if let Some(r) = &mut resolver
                && let Err(e) = r.resolve(&mut lines)
            {
                crate::log::line(&format!(
                    "ietsp fast-build: --skip-cached: {e}; building this batch"
                ));
            }
        }

        for line in &lines {
            match line {
                Line::Error(e) => {
                    let skipped = d.skip_unsupported && e.error.contains(UNSUPPORTED_MARKER);
                    failed |= !skipped;
                    if !skipped {
                        stop |= d.fail_fast;
                    }
                    pending.push(Record {
                        success: skipped,
                        error: Some(e.error.clone()),
                        skipped,
                        ..Record::eval(&e.attr)
                    });
                }
                Line::Job(job) => {
                    let info = Info::of(job);
                    pending.push(Record {
                        outputs: (!info.outputs.is_empty())
                            .then(|| info.outputs.iter().cloned().collect::<BTreeMap<_, _>>()),
                        drv_path: Some(info.drv_path.to_string()),
                        cache_status: info.cache_status.clone(),
                        ..Record::eval(&info.attr)
                    });
                    // workers.py:73-84, in the same order. A job dropped here
                    // gets no BUILD record at all, as nix-fast-build does: the
                    // EVAL record already carries its cache status.
                    if info.cache_status.as_deref() == Some("cached") {
                        n_cached += 1;
                        continue;
                    }
                    if info.cache_status.as_deref() == Some("local") && d.out_link.is_none() {
                        n_cached += 1;
                        continue;
                    }
                    if let Some(sys) = &info.system
                        && !d.systems.contains(sys)
                    {
                        n_other_system += 1;
                        continue;
                    }
                    let mut t = Target::new(&info.attr, info.drv_path.clone(), Vec::new());
                    t.cache_status = info.cache_status.clone();
                    t.label = position(job).map(|p| format!("{} ({p})", info.attr));
                    targets.push(t);
                }
            }
        }

        let (dispatcher, up) = match (&mut dispatcher, &mut up) {
            (Some(d), Some(u)) => (d, u),
            _ => {
                // The first batch: the sources the flake copied are in the
                // table now, so the log rewriter can be built.
                let prov = crate::provenance::Provenance::from_map(
                    crate::provenance::SourceMap::from_sources(&d.sources.origins()),
                );
                let mut fresh = Dispatcher::open(&d.transport, &d.options, prov, d.style)?;
                fresh.retries = d.retries;
                if d.stream_json_lines {
                    fresh
                        .reporter
                        .stream_to(Box::new(std::io::BufWriter::new(std::io::stdout())));
                }
                dispatcher = Some(fresh);
                up = Some(StreamUpload::new(d.drvs.clone(), d.sources.clone()));
                (
                    dispatcher.as_mut().expect("just set"),
                    up.as_mut().expect("just set"),
                )
            }
        };
        for r in pending {
            dispatcher.record(r);
        }
        if stop {
            for t in &targets {
                dispatcher.record(Record {
                    skipped: true,
                    success: false,
                    error: Some("skipped: --fail-fast".into()),
                    drv_path: Some(t.drv.to_string()),
                    ..Record::build(&t.attr)
                });
            }
            continue;
        }
        if targets.is_empty() {
            continue;
        }
        let outcomes = dispatcher.submit(up, targets)?;
        if first_build.is_none() {
            first_build = Some(t0.elapsed());
        }
        for o in &outcomes {
            if let Some(prefix) = &d.out_link
                && o.success
            {
                for (name, path) in &o.outputs {
                    let link = match name.as_str() {
                        "out" => format!("{prefix}-{}", o.attr),
                        n => format!("{prefix}-{}-{n}", o.attr),
                    };
                    let abs = crate::cli::abs_path(&link, &crate::cli::cwd());
                    let abs = String::from_utf8_lossy(&abs).into_owned();
                    if let Err(e) = dispatcher.out_link(path, std::path::Path::new(&abs)) {
                        crate::log::Report::new(e.code(), format!("cannot create '{abs}': {e}"))
                            .emit();
                        failed = true;
                    }
                }
            }
            dispatcher.record(o.record());
        }
        failed |= dispatcher.exit != 0;
        stop |= d.fail_fast && dispatcher.exit != 0;
    }

    if let Some(mut dd) = dispatcher {
        records = std::mem::take(&mut dd.reporter.records);
        failed |= dd.exit != 0;
        let total = t0.elapsed();
        crate::log::line(&format!(
            "fast-build: {} built, {n_cached} cached, {n_other_system} for another system, {} build ops, {} paths uploaded ({} MB), first build at {:.3}s, total {:.3}s",
            records
                .iter()
                .filter(|r| r.kind == results::Kind::Build)
                .count(),
            dd.ops,
            dd.upload.added,
            dd.upload.bytes >> 20,
            first_build.unwrap_or_default().as_secs_f64(),
            total.as_secs_f64()
        ));
    }
    Ok(Report { records, failed })
}

/// The `pos` key `--provenance` adds to a job: where its derivation is
/// defined, already mapped to a local file.
fn position(job: &crate::jobs::line::Job) -> Option<String> {
    serde_json::from_slice::<String>(job.extra.get("pos")?).ok()
}

fn write_result_file(
    path: &PathBuf,
    format: Format,
    suite: &str,
    records: &[Record],
) -> Result<()> {
    let f = std::fs::File::create(path)
        .map_err(|e| CrateError::io(format!("cannot create '{}'", path.display()), e))?;
    let mut w = std::io::BufWriter::new(f);
    let r = match format {
        Format::Json => results::dump_json(&mut w, records),
        Format::Junit => results::dump_junit(&mut w, suite, records),
    };
    r.map_err(|e| CrateError::io(format!("cannot write '{}'", path.display()), e))
}

fn write_step_summary(path: &std::path::Path, records: &[Record], code: i32) {
    let open = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(path);
    match open {
        Ok(f) => {
            let mut w = std::io::BufWriter::new(f);
            if let Err(e) = results::step_summary(&mut w, records, code) {
                crate::log::line(&format!(
                    "ietsp fast-build: cannot write '{}': {e}",
                    path.display()
                ));
            }
        }
        Err(e) => crate::log::line(&format!(
            "ietsp fast-build: cannot write '{}': {e}",
            path.display()
        )),
    }
}

fn summary(r: &Report) {
    let builds: Vec<&Record> = r
        .records
        .iter()
        .filter(|r| r.kind == results::Kind::Build)
        .collect();
    let bad: Vec<&&Record> = builds.iter().filter(|b| !b.success && !b.skipped).collect();
    if bad.is_empty() {
        crate::log::line(&format!("fast-build: {} succeeded, 0 failed", builds.len()));
        return;
    }
    crate::log::line(&format!(
        "fast-build: {} succeeded, {} failed: {}",
        builds.iter().filter(|b| b.success).count(),
        bad.len(),
        bad.iter()
            .map(|b| b.attr.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cap(url: &str, j: Option<u64>) -> (usize, u64) {
        let mut t = Transport::parse(url).unwrap();
        let mut o = Options {
            max_jobs: 2,
            ..Options::default()
        };
        let n = jobs(j, &mut t, &mut o);
        if let Transport::Grpc(g) = &t {
            assert_eq!(g.max_builds, n);
        }
        (n, o.max_jobs)
    }

    #[test]
    fn grpc_ignores_nix_conf_max_jobs() {
        assert_eq!(cap("grpc://farm:50051", None), (64, 2));
        assert_eq!(cap("grpc://farm:50051?max-builds=8", Some(0)), (8, 2));
    }

    #[test]
    fn explicit_jobs_lower_max_builds() {
        assert_eq!(cap("grpc://farm:50051", Some(16)), (16, 16));
        assert_eq!(cap("grpc://farm:50051?max-builds=8", Some(16)), (8, 16));
        assert_eq!(cap("daemon", None), (2, 2));
        assert_eq!(cap("daemon", Some(5)), (5, 5));
    }
}

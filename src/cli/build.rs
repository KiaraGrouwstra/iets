//! `ietsp build`: evaluate to derivations and build them through a daemon.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use crate::build::fixhash::{self, Answer};
use crate::cli::help::{arg_error, value};
use crate::cli::{EvalArgs, Input, Run, build_targets, evaluate, parse_eval_args, report, run_for};
use crate::error::Error as CrateError;
use crate::eval::Evaluator;
use crate::store::locate::{drv_roots, locate_in_store};
use crate::trace::run::Cache;

struct BuildArgs {
    url: Option<String>,
    timings: bool,
    no_build: bool,
    dry_run: bool,
    json: bool,
    fetch_to: Option<std::path::PathBuf>,
    outputs: Vec<String>,
    sources: HashMap<String, std::path::PathBuf>,
}

/// `ietsp build`: evaluate to derivations and ship their closure from memory, or (legacy)
/// take a `.drv` path and read its closure from `/nix/store`. A fixed-output
/// build that fails on its hash gets it fixed per the `fix-hashes` setting.
pub fn cmd_build(argv: &[String]) -> i32 {
    use crate::settings::FixHashes;
    let mut ask = match crate::session::Session::current().settings().fix_hashes {
        FixHashes::Never => return build_once(argv),
        FixHashes::Ask if !fixhash::interactive() => return build_once(argv),
        FixHashes::Ask => true,
        FixHashes::Always => false,
    };
    let mut wrote: Vec<String> = Vec::new();
    loop {
        let code = build_once(argv);
        let mismatches = crate::log::take_mismatches();
        if code == 0 || mismatches.is_empty() {
            return code;
        }
        let files = local_files(argv);
        let mut fixed = false;
        for (specified, got) in mismatches {
            if wrote.contains(&specified) {
                crate::log::line(&format!(
                    "warning: fix-hashes: the hash changed again, from '{specified}' to '{got}': the output is not reproducible"
                ));
                return code;
            }
            let fix = match fixhash::locate(&files, &specified, &got) {
                Ok(f) => f,
                Err(why) => {
                    crate::log::line(&format!("warning: fix-hashes: {why}"));
                    continue;
                }
            };
            if ask {
                match fixhash::prompt(&fix) {
                    Answer::No => continue,
                    Answer::All => ask = false,
                    Answer::Yes => {}
                }
            }
            if let Err(e) = fix.write() {
                crate::log::line(&format!("warning: fix-hashes: {e}"));
                continue;
            }
            crate::log::fixed(&fix.pos(), &fix.old, &fix.new);
            wrote.push(got);
            fixed = true;
        }
        if !fixed {
            return code;
        }
    }
}

fn parse(argv: &[String]) -> (BuildArgs, EvalArgs) {
    let mut b = BuildArgs {
        url: None,
        timings: false,
        no_build: false,
        dry_run: false,
        json: false,
        fetch_to: None,
        outputs: Vec::new(),
        sources: HashMap::new(),
    };
    let mut rest: Vec<String> = Vec::new();
    let mut i = 0;
    while i < argv.len() {
        let s = argv[i].as_str();
        let mut next = || {
            i += 1;
            value(argv, i, s).clone()
        };
        match s {
            "--builder" => b.url = Some(next()),
            "--timings" => b.timings = true,
            "--no-build" => b.no_build = true,
            "--dry-run" => b.dry_run = true,
            "--json" => b.json = true,
            "--fetch-to" => b.fetch_to = Some(next().into()),
            "--out" => b.outputs.push(next()),
            "--source" => {
                let spec = next();
                let Some((store, local)) = spec.split_once('=') else {
                    arg_error(format!("--source wants STOREPATH=LOCAL, got '{spec}'"))
                };
                b.sources.insert(store.to_string(), local.into());
            }
            s => rest.push(s.to_string()),
        }
        i += 1;
    }
    let a = parse_eval_args(&rest);
    (b, a)
}

/// The local files the evaluation of `argv` parses, from a fresh
/// evaluation: a cache hit does not know them.
fn local_files(argv: &[String]) -> Vec<PathBuf> {
    let (_, a) = parse(argv);
    let a = EvalArgs {
        no_cache: true,
        ..a
    };
    let Ok(run) = Run::new(&a) else {
        return Vec::new();
    };
    crate::cli::run_parallel(run, move |ev, _| {
        let mut roots = Vec::new();
        let _ = evaluate(ev, a).and_then(|v| drv_roots(ev, v, &mut roots));
        fixhash::local_files(ev)
    })
}

fn build_once(argv: &[String]) -> i32 {
    let (b, a) = parse(argv);
    if let Some(Input::File(f)) = &a.input
        && f.ends_with(".drv")
    {
        return build_drv_file(&b, f);
    }

    let t0 = std::time::Instant::now();
    let a = EvalArgs {
        timings: a.timings || b.timings,
        ..a
    };
    let run = run_for(&a).cached(&a, "build");
    if let Some(code) = run.hit(|c| {
        let (roots, drvs, sources) = c.read_build()?;
        c.replay_warnings();
        if b.timings {
            eprintln!(
                "eval: skipped, {} derivations and {} sources from the cache, {:.3}s",
                drvs.len(),
                sources.len(),
                t0.elapsed().as_secs_f64()
            );
        }
        let (root_paths, outputs) = build_targets(&b.outputs, &roots);
        let prov = crate::provenance::Provenance::from_map(
            crate::provenance::SourceMap::from_sources(&sources),
        );
        let inputs = crate::remote::plan::Inputs {
            drvs: &drvs,
            sources: &sources,
        };
        Some(build_roots(&b, &root_paths, &outputs, inputs, &prov))
    }) {
        return code;
    }
    // parallel: eval and build run as the main fiber
    crate::cli::run_parallel(run, move |ev, cache| build_eval(ev, &b, a, t0, cache))
}

/// The Nix settings, for `system` and the `store` that `--builder` defaults to;
/// `ietsp build` takes no `--option`. An unreadable nix.conf leaves the
/// defaults, as it did when nothing here read one.
fn config() -> crate::config::Config {
    crate::config::Config::load(&[]).unwrap_or_else(|_| crate::config::Config::empty())
}

fn build_eval(
    ev: &mut Evaluator,
    b: &BuildArgs,
    a: EvalArgs,
    t0: std::time::Instant,
    cache: Option<Cache>,
) -> i32 {
    let mut roots = Vec::new();
    let target = a.attr.clone().or_else(|| match &a.input {
        Some(Input::Flake(f)) => f.split_once('#').map(|(_, frag)| frag.to_string()),
        _ => None,
    });
    let r = evaluate(ev, a).and_then(|v| drv_roots(ev, v, &mut roots));
    if let Err(e) = r {
        return report(ev, &e);
    }
    if let Some(t) = &target {
        for (p, _) in &roots {
            ev.drv.set_attr(p, t);
        }
    }
    let eval_time = t0.elapsed();
    if b.timings {
        eprintln!(
            "eval: {} derivations, {} sources, {:.3}s",
            ev.drv.nr_derivations,
            ev.known.origins().len(),
            eval_time.as_secs_f64()
        );
    }
    if let Some(c) = &cache {
        c.write(ev, &crate::trace::wire::encode_roots(&roots), true);
    }
    let (root_paths, outputs) = build_targets(&b.outputs, &roots);
    let prov = crate::provenance::Provenance::snapshot(ev);
    let drvs = ev.drv.drvs.lock();
    let sources = ev.known.origins();
    let inputs = crate::remote::plan::Inputs {
        drvs: &drvs,
        sources: &sources,
    };
    build_roots(b, &root_paths, &outputs, inputs, &prov)
}

/// Legacy mode: `DRV` is a `.drv` store path, or a copy of the file named
/// `<hash>-<name>.drv`; its closure is read from `/nix/store`.
fn build_drv_file(b: &BuildArgs, root_arg: &str) -> i32 {
    use crate::builtins::derivation::DrvInfo;
    use crate::store::drv::Derivation;
    use crate::store::path::StorePath;
    let base = std::path::Path::new(root_arg)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let root = match StorePath::from_base_name(&base) {
        Ok(p) if p.is_derivation() => p,
        Ok(p) => {
            return CrateError::Store(format!("'{p}' is not a derivation")).report();
        }
        Err(e) => return CrateError::from(e).report(),
    };
    // The root may be a copy outside the store; its inputs are read from `/nix/store`.
    let info = std::fs::read_to_string(root_arg)
        .map_err(CrateError::from)
        .and_then(|text| {
            let name = Derivation::name_from_path(&root)?;
            let drv = Derivation::parse(&text, name)?;
            let mut references = drv.input_srcs;
            references.extend(drv.input_drvs.into_keys());
            Ok(DrvInfo {
                outputs: drv.outputs.into_keys().collect(),
                references,
                aterm: Some(Arc::new(text.into_bytes())),
                pos: None,
                attr: None,
            })
        });
    let info = match info {
        Ok(i) => i,
        Err(e) => {
            let msg = format!("cannot read derivation '{root_arg}': {e}");
            return crate::log::Report::new(e.code(), msg).emit();
        }
    };
    let drvs = HashMap::from([(root.clone(), info)]);
    let sources = HashMap::new();
    let prov = crate::provenance::Provenance::from_map(crate::provenance::SourceMap::from_sources(
        &sources,
    ));
    build_roots(
        b,
        &[root],
        &b.outputs,
        crate::remote::plan::Inputs {
            drvs: &drvs,
            sources: &sources,
        },
        &prov,
    )
}

fn build_roots(
    b: &BuildArgs,
    roots: &[crate::store::path::StorePath],
    outputs: &[String],
    inputs: crate::remote::plan::Inputs<'_>,
    prov: &crate::provenance::Provenance,
) -> i32 {
    use crate::build::render::Style;
    use crate::build::upload::PlanUpload;
    use crate::build::{Dispatcher, Target};
    use crate::store::path::StorePath;

    let cfg = config();
    let url = crate::settings::builder(b.url.as_deref(), &cfg);
    let transport = match crate::remote::worker::Transport::parse_checked(&url, &cfg.system()) {
        Ok(Some(t)) => t,
        Ok(None) => {
            return CrateError::Usage(format!(
                "unsupported builder '{url}': expected daemon, unix:///path, ssh-ng://[user@]host or grpc://host:port"
            ))
            .report();
        }
        Err(e) => return CrateError::Usage(e.to_string()).report(),
    };
    let t0 = std::time::Instant::now();
    let plan = match crate::remote::plan::plan(roots, inputs) {
        Ok(p) => p,
        Err(e) => return e.report(),
    };
    let plan_time = t0.elapsed();
    if b.timings && plan.store_drvs > 0 {
        eprintln!(
            "plan: {} drvs not from this eval, read from /nix/store",
            plan.store_drvs
        );
    }
    let locate = |paths: &[StorePath]| locate_in_store(paths, &b.sources);

    let mut up = PlanUpload::new(&plan, &locate);
    let mut d = match Dispatcher::open(
        &transport,
        &crate::remote::worker::Options::default(),
        prov.clone(),
        Style::detect(),
    ) {
        Ok(d) => d,
        Err(e) => return e.report(),
    };

    if b.no_build {
        if let Err(e) = d.upload_only(&mut up, &roots[..1]) {
            return e.report();
        }
        timings(b, &d, &plan, plan_time);
        return 0;
    }

    let targets: Vec<Target> = roots
        .iter()
        .map(|r| {
            // The bare attribute: the dispatcher adds the position, once.
            let name = prov
                .attr_of(&r.to_string())
                .unwrap_or_else(|| crate::provenance::drv_name(&r.to_string()));
            Target::new(&name, r.clone(), outputs.to_vec())
        })
        .collect();
    if b.dry_run {
        return match d.missing(&mut up, &targets) {
            Ok(m) => print_missing(&m, &plan, b.json),
            Err(e) => e.report(),
        };
    }
    let outcomes = match d.submit(&mut up, targets) {
        Ok(o) => o,
        Err(e) => return e.report(),
    };
    if let Some(dir) = &b.fetch_to {
        for o in &outcomes {
            for path in o.outputs.values() {
                if let Err(e) = d.fetch(path, dir) {
                    return e.report();
                }
            }
        }
    }
    timings(b, &d, &plan, plan_time);
    for o in &outcomes {
        for path in o.outputs.values() {
            println!("{path}");
        }
    }
    d.exit
}

/// shared.cc:printMissing at lvlError, as `nix build --dry-run` prints it;
/// with `--json` the same sets on stdout. `willBuild` goes in the plan's
/// order, references first, as Nix's reversed topoSortPaths does.
fn print_missing(
    m: &crate::remote::worker::Missing,
    plan: &crate::remote::plan::Plan,
    json: bool,
) -> i32 {
    let at: HashMap<String, usize> = (plan.drvs.iter().enumerate())
        .map(|(i, o)| (o.store_path().to_string(), i))
        .collect();
    let mut build = m.will_build.clone();
    build.sort_by_key(|p| at.get(p).copied().unwrap_or(usize::MAX));
    let mut fetch = m.will_substitute.clone();
    fetch.sort_by_key(|p| (p.get(44..).unwrap_or(p).to_string(), p.clone()));
    let sizes = format!(
        "{} download, {} unpacked",
        render_size(m.download_size),
        render_size(m.nar_size)
    );
    let unknown = "don't know how to build these paths:".to_string();
    let sections = [
        (
            &build,
            "this derivation will be built:".into(),
            format!("these {} derivations will be built:", build.len()),
        ),
        (
            &fetch,
            format!("this path will be fetched ({sizes}):"),
            format!("these {} paths will be fetched ({sizes}):", fetch.len()),
        ),
        (&m.unknown, unknown.clone(), unknown),
    ];
    for (paths, one, many) in sections {
        if !paths.is_empty() {
            crate::log::msg(0, if paths.len() == 1 { &one } else { &many });
            paths
                .iter()
                .for_each(|p| crate::log::msg(0, &format!("  {p}")));
        }
    }
    if json {
        let v = serde_json::json!({
            "willBuild": build,
            "willSubstitute": fetch,
            "unknown": m.unknown,
            "downloadSize": m.download_size,
            "narSize": m.nar_size,
        });
        println!("{v}");
    }
    0
}

/// util.cc:renderSize: KiB at least, one decimal.
fn render_size(bytes: u64) -> String {
    let (mut unit, mut v) = (0, bytes);
    while v > 1024 && unit < 8 {
        unit += 1;
        v /= 1024;
    }
    let scaled = bytes as f64 / 1024f64.powi(unit.max(1));
    format!("{scaled:.1} {}iB", b"KKMGTPEZY"[unit as usize] as char)
}

/// `--timings`: one line per phase of the flow, as the backend measured it.
fn timings(
    b: &BuildArgs,
    d: &crate::build::Dispatcher,
    plan: &crate::remote::plan::Plan,
    plan_time: std::time::Duration,
) {
    if b.timings {
        eprintln!("{}", d.report(plan, plan_time));
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn sizes_render_as_nix_renders_them() {
        let got: Vec<String> = [100, 2000, 10547, 1_572_864, 6_000_000_000]
            .map(super::render_size)
            .to_vec();
        assert_eq!(
            got,
            ["0.1 KiB", "2.0 KiB", "10.3 KiB", "1.5 MiB", "5.6 GiB"]
        );
    }
}

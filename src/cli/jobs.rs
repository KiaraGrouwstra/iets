//! `ietsp eval-jobs`: flags, the three process roles, and the summary.

use std::process::exit;

use crate::cli::{
    EvalArgs, Input, abs_path, auto_args, cwd, evaluate, parse_eval_args, report, usage,
};
use crate::error::Error as CrateError;
use crate::eval::{Error, Evaluator, Pos};
use crate::runtime::Word;

/// `ietsp eval-jobs`'s own flags; everything else goes to `parse_eval_args`.
struct JobsArgs {
    opts: crate::jobs::Opts,
    check_cache_status: bool,
    gc_roots_dir: Option<std::path::PathBuf>,
    eval_store: String,
    apply: Option<String>,
    select: Option<String>,
    /// `--max-memory-size MB`: worker processes restarted above it.
    max_memory: Option<u64>,
    worker: bool,
    flake: bool,
    quiet: bool,
}

fn parse_jobs_args(argv: &[String]) -> (JobsArgs, Vec<String>) {
    let mut j = JobsArgs {
        opts: crate::jobs::Opts {
            force_recurse: false,
            meta: false,
            show_input_drvs: false,
            no_instantiate: false,
            constituents: false,
            chunks: 1,
            chunk: 0,
            chunk_depth: 2,
        },
        check_cache_status: false,
        gc_roots_dir: None,
        eval_store: "daemon".into(),
        apply: None,
        select: None,
        max_memory: None,
        worker: false,
        flake: false,
        quiet: false,
    };
    let mut rest: Vec<String> = Vec::new();
    let mut i = 0;
    let arg = |i: &mut usize| -> String {
        *i += 1;
        argv.get(*i).cloned().unwrap_or_else(|| usage())
    };
    let num = |i: &mut usize| -> u64 { arg(i).parse().unwrap_or_else(|_| usage()) };
    let unsupported = |flag: &str, why: &str| -> ! {
        eprintln!("error: {flag} is not supported by ietsp: {why} (docs/eval-jobs.md)");
        exit(1)
    };
    while i < argv.len() {
        let s = argv[i].as_str();
        match s {
            "--force-recurse" => j.opts.force_recurse = true,
            "--meta" => j.opts.meta = true,
            "--show-input-drvs" => j.opts.show_input_drvs = true,
            "--no-instantiate" => j.opts.no_instantiate = true,
            "--constituents" => j.opts.constituents = true,
            "--check-cache-status" => j.check_cache_status = true,
            "--chunks" => j.opts.chunks = (num(&mut i) as u32).max(1),
            "--chunk" => j.opts.chunk = num(&mut i) as u32,
            "--chunk-depth" => j.opts.chunk_depth = num(&mut i) as u32,
            "--max-memory-size" => j.max_memory = Some(num(&mut i)),
            "--apply" => j.apply = Some(arg(&mut i)),
            "--select" => j.select = Some(arg(&mut i)),
            "--gc-roots-dir" => {
                let d = arg(&mut i);
                let cwd = cwd();
                j.gc_roots_dir = Some(std::path::PathBuf::from(
                    <std::ffi::OsStr as std::os::unix::ffi::OsStrExt>::from_bytes(&abs_path(
                        &d, &cwd,
                    )),
                ));
            }
            "--eval-store" => j.eval_store = arg(&mut i),
            "--max-jobs" | "-j" => {
                let _ = arg(&mut i);
            }
            "--log-format" => {
                let f = arg(&mut i);
                if f != "raw" {
                    unsupported(
                        &format!("--log-format {f}"),
                        "ietsp logs plain lines to stderr; only 'raw' is accepted",
                    );
                }
            }
            "--flake" => j.flake = true,
            "--worker" => j.worker = true,
            "--quiet" => j.quiet = true,
            "--show-trace" | "--verbose" | "--debug" => {}
            "--override-input" | "--override-flake" => unsupported(
                s,
                "ietsp evaluates locked flakes only; an override makes the lock file incomplete",
            ),
            "--reference-lock-file" => {
                unsupported(s, "ietsp reads the flake's own flake.lock only")
            }
            "--repair" => unsupported(s, "ietsp does not write or repair store paths"),
            "--arg-from-file" => {
                let name = arg(&mut i);
                let path = arg(&mut i);
                let contents = std::fs::read_to_string(&path).unwrap_or_else(|e| {
                    eprintln!("error: cannot read '{path}': {e}");
                    exit(1)
                });
                rest.extend(["--argstr".to_string(), name, contents]);
            }
            "--arg-from-stdin" => {
                let name = arg(&mut i);
                let contents = std::io::read_to_string(std::io::stdin()).unwrap_or_else(|e| {
                    eprintln!("error: cannot read stdin: {e}");
                    exit(1)
                });
                rest.extend(["--argstr".to_string(), name, contents]);
            }
            "--include" => {
                rest.push("-I".to_string());
                rest.push(arg(&mut i));
            }
            _ => rest.push(s.to_string()),
        }
        i += 1;
    }
    (j, rest)
}

/// nix-eval-jobs.cc:validateIncompatibleFlags.
fn check_jobs_args(j: &JobsArgs) -> crate::error::Result<crate::remote::worker::Transport> {
    if j.opts.chunk >= j.opts.chunks {
        return Err(CrateError::Usage("--chunk must be below --chunks".into()));
    }
    if j.opts.no_instantiate {
        let mut bad = Vec::new();
        if j.opts.show_input_drvs {
            bad.push("--show-input-drvs");
        }
        if j.check_cache_status {
            bad.push("--check-cache-status");
        }
        if j.opts.constituents {
            bad.push("--constituents");
        }
        if !bad.is_empty() {
            return Err(CrateError::Usage(format!(
                "--no-instantiate is incompatible with: {}. These features require instantiated derivations.",
                bad.join(", ")
            )));
        }
    }
    crate::remote::worker::Transport::parse(&j.eval_store).ok_or_else(|| {
        CrateError::Usage(format!(
            "--eval-store '{}': eval-jobs needs a Nix daemon: daemon, unix:///path or ssh-ng://host",
            j.eval_store
        ))
    })
}

/// `ietsp eval-jobs`: nix-eval-jobs's JSON lines. One process by default;
/// with `--max-memory-size`, worker processes restarted above the limit.
/// See docs/eval-jobs.md.
pub fn cmd_eval_jobs(argv: &[String]) -> i32 {
    let (j, rest) = parse_jobs_args(argv);
    let mut a = parse_eval_args(&rest);
    a.no_cache = true;
    a.flake_whole = true;
    // nix-eval-jobs takes the argument as a file unless --flake says otherwise.
    a.input = match a.input.take() {
        Some(Input::Flake(s)) | Some(Input::File(s)) => Some(if j.flake {
            Input::Flake(s)
        } else {
            Input::File(s)
        }),
        other => other,
    };
    if a.input.is_none() {
        eprintln!("error: no expression specified");
        return 1;
    }
    let transport = match check_jobs_args(&j) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    let t0 = std::time::Instant::now();
    if j.worker {
        let mut ev = Evaluator::new();
        ev.drv.keep_aterm = !j.opts.no_instantiate;
        return crate::parallel::run(1, ev, move |ev| jobs_worker(ev, a, &j, &transport));
    }
    if j.max_memory.is_some() {
        return jobs_collector(argv, a.workers, &j, &transport, t0);
    }
    let workers = a.workers;
    let mut ev = Evaluator::new();
    ev.drv.keep_aterm = !j.opts.no_instantiate;
    crate::parallel::run(workers, ev, move |ev| {
        jobs_single(ev, a, &j, &transport, t0)
    })
}

/// worker.cc:initializeRootValue: the root, auto-called with the
/// `--arg`s, then `--select`ed; `jobs::configure` done on the way.
fn jobs_root(ev: &mut Evaluator, a: EvalArgs, j: &JobsArgs) -> Result<Word, Box<Error>> {
    let cwd = cwd();
    let args = auto_args(ev, &a.args, &cwd)?;
    let apply = match &j.apply {
        Some(e) => Some(ev.eval_string(e, &cwd)?),
        None => None,
    };
    crate::jobs::configure(ev, j.opts, args, apply);
    let root = evaluate(
        ev,
        EvalArgs {
            args: Vec::new(),
            ..a
        },
    )?;
    let root = ev.auto_call(root, args)?;
    let Some(select) = &j.select else {
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
}

fn jobs_summary(
    j: &JobsArgs,
    stats: crate::jobs::Stats,
    inst: Option<crate::jobs::instantiate::Stats>,
    t0: std::time::Instant,
) {
    if j.quiet {
        return;
    }
    let mut s = format!(
        "eval-jobs: {} jobs, {} errors, {:.3}s, {}",
        stats.jobs,
        stats.errors,
        t0.elapsed().as_secs_f64(),
        if j.max_memory.is_some() {
            format!(
                "{} worker processes spawned",
                crate::jobs::collector::SPAWNED.load(std::sync::atomic::Ordering::Relaxed)
            )
        } else {
            format!("peak RSS {} MB", crate::jobs::collector::peak_rss_mb())
        }
    );
    if let Some(i) = inst {
        s.push_str(&format!(
            "\ninstantiate: {} batches, {} paths queried, {} added ({} MB), {} roots, {:.3}s",
            i.batches,
            i.queried,
            i.added,
            i.bytes >> 20,
            i.roots,
            i.time.as_secs_f64()
        ));
    }
    eprintln!("{s}");
}

/// One process: the walk on the main fiber, instantiation and cache
/// status on a thread beside it, constituents at the end.
fn jobs_single(
    ev: &mut Evaluator,
    a: EvalArgs,
    j: &JobsArgs,
    transport: &crate::remote::worker::Transport,
    t0: std::time::Instant,
) -> i32 {
    use crate::jobs::cache::Resolver;
    use crate::jobs::emit::Emitter;
    use crate::jobs::instantiate::{Instantiator, Store};
    use std::sync::{Arc, Mutex};

    let root = match jobs_root(ev, a, j) {
        Ok(r) => r,
        Err(e) => {
            report(ev, &e);
            return 1;
        }
    };
    let stdout = Box::new(std::io::BufWriter::new(std::io::stdout()));
    let emitter = Arc::new(Mutex::new(Emitter::new(stdout, j.opts.constituents)));
    let mut inst_stats = None;
    if j.opts.no_instantiate {
        let em = emitter.clone();
        crate::jobs::run(ev, root, &mut |line| em.lock().unwrap().emit(line));
        if std::env::var_os("IETSP_STATS").is_some() {
            ev.print_stats();
        }
    } else {
        let setup = Store::open(
            transport,
            j.gc_roots_dir.clone(),
            ev.drv.drvs.clone(),
            ev.sources.clone(),
        )
        .and_then(|store| {
            let resolver = if j.check_cache_status {
                Some(Resolver::open(transport)?)
            } else {
                None
            };
            Ok((store, resolver))
        });
        let (store, mut resolver) = match setup {
            Ok(s) => s,
            Err(e) => {
                eprintln!("error: {e}");
                return 1;
            }
        };
        let em = emitter.clone();
        let inst = Instantiator::start(
            store,
            Box::new(move |mut lines| {
                if let Some(r) = &mut resolver {
                    r.resolve(&mut lines)?;
                }
                let mut e = em.lock().unwrap();
                for l in lines {
                    e.emit(l);
                }
                Ok(())
            }),
        );
        crate::jobs::run(ev, root, &mut |line| inst.push(line));
        let (store, stats) = match inst.finish() {
            Ok(r) => r,
            Err(e) => {
                eprintln!("error: {e}");
                return 1;
            }
        };
        inst_stats = Some(stats);
        let mut conn = store.into_connection();
        if let Err(e) = emitter
            .lock()
            .unwrap()
            .finish(&mut conn, j.gc_roots_dir.as_deref())
        {
            eprintln!("error: {e}");
            return 1;
        }
    }
    let em = emitter.lock().unwrap();
    jobs_summary(j, em.stats, inst_stats, t0);
    if em.fatal { 1 } else { 0 }
}

/// A worker process under `--max-memory-size`: the root once, then
/// `do` requests from the collector until `exit` or the limit.
fn jobs_worker(
    ev: &mut Evaluator,
    a: EvalArgs,
    j: &JobsArgs,
    transport: &crate::remote::worker::Transport,
) -> i32 {
    use crate::jobs::collector::{Out, run_worker, write_out};
    use crate::jobs::instantiate::{Instantiator, Store};
    use std::sync::{Arc, Mutex};

    let out: Out = Arc::new(Mutex::new(Box::new(std::io::stdout())));
    let fail = |out: &Out, msg: &str| -> i32 {
        eprintln!("{msg}");
        write_out(
            out,
            &format!(
                "{{\"error\":{}}}",
                String::from_utf8_lossy(&crate::jobs::line::json_str(msg))
            )
            .into_bytes(),
        );
        1
    };
    let root = match jobs_root(ev, a, j) {
        Ok(r) => r,
        Err(e) => return fail(&out, &crate::jobs::error_text(ev, &e)),
    };
    let inst = if j.opts.no_instantiate {
        None
    } else {
        match Store::open(
            transport,
            j.gc_roots_dir.clone(),
            ev.drv.drvs.clone(),
            ev.sources.clone(),
        ) {
            Ok(store) => {
                let o = out.clone();
                Some(Instantiator::start(
                    store,
                    Box::new(move |lines| {
                        for l in lines {
                            write_out(&o, &l.render());
                        }
                        Ok(())
                    }),
                ))
            }
            Err(e) => return fail(&out, &format!("error: {e}")),
        }
    };
    match run_worker(ev, root, inst, j.max_memory.unwrap_or(4096), &out) {
        Ok(()) => 0,
        Err(e) => fail(&out, &format!("error: {e}")),
    }
}

/// The main process under `--max-memory-size`: collector threads drive
/// the worker processes; cache status and constituents run here.
fn jobs_collector(
    argv: &[String],
    workers: usize,
    j: &JobsArgs,
    transport: &crate::remote::worker::Transport,
    t0: std::time::Instant,
) -> i32 {
    use crate::jobs::Line;
    use crate::jobs::cache::Resolver;
    use crate::jobs::collector::{Sink, run_collector};
    use crate::jobs::emit::Emitter;
    use crate::remote::worker::Connection;
    use std::sync::mpsc::channel;
    use std::sync::{Arc, Mutex};

    let stdout = Box::new(std::io::BufWriter::new(std::io::stdout()));
    let emitter = Arc::new(Mutex::new(Emitter::new(stdout, j.opts.constituents)));
    let (tx, rx) = channel::<Line>();
    let resolver_thread = if j.check_cache_status {
        let mut resolver = match Resolver::open(transport) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("error: {e}");
                return 1;
            }
        };
        let em = emitter.clone();
        Some(
            std::thread::Builder::new()
                .name("cache-status".into())
                .stack_size(64 << 20)
                .spawn(move || -> crate::error::Result<()> {
                    while let Ok(first) = rx.recv() {
                        let mut batch = vec![first];
                        while let Ok(l) = rx.try_recv() {
                            batch.push(l);
                        }
                        resolver.resolve(&mut batch)?;
                        let mut e = em.lock().unwrap();
                        for l in batch {
                            e.emit(l);
                        }
                    }
                    Ok(())
                })
                .expect("spawn cache-status thread"),
        )
    } else {
        drop(rx);
        None
    };
    let sink: Sink = {
        let em = emitter.clone();
        let tx = resolver_thread.is_some().then_some(tx);
        Arc::new(Mutex::new(Box::new(move |line: Line| match &tx {
            Some(tx) => {
                let _ = tx.send(line);
            }
            None => em.lock().unwrap().emit(line),
        })))
    };
    let r = run_collector(argv, workers, sink.clone());
    drop(sink);
    let r = r.and_then(|_| match resolver_thread {
        Some(t) => t
            .join()
            .map_err(|_| CrateError::Protocol("cache-status thread panicked".into()))?,
        None => Ok(()),
    });
    if let Err(e) = r {
        eprintln!("error: {e}");
        return 1;
    }
    if j.opts.constituents {
        let done = Connection::open(transport, &mut |_| {}).and_then(|mut conn| {
            emitter
                .lock()
                .unwrap()
                .finish(&mut conn, j.gc_roots_dir.as_deref())
        });
        if let Err(e) = done {
            eprintln!("error: {e}");
            return 1;
        }
    }
    let em = emitter.lock().unwrap();
    jobs_summary(j, em.stats, None, t0);
    if em.fatal { 1 } else { 0 }
}

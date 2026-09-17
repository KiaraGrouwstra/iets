use std::collections::{HashMap, HashSet};
use std::process::exit;

use ietsp::eval::{Error, Evaluator, Pos};
use ietsp::runtime::Word;
use ietsp::syntax::ast::FileId;
use ietsp::syntax::symbol::Interner;
use ietsp::syntax::{dump, line_col, parse};

const STACK_SIZE: usize = 4 << 30;

fn usage() -> ! {
    eprintln!(
        "usage: ietsp eval [--strict] [--json] [--raw] [--workers N] [--timings] [--no-cache] [--cache-dir DIR] [-I prefix=path]... [--arg name expr] [--argstr name str] [-A attr] [--dump-drvs DIR] (-E EXPR | FILE)\n       ietsp parse [-q] FILE...   (FILE may be - to read a list from stdin)\n       ietsp cache-dump READSET   (print a cache entry's read set)\n       ietsp amber-keys [--lstat] PATH   (every amber object key of a tree)\n       ietsp build [--builder URL] [--timings] [--no-build] [--fetch-to DIR] [--no-cache] [--cache-dir DIR] [--out NAME]... [--workers N] [-I prefix=path]... [--arg name expr] [--argstr name str] [-A attr] (-E EXPR | FILE)\n       ietsp build [--builder URL] [--timings] [--fetch-to DIR] [--out NAME]... [--source STOREPATH=LOCAL]... DRV\n       ietsp eval-jobs [--workers N] [--force-recurse] [--meta] [--show-input-drvs] [--chunks M --chunk N [--chunk-depth D]] [--max-memory-size MB] [-I prefix=path]... [--arg name expr] [--argstr name str] (-E EXPR | FILE | FLAKEREF#attr)\n       ietsp print-dev-env [--builder URL] [--gc-root PATH] [--json] [--workers N] [--no-cache] [--cache-dir DIR] [--timings] [FLAKEREF[#attr]]   (default: ., a daemon URL)\n       URL: http://host:port (gRPC), daemon, unix:///path, ssh-ng://[user@]host (Nix worker protocol)"
    );
    exit(2)
}

/// With `-q`, parse every file, report errors, and print totals. Used to
/// check and time the parser on whole trees such as nixpkgs.
fn cmd_parse(files: &[String], quiet: bool) -> i32 {
    let mut interner = Interner::new();
    let mut errors = 0usize;
    let mut bytes = 0usize;
    let start = std::time::Instant::now();
    for (i, file) in files.iter().enumerate() {
        let src = match std::fs::read(file) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("error: cannot read {file}: {e}");
                errors += 1;
                continue;
            }
        };
        bytes += src.len();
        match parse(&src, FileId(i as u32), &mut interner) {
            Ok(ast) => {
                if !quiet {
                    println!("{}", dump::dump(&ast, &interner));
                }
            }
            Err(e) => {
                let (line, col) = line_col(&src, e.span.start);
                eprintln!("error: {}\n       at {file}:{line}:{col}", e.msg);
                errors += 1;
            }
        }
    }
    if quiet {
        let dt = start.elapsed();
        eprintln!(
            "{} files, {} bytes, {} errors, {:.3}s",
            files.len(),
            bytes,
            errors,
            dt.as_secs_f64()
        );
    }
    if errors == 0 { 0 } else { 1 }
}

struct EvalArgs {
    strict: bool,
    json: bool,
    raw: bool,
    includes: Vec<(Vec<u8>, Vec<u8>)>,
    args: Vec<(String, ArgKind)>,
    input: Option<Input>,
    dump_drvs: Option<String>,
    attr: Option<String>,
    lint_absolute: ietsp::compile::Lint,
    lint_short: ietsp::compile::Lint,
    xp_features: Vec<String>,
    workers: usize,
    timings: bool,
    no_cache: bool,
    cache_dir: Option<String>,
    /// `eval-jobs --flake DIR` with no fragment: the whole `callFlake` result,
    /// not its `outputs`.
    flake_whole: bool,
}

enum ArgKind {
    Expr(String),
    Str(String),
}

enum Input {
    Expr(String),
    File(String),
    /// `DIR#attr.path`, see `ietsp::flake`.
    Flake(String),
}

fn parse_include(s: &str) -> (Vec<u8>, Vec<u8>) {
    let (prefix, path) = match s.split_once('=') {
        Some((p, v)) => (p.as_bytes().to_vec(), v),
        None => (Vec::new(), s),
    };
    let abs = if path.starts_with('/') {
        path.as_bytes().to_vec()
    } else {
        let mut cwd = std::env::current_dir()
            .map(|p| p.into_os_string().into_encoded_bytes())
            .unwrap_or_else(|_| b"/".to_vec());
        cwd.push(b'/');
        cwd.extend_from_slice(path.as_bytes());
        ietsp::compile::canon_path(&cwd)
    };
    (prefix, abs)
}

fn parse_eval_args(argv: &[String]) -> EvalArgs {
    let mut a = EvalArgs {
        strict: false,
        json: false,
        raw: false,
        includes: Vec::new(),
        args: Vec::new(),
        input: None,
        dump_drvs: None,
        attr: None,
        lint_absolute: ietsp::compile::Lint::Ignore,
        lint_short: ietsp::compile::Lint::Ignore,
        xp_features: Vec::new(),
        workers: ietsp::parallel::default_workers(),
        timings: false,
        no_cache: std::env::var_os("IETSP_CACHE").is_some_and(|v| v == "0"),
        cache_dir: None,
        flake_whole: false,
    };
    let mut i = 0;
    while i < argv.len() {
        let s = argv[i].as_str();
        match s {
            "--strict" => a.strict = true,
            "--json" => a.json = true,
            "--raw" => a.raw = true,
            "--timings" => a.timings = true,
            "--no-cache" => a.no_cache = true,
            "--cache-dir" => {
                i += 1;
                a.cache_dir = Some(argv.get(i).unwrap_or_else(|| usage()).clone());
            }
            "-I" | "--include" => {
                i += 1;
                a.includes
                    .push(parse_include(argv.get(i).unwrap_or_else(|| usage())));
            }
            "-E" | "--expr" => {
                i += 1;
                a.input = Some(Input::Expr(argv.get(i).unwrap_or_else(|| usage()).clone()));
            }
            "--arg" | "--argstr" => {
                let name = argv.get(i + 1).unwrap_or_else(|| usage()).clone();
                let val = argv.get(i + 2).unwrap_or_else(|| usage()).clone();
                a.args.push((
                    name,
                    if s == "--arg" {
                        ArgKind::Expr(val)
                    } else {
                        ArgKind::Str(val)
                    },
                ));
                i += 2;
            }
            "-A" | "--attr" => {
                i += 1;
                a.attr = Some(argv.get(i).unwrap_or_else(|| usage()).clone());
            }
            "--dump-drvs" => {
                i += 1;
                a.dump_drvs = Some(argv.get(i).unwrap_or_else(|| usage()).clone());
            }
            "--workers" => {
                i += 1;
                a.workers = argv
                    .get(i)
                    .and_then(|v| v.parse().ok())
                    .filter(|n| *n >= 1)
                    .unwrap_or_else(|| usage());
            }
            "--option" => {
                // accepted for nix-instantiate compatibility; ignored
                i += 2;
            }
            "--lint-absolute-path-literals"
            | "--lint-short-path-literals"
            | "--lint-url-literals" => {
                i += 1;
                let v = argv.get(i).unwrap_or_else(|| usage());
                let Some(mode) = ietsp::compile::Lint::parse(v) else {
                    eprintln!("error: invalid lint mode '{v}'");
                    usage()
                };
                match s {
                    "--lint-absolute-path-literals" => a.lint_absolute = mode,
                    "--lint-short-path-literals" => a.lint_short = mode,
                    _ => {}
                }
            }
            "--impure" | "--pure-eval" | "--readonly-mode" => {}
            "--extra-experimental-features" | "--experimental-features" => {
                i += 1;
                let v = argv.get(i).unwrap_or_else(|| usage());
                a.xp_features
                    .extend(v.split_whitespace().map(str::to_owned));
            }
            _ if s.starts_with("-I") && s.len() > 2 => a.includes.push(parse_include(&s[2..])),
            _ if s.starts_with('-') && s != "-" => {
                eprintln!("error: unknown flag '{s}'");
                usage()
            }
            _ if ietsp::flake::is_flake_ref(s) => a.input = Some(Input::Flake(s.to_string())),
            _ => a.input = Some(Input::File(s.to_string())),
        }
        i += 1;
    }
    a
}

fn home() -> Option<Vec<u8>> {
    std::env::var_os("HOME").map(|h| h.into_encoded_bytes())
}

fn flake_ref(f: &str, cwd: &[u8]) -> Result<ietsp::flake::FlakeRef, String> {
    ietsp::flake::parse_flake_ref(f, cwd, home().as_deref())
}

fn report(ev: &Evaluator, e: &Error) {
    eprintln!("error: {}", e.msg);
    if !e.pos.is_none() {
        eprintln!("       at {}", ev.describe_pos(e.pos));
    }
    for (pos, msg) in e.trace.iter() {
        if pos.is_none() {
            eprintln!("       … {msg}");
        } else {
            eprintln!("       … {msg} at {}", ev.describe_pos(*pos));
        }
    }
}

/// Applies the shared `eval` settings and evaluates the input, `--arg`s and `-A` included.
fn evaluate(ev: &mut Evaluator, a: EvalArgs) -> Result<Word, Box<Error>> {
    ev.compile_settings.lint_absolute_path_literals = a.lint_absolute;
    ev.compile_settings.lint_short_path_literals = a.lint_short;
    if a.xp_features.iter().any(|f| f == "parse-toml-timestamps") {
        ietsp::builtins::text::set_parse_toml_timestamps(true);
    }
    ietsp::builtins::set_search_path(ev, full_includes(&a.includes));
    if let Some(dir) = &a.dump_drvs {
        std::fs::create_dir_all(dir).map_err(|e| {
            Error::boxed(
                ietsp::eval::ErrorKind::Io,
                format!("cannot create '{dir}': {e}"),
                Pos::NONE,
            )
        })?;
        ev.drv.dump_dir = Some(std::path::PathBuf::from(dir));
    }

    let cwd = cwd();

    let mut v = match a.input {
        Some(Input::Expr(e)) => ev.eval_string(&e, &cwd)?,
        Some(Input::File(f)) => ev.eval_file(&abs_path(&f, &cwd), Pos::NONE)?,
        Some(Input::Flake(f)) => {
            let fr = flake_ref(&f, &cwd)
                .map_err(|m| Error::boxed(ietsp::eval::ErrorKind::Eval, m, Pos::NONE))?;
            if a.flake_whole && fr.attr.is_empty() {
                ietsp::flake::eval_flake_value(ev, &fr)?
            } else {
                ietsp::flake::eval_flake(ev, &fr)?
            }
        }
        None => usage(),
    };
    if !a.args.is_empty() {
        let args = auto_args(ev, &a.args, &cwd)?;
        v = ev.auto_call(v, args)?;
    }
    if let Some(attr) = &a.attr {
        for name in attr.split('.').filter(|n| !n.is_empty()) {
            let attrs = ev.force_attrs(v, Pos::NONE, "while selecting an attribute")?;
            let sym = ev.sym(name.as_bytes());
            let Some(a) = attrs.get(sym) else {
                return Err(Error::boxed(
                    ietsp::eval::ErrorKind::MissingAttr,
                    format!("attribute '{name}' in selection path '{attr}' not found"),
                    Pos::NONE,
                ));
            };
            v = a.cell.get();
        }
    }
    Ok(v)
}

/// The `--arg`/`--argstr` attrset (`nix-instantiate`'s autoArgs).
fn auto_args(
    ev: &mut Evaluator,
    args: &[(String, ArgKind)],
    cwd: &[u8],
) -> Result<Word, Box<Error>> {
    let mut entries = Vec::new();
    for (name, kind) in args {
        let w = match kind {
            ArgKind::Expr(e) => {
                let ir =
                    ev.compile_source(b"\xc2\xabarg\xc2\xbb", e.as_bytes().to_vec(), cwd, None)?;
                let root = ev.root_env;
                ev.arena.mk_thunk(root, ir)
            }
            ArgKind::Str(s) => ev.mk_string(s.as_bytes()),
        };
        let sym = ev.sym(name.as_bytes());
        entries.push((sym, Pos::NONE, w));
    }
    ev.mk_attrs(&mut entries, Pos::NONE)
}

/// `ietsp eval-jobs`'s own flags; everything else goes to `parse_eval_args`.
struct JobsArgs {
    opts: ietsp::jobs::Opts,
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
        opts: ietsp::jobs::Opts {
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
fn check_jobs_args(j: &JobsArgs) -> Result<ietsp::remote::worker::Transport, String> {
    if j.opts.chunk >= j.opts.chunks {
        return Err("--chunk must be below --chunks".into());
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
            return Err(format!(
                "--no-instantiate is incompatible with: {}. These features require instantiated derivations.",
                bad.join(", ")
            ));
        }
    }
    ietsp::remote::worker::Transport::parse(&j.eval_store).ok_or_else(|| {
        format!(
            "--eval-store '{}': eval-jobs needs a Nix daemon: daemon, unix:///path or ssh-ng://host",
            j.eval_store
        )
    })
}

/// `ietsp eval-jobs`: nix-eval-jobs's JSON lines. One process by default;
/// with `--max-memory-size`, worker processes restarted above the limit.
/// See docs/eval-jobs.md.
fn cmd_eval_jobs(argv: &[String]) -> i32 {
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
        let ev = Evaluator::new();
        return ietsp::parallel::run(1, ev, move |ev| jobs_worker(ev, a, &j, &transport));
    }
    if j.max_memory.is_some() {
        return jobs_collector(argv, a.workers, &j, &transport, t0);
    }
    let workers = a.workers;
    let ev = Evaluator::new();
    ietsp::parallel::run(workers, ev, move |ev| {
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
    ietsp::jobs::configure(ev, j.opts, args, apply);
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
    stats: ietsp::jobs::Stats,
    inst: Option<ietsp::jobs::instantiate::Stats>,
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
                ietsp::jobs::collector::SPAWNED.load(std::sync::atomic::Ordering::Relaxed)
            )
        } else {
            format!("peak RSS {} MB", ietsp::jobs::collector::peak_rss_mb())
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
    transport: &ietsp::remote::worker::Transport,
    t0: std::time::Instant,
) -> i32 {
    use ietsp::jobs::cache::Resolver;
    use ietsp::jobs::emit::Emitter;
    use ietsp::jobs::instantiate::{Instantiator, Store};
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
        ietsp::jobs::run(ev, root, &mut |line| em.lock().unwrap().emit(line));
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
        ietsp::jobs::run(ev, root, &mut |line| inst.push(line));
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
    transport: &ietsp::remote::worker::Transport,
) -> i32 {
    use ietsp::jobs::collector::{Out, run_worker, write_out};
    use ietsp::jobs::instantiate::{Instantiator, Store};
    use std::sync::{Arc, Mutex};

    let out: Out = Arc::new(Mutex::new(Box::new(std::io::stdout())));
    let fail = |out: &Out, msg: &str| -> i32 {
        eprintln!("{msg}");
        write_out(
            out,
            &format!(
                "{{\"error\":{}}}",
                String::from_utf8_lossy(&ietsp::jobs::line::json_str(msg))
            )
            .into_bytes(),
        );
        1
    };
    let root = match jobs_root(ev, a, j) {
        Ok(r) => r,
        Err(e) => return fail(&out, &ietsp::jobs::error_text(ev, &e)),
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
    transport: &ietsp::remote::worker::Transport,
    t0: std::time::Instant,
) -> i32 {
    use ietsp::jobs::Line;
    use ietsp::jobs::cache::Resolver;
    use ietsp::jobs::collector::{Sink, run_collector};
    use ietsp::jobs::emit::Emitter;
    use ietsp::remote::worker::Connection;
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
                .spawn(move || -> Result<(), String> {
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
            .map_err(|_| "cache-status thread panicked".to_string())?,
        None => Ok(()),
    });
    if let Err(e) = r {
        eprintln!("error: {e}");
        return 1;
    }
    if j.opts.constituents {
        let done = Connection::open(transport, &mut |_| {})
            .map_err(|e| e.to_string())
            .and_then(|mut conn| {
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

fn run_eval(a: EvalArgs) -> i32 {
    let mut ev = Evaluator::new();
    let cache = Cache::setup(&a, "eval", &ev);
    if let Some(c) = &cache
        && c.lookup()
        && let Some(out) = c.store.read(&c.key, ietsp::trace::store::RESULT)
    {
        use std::io::Write;
        let _ = std::io::stdout().write_all(&out);
        let _ = std::io::stdout().flush();
        return 0;
    }
    if cache.is_some() {
        ev.trace = ietsp::trace::Trace::enabled();
    }
    // parallel: the rest runs as the main fiber
    ietsp::parallel::run(a.workers, ev, move |ev| eval_and_print(ev, a, cache))
}

fn eval_and_print(ev: &mut Evaluator, a: EvalArgs, cache: Option<Cache>) -> i32 {
    let strict = a.strict;
    let json = a.json;
    let raw = a.raw;
    let t0 = std::time::Instant::now();
    let timings = a.timings;
    let v = match evaluate(ev, a) {
        Ok(v) => v,
        Err(e) => {
            report(ev, &e);
            return 1;
        }
    };

    let mut out: Vec<u8> = Vec::new();
    let r = if raw {
        // nix eval --raw: coerce without copying to the store, no newline
        let mut ctx = Vec::new();
        ev.coerce_to_string(
            v,
            &mut ctx,
            Pos::NONE,
            "while generating the eval command output",
            false,
            false,
            true,
        )
        .map(|s| out.extend_from_slice(&s))
    } else if json {
        let mut ctx = Vec::new();
        ietsp::print::print_json(ev, v, &mut out, &mut ctx, strict, Pos::NONE)
    } else {
        (|| {
            let v = if strict {
                ev.force_deep(v)?
            } else {
                ev.force(v)?
            };
            let mut seen = HashSet::new();
            ietsp::print::print_ambiguous(ev, v, 1, &mut out, &mut seen, 0)
        })()
    };
    if std::env::var_os("IETSP_STATS").is_some() {
        ev.print_stats();
    }
    match r {
        Ok(()) => {
            if !raw {
                out.push(b'\n');
            }
            use std::io::Write;
            let _ = std::io::stdout().write_all(&out);
            let _ = std::io::stdout().flush();
            if timings {
                eprintln!("eval: {:.3}s", t0.elapsed().as_secs_f64());
            }
            if let Some(c) = &cache {
                c.write(ev, &out, false);
            }
            0
        }
        Err(e) => {
            report(ev, &e);
            1
        }
    }
}

/// Answers the plan's locator from the local Nix store: one `nix path-info --json` call
/// tells how each input source was content-addressed and what it references. Paths named
/// by `--source` are read from the given local tree instead and are plain sources.
fn locate_in_store(
    paths: &[ietsp::store::path::StorePath],
    overrides: &HashMap<String, std::path::PathBuf>,
) -> Result<Vec<ietsp::remote::plan::SourceInfo>, String> {
    use ietsp::remote::plan::{SourceData, SourceInfo, SourceKind};
    use ietsp::store::path::{STORE_DIR, StorePath};

    let ask: Vec<String> = paths
        .iter()
        .map(|p| p.to_string())
        .filter(|p| !overrides.contains_key(p))
        .collect();
    let mut info: HashMap<String, serde_json::Value> = HashMap::new();
    if !ask.is_empty() {
        let out = std::process::Command::new("nix")
            .args([
                "--extra-experimental-features",
                "nix-command",
                "path-info",
                "--json",
            ])
            .args(&ask)
            .stderr(std::process::Stdio::inherit())
            .output()
            .map_err(|e| format!("nix path-info: {e}"))?;
        if !out.status.success() {
            return Err("nix path-info failed".into());
        }
        info = serde_json::from_slice(&out.stdout).map_err(|e| format!("nix path-info: {e}"))?;
    }
    paths
        .iter()
        .map(|p| {
            let full = p.to_string();
            if let Some(local) = overrides.get(&full) {
                return Ok(SourceInfo::tree(local.clone()));
            }
            let v = info
                .get(&full)
                .ok_or_else(|| format!("'{full}' is not in the local store"))?;
            let ca = v["ca"].as_str().unwrap_or("");
            let kind = if ca.starts_with("fixed:r:sha256:") {
                SourceKind::Source
            } else if ca.starts_with("text:sha256:") {
                let references = v["references"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|r| r.as_str()).collect::<Vec<_>>())
                    .unwrap_or_default()
                    .into_iter()
                    .map(|r| StorePath::parse(r).map_err(|e| e.0))
                    .collect::<Result<_, _>>()?;
                SourceKind::Text { references }
            } else {
                return Err(format!(
                    "'{full}': unsupported input source (ca = '{ca}'); v1 sends only recursive sha256 sources and text files"
                ));
            };
            Ok(SourceInfo {
                data: SourceData::Local {
                    path: std::path::PathBuf::from(format!("{STORE_DIR}/{}", p.base_name())),
                    accepted: None,
                },
                kind,
            })
        })
        .collect()
}

/// The derivations a build result names: an attrset with `drvPath`, a string with derivation
/// context, or a list of those. `!out!drv` context also names the wanted output.
fn drv_roots(
    ev: &mut Evaluator,
    v: Word,
    roots: &mut Vec<(ietsp::store::path::StorePath, Option<String>)>,
) -> Result<(), Box<Error>> {
    use ietsp::runtime::Ctx;
    use ietsp::store::path::StorePath;
    let bad = |msg: String| Error::boxed(ietsp::eval::ErrorKind::Eval, msg, Pos::NONE);
    let parse = |p: &[u8]| {
        StorePath::parse(&String::from_utf8_lossy(p))
            .map_err(|e| Error::boxed(ietsp::eval::ErrorKind::Eval, e.0, Pos::NONE))
    };
    let v = ev.force(v)?;
    if let Some(attrs) = v.as_attrs() {
        let Some(a) = attrs.get(ev.s.drv_path) else {
            return Err(bad(
                "expression evaluated to an attrset without 'drvPath'".into()
            ));
        };
        let s = ev.force_string(a.cell.get(), Pos::NONE, "while evaluating 'drvPath'")?;
        let p = parse(s.bytes)?;
        if !p.is_derivation() {
            return Err(bad(format!("'{p}' is not a derivation path")));
        }
        roots.push((p, None));
        return Ok(());
    }
    if let Some(s) = v.as_string() {
        let mut found = false;
        for c in s.ctx {
            match c {
                Ctx::Built { drv, output } => {
                    let out = String::from_utf8_lossy(output).into_owned();
                    roots.push((parse(drv)?, Some(out)));
                    found = true;
                }
                Ctx::DrvDeep(drv) => {
                    roots.push((parse(drv)?, None));
                    found = true;
                }
                Ctx::Opaque(_) => {}
            }
        }
        if !found {
            return Err(bad(format!(
                "string '{}' has no derivation in its context",
                String::from_utf8_lossy(s.bytes)
            )));
        }
        return Ok(());
    }
    if let Some(l) = v.as_list() {
        for c in l.cells() {
            drv_roots(ev, c.get(), roots)?;
        }
        return Ok(());
    }
    Err(bad(
        "expression must evaluate to a derivation, a string with derivation context, or a list of those".into(),
    ))
}

struct BuildArgs {
    url: String,
    timings: bool,
    no_build: bool,
    fetch_to: Option<std::path::PathBuf>,
    outputs: Vec<String>,
    sources: HashMap<String, std::path::PathBuf>,
}

/// `ietsp build`: evaluate to derivations and ship their closure from memory, or (legacy)
/// take a `.drv` path and read its closure from `/nix/store`.
fn cmd_build(argv: &[String]) -> i32 {
    let mut b = BuildArgs {
        url: "http://127.0.0.1:9455".to_string(),
        timings: false,
        no_build: false,
        fetch_to: None,
        outputs: Vec::new(),
        sources: HashMap::new(),
    };
    let mut rest: Vec<String> = Vec::new();
    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
            "--builder" => {
                i += 1;
                b.url = argv.get(i).cloned().unwrap_or_else(|| usage());
            }
            "--timings" => b.timings = true,
            "--no-build" => b.no_build = true,
            "--fetch-to" => {
                i += 1;
                b.fetch_to = Some(std::path::PathBuf::from(
                    argv.get(i).cloned().unwrap_or_else(|| usage()),
                ));
            }
            "--out" => {
                i += 1;
                b.outputs
                    .push(argv.get(i).cloned().unwrap_or_else(|| usage()));
            }
            "--source" => {
                i += 1;
                let spec = argv.get(i).unwrap_or_else(|| usage());
                let (store, local) = spec.split_once('=').unwrap_or_else(|| usage());
                b.sources
                    .insert(store.to_string(), std::path::PathBuf::from(local));
            }
            s => rest.push(s.to_string()),
        }
        i += 1;
    }
    let a = parse_eval_args(&rest);
    if let Some(Input::File(f)) = &a.input
        && f.ends_with(".drv")
    {
        return build_drv_file(&b, f);
    }

    let t0 = std::time::Instant::now();
    let mut ev = Evaluator::new();
    let a = EvalArgs {
        timings: a.timings || b.timings,
        ..a
    };
    let cache = Cache::setup(&a, "build", &ev);
    if let Some(c) = &cache
        && c.lookup()
        && let Some(hit) = c.read_build()
    {
        let (roots, drvs, sources) = hit;
        if b.timings {
            eprintln!(
                "eval: skipped, {} derivations and {} sources from the cache, {:.3}s",
                drvs.len(),
                sources.len(),
                t0.elapsed().as_secs_f64()
            );
        }
        let (root_paths, outputs) = build_targets(&b, &roots);
        let inputs = ietsp::remote::plan::Inputs {
            drvs: &drvs,
            sources: &sources,
        };
        return build_roots(&b, &root_paths, &outputs, inputs);
    }
    if cache.is_some() {
        ev.trace = ietsp::trace::Trace::enabled();
    }
    // parallel: eval and build run as the main fiber
    ietsp::parallel::run(a.workers, ev, move |ev| build_eval(ev, &b, a, t0, cache))
}

/// The drv paths to build and the outputs to print: `--out` plus the outputs the roots' context named.
fn build_targets(
    b: &BuildArgs,
    roots: &[(ietsp::store::path::StorePath, Option<String>)],
) -> (Vec<ietsp::store::path::StorePath>, Vec<String>) {
    let mut outputs = b.outputs.clone();
    for (_, out) in roots {
        if let Some(o) = out
            && !outputs.contains(o)
        {
            outputs.push(o.clone());
        }
    }
    (roots.iter().map(|(p, _)| p.clone()).collect(), outputs)
}

fn build_eval(
    ev: &mut Evaluator,
    b: &BuildArgs,
    a: EvalArgs,
    t0: std::time::Instant,
    cache: Option<Cache>,
) -> i32 {
    let mut roots = Vec::new();
    let r = evaluate(ev, a).and_then(|v| drv_roots(ev, v, &mut roots));
    if let Err(e) = r {
        report(ev, &e);
        return 1;
    }
    let eval_time = t0.elapsed();
    if b.timings {
        eprintln!(
            "eval: {} derivations, {} sources, {:.3}s",
            ev.drv.nr_derivations,
            ev.source_origins().len(),
            eval_time.as_secs_f64()
        );
    }
    if let Some(c) = &cache {
        c.write(ev, &ietsp::trace::wire::encode_roots(&roots), true);
    }
    let (root_paths, outputs) = build_targets(b, &roots);
    let drvs = ev.drv.drvs.lock();
    let sources = ev.source_origins();
    let inputs = ietsp::remote::plan::Inputs {
        drvs: &drvs,
        sources: &sources,
    };
    build_roots(b, &root_paths, &outputs, inputs)
}

/// Legacy mode: `DRV` is a `.drv` store path, or a copy of the file named
/// `<hash>-<name>.drv`; its closure is read from `/nix/store`.
fn build_drv_file(b: &BuildArgs, root_arg: &str) -> i32 {
    use ietsp::builtins::derivation::DrvInfo;
    use ietsp::store::drv::Derivation;
    use ietsp::store::path::StorePath;
    let base = std::path::Path::new(root_arg)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let root = match StorePath::from_base_name(&base) {
        Ok(p) if p.is_derivation() => p,
        Ok(p) => {
            eprintln!("error: '{p}' is not a derivation");
            return 1;
        }
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    // The root may be a copy outside the store; its inputs are read from `/nix/store`.
    let info = std::fs::read_to_string(root_arg)
        .map_err(|e| e.to_string())
        .and_then(|text| {
            let name = Derivation::name_from_path(&root).map_err(|e| e.0)?;
            let drv = Derivation::parse(&text, name).map_err(|e| e.0)?;
            let mut references = drv.input_srcs;
            references.extend(drv.input_drvs.into_keys());
            Ok(DrvInfo {
                outputs: drv.outputs.into_keys().collect(),
                references,
                aterm: Some(text.into_bytes()),
            })
        });
    let info = match info {
        Ok(i) => i,
        Err(e) => {
            eprintln!("error: cannot read derivation '{root_arg}': {e}");
            return 1;
        }
    };
    let drvs = HashMap::from([(root.clone(), info)]);
    let sources = HashMap::new();
    build_roots(
        b,
        &[root],
        &b.outputs,
        ietsp::remote::plan::Inputs {
            drvs: &drvs,
            sources: &sources,
        },
    )
}

fn build_roots(
    b: &BuildArgs,
    roots: &[ietsp::store::path::StorePath],
    outputs: &[String],
    inputs: ietsp::remote::plan::Inputs<'_>,
) -> i32 {
    use ietsp::remote::proto::build_event::Event;
    use ietsp::store::path::StorePath;

    let t0 = std::time::Instant::now();
    let locate = |paths: &[StorePath]| locate_in_store(paths, &b.sources);
    let plan = match ietsp::remote::plan::plan(roots, inputs, &locate) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    let plan_time = t0.elapsed();
    if b.timings && (plan.store_drvs > 0 || plan.store_sources > 0) {
        eprintln!(
            "plan: {} drvs and {} sources not from this eval, read from /nix/store",
            plan.store_drvs, plan.store_sources
        );
    }

    let transport = ietsp::remote::worker::Transport::parse(&b.url);
    let mut built = Vec::new();
    let mut failed = false;
    let roots = if b.no_build { &roots[..1] } else { roots };
    for root in roots {
        let mut on_event = |ev: Event| match ev {
            Event::Scheduled(s) => eprintln!("scheduled: {}", s.drv_paths.join(" ")),
            Event::Started(s) => eprintln!("building {}", s.drv_path),
            Event::Log(l) => {
                let line = String::from_utf8_lossy(&l.line);
                match StorePath::parse(&l.drv_path) {
                    Ok(p) => eprintln!("{}> {line}", p.name()),
                    Err(_) => eprintln!("{line}"),
                }
            }
            Event::Built(b) => built.push((b.output, b.store_path)),
            Event::Failed(f) => {
                failed = true;
                eprintln!("error: build of {} failed: {}", f.drv_path, f.reason);
            }
            Event::Done(d) => failed |= !d.success,
        };
        let report = if let Some(transport) = &transport {
            ietsp::remote::worker::build::build(
                transport,
                &plan,
                root,
                outputs,
                b.no_build,
                b.fetch_to.as_deref(),
                &mut on_event,
            )
            .map(|t| t.report(&plan, plan_time))
            .map_err(|e| e.to_string())
        } else {
            ietsp::remote::client::build_blocking(
                &b.url,
                &plan,
                root,
                outputs,
                b.no_build,
                &mut on_event,
            )
            .map(|t| t.report(&plan, plan_time))
            .map_err(|e| e.to_string())
        };
        match report {
            Ok(r) => {
                if b.timings {
                    eprintln!("{r}");
                }
            }
            Err(e) => {
                eprintln!("error: {e}");
                return 1;
            }
        }
    }
    let wanted: Vec<&(String, String)> = built
        .iter()
        .filter(|(name, _)| outputs.is_empty() || outputs.contains(name))
        .collect();
    if let Some(dir) = &b.fetch_to
        && transport.is_none()
        && !failed
    {
        let t0 = std::time::Instant::now();
        if let Err(e) = std::fs::create_dir_all(dir) {
            eprintln!("error: cannot create '{}': {e}", dir.display());
            return 1;
        }
        for (_, path) in &wanted {
            let dest = match StorePath::parse(path) {
                Ok(p) => dir.join(p.base_name()),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 1;
                }
            };
            if let Err(e) = ietsp::remote::client::fetch_blocking(&b.url, path, &dest) {
                eprintln!("error: fetch {path}: {e}");
                return 1;
            }
        }
        if b.timings {
            eprintln!(
                "fetch: {} outputs, {:.3}s",
                wanted.len(),
                t0.elapsed().as_secs_f64()
            );
        }
    }
    for (_, path) in &wanted {
        println!("{path}");
    }
    if failed { 1 } else { 0 }
}

struct DevEnvArgs {
    transport: ietsp::remote::worker::Transport,
    /// Absolute; a symlink to the env output, registered as an indirect GC root.
    gc_root: Option<std::path::PathBuf>,
}

/// Build events on the daemon connection: log lines are printed live with `--timings`,
/// else kept and printed only when the build fails.
struct EventSink {
    live: bool,
    log: Vec<String>,
    failed: Option<String>,
}

impl EventSink {
    fn event(&mut self, e: ietsp::remote::proto::build_event::Event) {
        use ietsp::remote::proto::build_event::Event;
        match e {
            Event::Log(l) => {
                let line = String::from_utf8_lossy(&l.line).into_owned();
                if self.live {
                    eprintln!("ietsp: {line}");
                } else {
                    self.log.push(line);
                }
            }
            Event::Started(s) if self.live => eprintln!("ietsp: building {}", s.drv_path),
            Event::Failed(f) => {
                self.failed = Some(format!("build of {} failed: {}", f.drv_path, f.reason));
            }
            Event::Done(d) if !d.success && self.failed.is_none() => {
                self.failed = Some("build failed".into());
            }
            _ => {}
        }
    }

    fn dump(&self) {
        for l in &self.log {
            eprintln!("ietsp: {l}");
        }
    }
}

/// `ietsp print-dev-env`: the bash `nix print-dev-env` prints for a flake's shell, from the
/// in-memory shell derivation through a Nix daemon. See docs/direnv.md.
fn cmd_print_dev_env(argv: &[String]) -> i32 {
    let mut url = "daemon".to_string();
    let mut gc_root: Option<String> = None;
    let mut rest: Vec<String> = Vec::new();
    let mut flake: Option<String> = None;
    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
            "--builder" => {
                i += 1;
                url = argv.get(i).cloned().unwrap_or_else(|| usage());
            }
            "--gc-root" => {
                i += 1;
                gc_root = Some(argv.get(i).cloned().unwrap_or_else(|| usage()));
            }
            s if s.starts_with('-') => rest.push(s.to_string()),
            s => {
                if flake.replace(s.to_string()).is_some() {
                    usage()
                }
            }
        }
        i += 1;
    }
    let mut a = parse_eval_args(&rest);
    a.input = Some(Input::Flake(flake.unwrap_or_else(|| ".".into())));
    let Some(transport) = ietsp::remote::worker::Transport::parse(&url) else {
        eprintln!(
            "error: print-dev-env needs a Nix daemon: --builder daemon, unix:///path or ssh-ng://host"
        );
        return 1;
    };
    let cwd = cwd();
    let gc_root = gc_root.map(|r| {
        std::path::PathBuf::from(
            <std::ffi::OsStr as std::os::unix::ffi::OsStrExt>::from_bytes(&abs_path(&r, &cwd)),
        )
    });
    if gc_root.is_some() && !matches!(transport, ietsp::remote::worker::Transport::Unix(_)) {
        eprintln!("error: --gc-root needs a local daemon (daemon or unix://)");
        return 1;
    }
    let d = DevEnvArgs { transport, gc_root };

    let t0 = std::time::Instant::now();
    let mut ev = Evaluator::new();
    let cache = Cache::setup(&a, "print-dev-env", &ev);
    if let Some(c) = &cache
        && c.lookup()
        && let Some(bytes) = c.store.read(&c.key, ietsp::trace::store::RESULT)
        && let Some(hit) = ietsp::devenv::CachedResult::decode(&bytes)
    {
        match dev_env_hit(&d, &hit) {
            Ok(true) => {
                use std::io::Write;
                let _ = std::io::stdout().write_all(&hit.bytes);
                let _ = std::io::stdout().flush();
                if a.timings {
                    eprintln!("total: {:.3}s", t0.elapsed().as_secs_f64());
                }
                return 0;
            }
            Ok(false) => {
                if a.timings {
                    eprintln!("cache: env output '{}' is gone, rebuilding", hit.out_path);
                }
            }
            Err(e) => {
                eprintln!("error: {e}");
                return 1;
            }
        }
    }
    if cache.is_some() {
        ev.trace = ietsp::trace::Trace::enabled();
    }
    // parallel: eval and the daemon flow run as the main fiber
    ietsp::parallel::run(a.workers, ev, move |ev| dev_env_full(ev, a, &d, cache, t0))
}

/// A cache hit is usable when the env output (and the files the script names) still exist;
/// the GC root is refreshed when it does not point at the output.
fn dev_env_hit(d: &DevEnvArgs, hit: &ietsp::devenv::CachedResult) -> Result<bool, String> {
    use ietsp::remote::worker::{Connection, Transport};
    let present = match &d.transport {
        Transport::Unix(_) => std::path::Path::new(&hit.out_path).exists(),
        Transport::Ssh { .. } => {
            let mut conn =
                Connection::open(&d.transport, &mut |_| {}).map_err(|e| e.to_string())?;
            !conn
                .query_valid_paths(std::slice::from_ref(&hit.out_path), &mut |_| {})
                .map_err(|e| e.to_string())?
                .is_empty()
        }
    };
    if !present || !hit.files.iter().all(|f| f.exists()) {
        return Ok(false);
    }
    if let Some(root) = &d.gc_root {
        ensure_gc_root(&d.transport, root, &hit.out_path, None)?;
    }
    Ok(true)
}

/// indirect-root-store.cc:addPermRoot: a symlink at `root` to `out_path`, then
/// `AddIndirectRoot`. Nothing happens when the link already points there.
fn ensure_gc_root(
    transport: &ietsp::remote::worker::Transport,
    root: &std::path::Path,
    out_path: &str,
    conn: Option<&mut ietsp::remote::worker::Connection>,
) -> Result<(), String> {
    use ietsp::remote::worker::Connection;
    use ietsp::store::path::STORE_DIR;
    if std::fs::read_link(root).ok().as_deref() == Some(std::path::Path::new(out_path)) {
        return Ok(());
    }
    if root.starts_with(STORE_DIR) {
        return Err(format!(
            "creating a garbage collector root ({}) in the Nix store is forbidden",
            root.display()
        ));
    }
    if let Ok(m) = std::fs::symlink_metadata(root)
        && (!m.file_type().is_symlink()
            || !std::fs::read_link(root)
                .map(|t| t.starts_with(STORE_DIR))
                .unwrap_or(false))
    {
        return Err(format!(
            "cannot create symlink {}; already exists",
            root.display()
        ));
    }
    ietsp::remote::worker::replace_symlink(out_path, root)
        .map_err(|e| format!("cannot create {}: {e}", root.display()))?;
    let root_s = root.to_str().ok_or("gc root path is not UTF-8")?;
    let mut opened;
    let conn = match conn {
        Some(c) => c,
        None => {
            opened = Connection::open(transport, &mut |_| {}).map_err(|e| e.to_string())?;
            &mut opened
        }
    };
    conn.add_indirect_root(root_s, &mut |_| {})
        .map_err(|e| e.to_string())
}

/// The cache-miss path: evaluate the shell, derive and build its `-env` derivation through
/// the daemon, read the environment, print it, and remember the bytes.
fn dev_env_full(
    ev: &mut Evaluator,
    a: EvalArgs,
    d: &DevEnvArgs,
    cache: Option<Cache>,
    t0: std::time::Instant,
) -> i32 {
    use ietsp::builtins::derivation::{DrvInfo, load_drv_traced};
    use ietsp::devenv;
    use ietsp::eval::ErrorKind;
    use ietsp::remote::worker::Connection;
    use ietsp::store::drv::Derivation;
    use ietsp::store::path::StorePath;
    use ietsp::store::source::SourceOrigin;
    use std::collections::BTreeSet;

    let timings = a.timings;
    let json = a.json;
    let cwd = cwd();
    let f = match &a.input {
        Some(Input::Flake(f)) => f.clone(),
        _ => usage(),
    };
    let (loc, frag) = f.split_once('#').unwrap_or((f.as_str(), ""));
    let candidates = devenv::attr_paths(frag, ev.current_system);
    let mut roots = Vec::new();
    let r = (|| {
        let outputs = evaluate(
            ev,
            EvalArgs {
                input: Some(Input::Flake(loc.to_string())),
                ..a
            },
        )?;
        for c in &candidates {
            let path = ietsp::flake::parse_attr_path(c)
                .map_err(|m| Error::boxed(ErrorKind::Eval, m, Pos::NONE))?;
            if let Some(v) = ietsp::flake::select(ev, outputs, &path)? {
                return drv_roots(ev, v, &mut roots);
            }
        }
        Err(Error::boxed(
            ErrorKind::MissingAttr,
            format!(
                "flake '{loc}' does not provide attribute {}",
                devenv::show_attr_paths(&candidates)
            ),
            Pos::NONE,
        ))
    })();
    if let Err(e) = r {
        report(ev, &e);
        return 1;
    }
    if roots.len() != 1 {
        eprintln!(
            "error: '{f}' needs to evaluate to a single derivation, but it evaluated to {} derivations",
            roots.len()
        );
        return 1;
    }
    let shell_path = roots[0].0.clone();
    let eval_time = t0.elapsed();

    let r = (|| -> Result<Vec<u8>, String> {
        let shell = match ev.drv.drvs.get(&shell_path).and_then(|i| i.aterm) {
            Some(aterm) => Derivation::parse(
                std::str::from_utf8(&aterm).map_err(|e| e.to_string())?,
                Derivation::name_from_path(&shell_path).map_err(|e| e.0)?,
            ),
            None => load_drv_traced(&ev.trace, &shell_path),
        }
        .map_err(|e| e.0)?;
        let trace = ev.trace.clone();
        let mut loader = move |p: &StorePath| load_drv_traced(&trace, p);
        let env_drv = devenv::env_derivation(&shell, &mut ev.drv.hash_cache.lock(), &mut loader)
            .map_err(|e| e.0)?;
        let text = env_drv.unparse().map_err(|e| e.0)?;
        let env_path = env_drv.drv_path_for(&text).map_err(|e| e.0)?;
        let mut references = env_drv.input_srcs.clone();
        references.extend(env_drv.input_drvs.keys().cloned());
        ev.drv.drvs.insert(
            env_path.clone(),
            DrvInfo {
                outputs: env_drv.outputs.keys().cloned().collect(),
                references,
                aterm: Some(text.into_bytes()),
            },
        );
        ev.sources.insert(
            devenv::get_env_sh_path(),
            SourceOrigin::Text {
                contents: devenv::GET_ENV_SH.as_bytes().to_vec(),
                references: BTreeSet::new(),
            },
        );
        let mut out_paths = Vec::new();
        for (name, out) in &env_drv.outputs {
            match out.path(&env_drv.name, name).map_err(|e| e.0)? {
                Some(p) => out_paths.push(p.to_string()),
                None => return Err("content-addressed shells are not supported".into()),
            }
        }
        if timings {
            eprintln!(
                "eval: {} derivations, {:.3}s\nenv drv: {env_path}",
                ev.drv.nr_derivations,
                eval_time.as_secs_f64()
            );
        }

        let t1 = std::time::Instant::now();
        let mut sink = EventSink {
            live: timings,
            log: Vec::new(),
            failed: None,
        };
        let mut conn =
            Connection::open(&d.transport, &mut |e| sink.event(e)).map_err(|e| e.to_string())?;
        let valid = conn
            .query_valid_paths(&out_paths, &mut |e| sink.event(e))
            .map_err(|e| e.to_string())?;
        if valid.len() != out_paths.len() {
            let t2 = std::time::Instant::now();
            let no_overrides = HashMap::new();
            let locate = |paths: &[StorePath]| locate_in_store(paths, &no_overrides);
            let plan = {
                let drvs = ev.drv.drvs.lock();
                let sources = ev.source_origins();
                let inputs = ietsp::remote::plan::Inputs {
                    drvs: &drvs,
                    sources: &sources,
                };
                ietsp::remote::plan::plan(std::slice::from_ref(&env_path), inputs, &locate)
                    .map_err(|e| e.to_string())?
            };
            let plan_time = t2.elapsed();
            let t = ietsp::remote::worker::build::build_on(
                &mut conn,
                &plan,
                &env_path,
                &[],
                false,
                None,
                &mut |e| sink.event(e),
            )
            .map_err(|e| e.to_string())?;
            if let Some(why) = &sink.failed {
                sink.dump();
                return Err(why.clone());
            }
            if timings {
                eprintln!("{}", t.report(&plan, plan_time));
            }
        } else if timings {
            eprintln!(
                "daemon: env output already valid, {:.3}s",
                t1.elapsed().as_secs_f64()
            );
        }

        let (out_path, bytes) = read_env_output(&d.transport, &mut conn, &out_paths, &mut sink)?;
        let env = devenv::BuildEnvironment::parse(&bytes)?;
        if let Some(root) = &d.gc_root {
            ensure_gc_root(&d.transport, root, &out_path, Some(&mut conn))?;
        }
        drop(conn);

        let (mut output, files) = if json {
            (
                serde_json::to_vec(&env.to_json()).map_err(|e| e.to_string())?,
                Vec::new(),
            )
        } else {
            let attrs_dir = match &d.gc_root {
                Some(r) => std::path::PathBuf::from(format!("{}-attrs", r.display())),
                None => std::env::temp_dir().join(format!("ietsp-dev-env-{}", std::process::id())),
            };
            let outputs_dir = std::path::Path::new(
                <std::ffi::OsStr as std::os::unix::ffi::OsStrExt>::from_bytes(&cwd),
            )
            .join("outputs");
            let rc = devenv::make_rc_script(&env, &attrs_dir, &outputs_dir)?;
            (rc.script.into_bytes(), rc.files)
        };
        output.push(b'\n');
        if let Some(c) = &cache {
            c.write(
                ev,
                &devenv::CachedResult {
                    out_path,
                    files,
                    bytes: output.clone(),
                }
                .encode(),
                false,
            );
        }
        if timings {
            eprintln!("daemon: {:.3}s", t1.elapsed().as_secs_f64());
        }
        Ok(output)
    })();
    match r {
        Ok(out) => {
            use std::io::Write;
            let _ = std::io::stdout().write_all(&out);
            let _ = std::io::stdout().flush();
            if timings {
                eprintln!("total: {:.3}s", t0.elapsed().as_secs_f64());
            }
            0
        }
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    }
}

/// develop.cc:getDerivationEnvironment, after the build: the first output (by name) whose
/// file is not empty holds the JSON. A local daemon's output is read from disk; a remote one
/// arrives as a NAR.
fn read_env_output(
    transport: &ietsp::remote::worker::Transport,
    conn: &mut ietsp::remote::worker::Connection,
    out_paths: &[String],
    sink: &mut EventSink,
) -> Result<(String, Vec<u8>), String> {
    use ietsp::remote::worker::Transport;
    use ietsp::store::path::StorePath;
    for p in out_paths {
        let bytes = match transport {
            Transport::Unix(_) => {
                std::fs::read(p).map_err(|e| format!("cannot read '{p}': {e}"))?
            }
            Transport::Ssh { .. } => {
                let dir =
                    std::env::temp_dir().join(format!("ietsp-dev-env-{}", std::process::id()));
                std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
                let dest = dir.join(StorePath::parse(p).map_err(|e| e.0)?.base_name());
                let _ = std::fs::remove_file(&dest);
                conn.nar_from_path(p, &mut |e| sink.event(e), &mut |r| {
                    ietsp::store::nar::restore_path(r, &dest)
                })
                .map_err(|e| e.to_string())?;
                let bytes = std::fs::read(&dest).map_err(|e| e.to_string())?;
                let _ = std::fs::remove_dir_all(&dir);
                bytes
            }
        };
        if !bytes.is_empty() {
            return Ok((p.clone(), bytes));
        }
    }
    Err("get-env.sh failed to produce an environment".into())
}

/// One line per observation of a `readset` file, for debugging misses.
fn cmd_cache_dump(file: &str) -> i32 {
    use ietsp::trace::Obs;
    let Ok(bytes) = std::fs::read(file) else {
        eprintln!("error: cannot read {file}");
        return 1;
    };
    let Some(obs) = ietsp::trace::wire::decode_readset(&bytes) else {
        eprintln!("error: {file} is not a readset");
        return 1;
    };
    let hex = |d: &[u8]| d.iter().map(|b| format!("{b:02x}")).collect::<String>();
    let s = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    for o in &obs {
        match o {
            Obs::File {
                path, size, hash, ..
            } => println!("file {} {size} {}", s(path), &hex(hash)[..16]),
            Obs::Dir { path, hash } => println!("dir {} {}", s(path), &hex(hash)[..16]),
            Obs::Stat { path, follow, kind } => {
                println!("stat {} follow={follow} {kind:?}", s(path))
            }
            Obs::Tree { root, filtered, .. } => println!(
                "tree {} accepted={}",
                s(root),
                filtered
                    .as_ref()
                    .map_or("all".to_string(), |f| f.accepted.len().to_string())
            ),
            Obs::Canon { path, target } => println!(
                "canon {} -> {}",
                s(path),
                target.as_deref().map_or("(none)".to_string(), s)
            ),
            Obs::Env { name, value } => println!(
                "env {}={}",
                s(name),
                value.as_deref().map_or("(unset)".to_string(), s)
            ),
            Obs::Cmd { dir, argv, hash } => println!(
                "cmd {} `{}` {}",
                s(dir),
                argv.iter().map(|a| s(a)).collect::<Vec<_>>().join(" "),
                &hex(hash)[..16]
            ),
        }
    }
    0
}

fn cwd() -> Vec<u8> {
    std::env::current_dir()
        .map(|p| p.into_os_string().into_encoded_bytes())
        .unwrap_or_else(|_| b"/".to_vec())
}

fn abs_path(f: &str, cwd: &[u8]) -> Vec<u8> {
    let p = f.as_bytes();
    if p.starts_with(b"/") {
        return p.to_vec();
    }
    let mut full = cwd.to_vec();
    full.push(b'/');
    full.extend_from_slice(p);
    ietsp::compile::canon_path(&full)
}

/// `-I` entries first, then `NIX_PATH`, as absolute `(prefix, path)` pairs.
fn full_includes(includes: &[(Vec<u8>, Vec<u8>)]) -> Vec<(Vec<u8>, Vec<u8>)> {
    let mut all = includes.to_vec();
    if let Ok(np) = std::env::var("NIX_PATH") {
        for entry in np.split(':').filter(|s| !s.is_empty()) {
            all.push(parse_include(entry));
        }
    }
    all
}

/// The tracing cache for one run: where the entry lives and its key. See docs/tracing-cache.md.
struct Cache {
    store: ietsp::trace::store::Store,
    key: String,
    timings: bool,
}

impl Cache {
    /// `None` when the cache is off or the command cannot be keyed.
    fn setup(a: &EvalArgs, subcommand: &str, ev: &Evaluator) -> Option<Cache> {
        use ietsp::trace::key::{Key, has_relative_path};
        if a.no_cache || a.dump_drvs.is_some() {
            return None;
        }
        let store = ietsp::trace::store::Store::locate(a.cache_dir.as_deref())?;
        let cwd = cwd();
        let mut k = Key::new(subcommand);
        let mut needs_cwd = false;
        match a.input.as_ref()? {
            Input::Expr(e) => {
                k.field("expr", e.as_bytes());
                needs_cwd |= has_relative_path(e);
            }
            Input::File(f) => {
                let p = abs_path(f, &cwd);
                let bytes = std::fs::read(std::path::Path::new(
                    <std::ffi::OsStr as std::os::unix::ffi::OsStrExt>::from_bytes(&p),
                ))
                .ok()?;
                k.field("file", &p);
                k.field("file-hash", &ietsp::trace::sha256(&bytes));
            }
            Input::Flake(f) => {
                let fr = flake_ref(f, &cwd).ok()?;
                k.field("flake", f.as_bytes());
                k.field("flake-dir", fr.dir.as_os_str().as_encoded_bytes());
                for name in ["flake.nix", "flake.lock"] {
                    let bytes = std::fs::read(fr.dir.join(name)).ok()?;
                    k.field(name, &ietsp::trace::sha256(&bytes));
                }
                needs_cwd = true;
            }
        }
        for (name, kind) in &a.args {
            match kind {
                ArgKind::Expr(e) => {
                    k.field("arg", name.as_bytes());
                    k.field("val", e.as_bytes());
                    needs_cwd |= has_relative_path(e);
                }
                ArgKind::Str(s) => {
                    k.field("argstr", name.as_bytes());
                    k.field("val", s.as_bytes());
                }
            }
        }
        for (prefix, path) in full_includes(&a.includes) {
            k.field("include-prefix", &prefix);
            k.field("include-path", &path);
        }
        k.field("attr", a.attr.as_deref().unwrap_or("").as_bytes());
        k.flag("strict", a.strict);
        k.flag("json", a.json);
        k.flag("raw", a.raw);
        k.field("lint-absolute", format!("{:?}", a.lint_absolute).as_bytes());
        k.field("lint-short", format!("{:?}", a.lint_short).as_bytes());
        for f in &a.xp_features {
            k.field("xp", f.as_bytes());
        }
        k.field("system", ev.current_system.as_bytes());
        if needs_cwd {
            k.field("cwd", &cwd);
        }
        Some(Cache {
            store,
            key: k.finish(),
            timings: a.timings,
        })
    }

    /// Validates the stored read set; true on a hit. A hit that refreshed
    /// stat fields rewrites `readset`.
    fn lookup(&self) -> bool {
        use ietsp::trace::store::READSET;
        let t0 = std::time::Instant::now();
        let Some(bytes) = self.store.read(&self.key, READSET) else {
            if self.timings {
                eprintln!("cache: miss (no entry)");
            }
            return false;
        };
        let Some(mut obs) = ietsp::trace::wire::decode_readset(&bytes) else {
            if self.timings {
                eprintln!("cache: miss (unreadable entry)");
            }
            return false;
        };
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get().min(4));
        let t_load = t0.elapsed();
        match ietsp::trace::validate::validate(&mut obs, threads) {
            Ok(r) => {
                if r.refreshed > 0 {
                    let _ = self.store.write(
                        &self.key,
                        READSET,
                        &ietsp::trace::wire::encode_readset(&obs),
                    );
                }
                if self.timings {
                    eprintln!(
                        "cache: hit, {} entries validated, {} rehashed, load {:.3}s, validate {:.3}s",
                        r.entries,
                        r.rehashed,
                        t_load.as_secs_f64(),
                        (t0.elapsed() - t_load).as_secs_f64()
                    );
                }
                true
            }
            Err(m) => {
                if self.timings {
                    eprintln!(
                        "cache: miss ({}), {} entries, {:.3}s",
                        m.0,
                        obs.len(),
                        t0.elapsed().as_secs_f64()
                    );
                }
                false
            }
        }
    }

    /// Writes the entry after a successful run; a run that observed
    /// `currentTime` or called a fetcher writes nothing. `tables` adds the
    /// drv and source tables, which only a `build` hit reads back.
    fn write(&self, ev: &Evaluator, result: &[u8], tables: bool) {
        use ietsp::trace::store::{DRVS, READSET, RESULT, SOURCES};
        use ietsp::trace::wire;
        if !ev.trace.is_cacheable() {
            if self.timings {
                eprintln!("cache: not written (uncacheable run)");
            }
            return;
        }
        let t0 = std::time::Instant::now();
        let obs = ev.trace.take();
        let r = if tables {
            self.store
                .write(&self.key, DRVS, &wire::encode_drvs(&ev.drv.drvs.lock()))
                .and_then(|_| {
                    self.store.write(
                        &self.key,
                        SOURCES,
                        &wire::encode_sources(&ev.source_origins()),
                    )
                })
        } else {
            Ok(())
        };
        let r = r
            .and_then(|_| self.store.write(&self.key, RESULT, result))
            .and_then(|_| {
                self.store
                    .write(&self.key, READSET, &wire::encode_readset(&obs))
            });
        match r {
            Ok(()) => {
                if self.timings {
                    eprintln!(
                        "cache: wrote {} entries, {:.3}s",
                        obs.len(),
                        t0.elapsed().as_secs_f64()
                    );
                }
            }
            Err(e) => eprintln!(
                "warning: cannot write cache entry {}: {e}",
                self.store.entry_dir(&self.key).display()
            ),
        }
    }

    /// The roots, drv table and source table of a `build` entry.
    #[allow(clippy::type_complexity)]
    fn read_build(
        &self,
    ) -> Option<(
        Vec<(ietsp::store::path::StorePath, Option<String>)>,
        HashMap<ietsp::store::path::StorePath, ietsp::builtins::derivation::DrvInfo>,
        HashMap<ietsp::store::path::StorePath, ietsp::store::source::SourceOrigin>,
    )> {
        use ietsp::trace::store::{DRVS, RESULT, SOURCES};
        use ietsp::trace::wire;
        let roots = wire::decode_roots(&self.store.read(&self.key, RESULT)?)?;
        let drvs = wire::decode_drvs(&self.store.read(&self.key, DRVS)?)?;
        let sources = wire::decode_sources(&self.store.read(&self.key, SOURCES)?)?;
        Some((roots, drvs, sources))
    }
}

/// `ietsp amber-keys [--lstat] PATH`: every amber object of the tree, sorted by key, in the
/// format of `tools/amber-diff/amber-keys.rs` so the two can be compared.
fn cmd_amber_keys(argv: &[String]) -> i32 {
    use ietsp::amber::tree::{Meta, Sink, ingest_path, no_filter};
    use ietsp::amber::{Key, Object, Type};
    use std::collections::BTreeMap;
    use std::io::Write;
    use std::sync::Mutex;

    let mut meta = Meta::Canonical;
    let mut path = None;
    for a in argv {
        match a.as_str() {
            "--lstat" => meta = Meta::Lstat,
            p => path = Some(p.to_string()),
        }
    }
    let Some(path) = path else {
        eprintln!("usage: ietsp amber-keys [--lstat] PATH");
        return 2;
    };
    struct Collect(Mutex<BTreeMap<Key, Option<Vec<u8>>>>);
    impl Sink for Collect {
        fn emit(&self, o: Object) {
            let bytes = (o.key.type_() != Type::Blob).then_some(o.bytes);
            self.0.lock().unwrap().insert(o.key, bytes);
        }
    }
    let sink = Collect(Mutex::new(BTreeMap::new()));
    let root = match ingest_path(std::path::Path::new(&path), &no_filter, meta, &sink) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    let out = std::io::stdout();
    let mut out = std::io::BufWriter::new(out.lock());
    for (k, bytes) in sink.0.into_inner().unwrap() {
        match bytes {
            None => writeln!(out, "{k} Blob {}", k.length()),
            Some(b) => writeln!(
                out,
                "{k} {} {} {}",
                k.type_(),
                k.length(),
                ietsp::store::hash::base16_encode(&b)
            ),
        }
        .unwrap();
    }
    match root {
        ietsp::amber::tree::Root::Dir(k) => writeln!(out, "root {k}"),
        ietsp::amber::tree::Root::File { key, executable } => {
            writeln!(
                out,
                "root {key} {}",
                if executable { "0555" } else { "0444" }
            )
        }
        ietsp::amber::tree::Root::Symlink(t) => {
            writeln!(out, "root link {}", String::from_utf8_lossy(&t))
        }
    }
    .unwrap();
    0
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = argv.first() else { usage() };
    match cmd.as_str() {
        "build" | "eval" | "eval-jobs" | "print-dev-env" => {
            let rest: Vec<String> = argv[1..].to_vec();
            let cmd = cmd.clone();
            let code = std::thread::Builder::new()
                .stack_size(STACK_SIZE)
                .spawn(move || match cmd.as_str() {
                    "build" => cmd_build(&rest),
                    "eval-jobs" => cmd_eval_jobs(&rest),
                    "print-dev-env" => cmd_print_dev_env(&rest),
                    _ => run_eval(parse_eval_args(&rest)),
                })
                .expect("spawn evaluator thread")
                .join()
                .unwrap_or(1);
            exit(code)
        }
        "cache-dump" if argv.len() == 2 => exit(cmd_cache_dump(&argv[1])),
        "amber-keys" => exit(cmd_amber_keys(&argv[1..])),
        "parse" if argv.len() > 1 => {
            let rest = &argv[1..];
            let quiet = rest[0] == "-q";
            let files = if quiet { &rest[1..] } else { rest };
            let files: Vec<String> = if files == ["-"] {
                std::io::read_to_string(std::io::stdin())
                    .unwrap_or_default()
                    .lines()
                    .map(str::to_owned)
                    .collect()
            } else {
                files.to_vec()
            };
            exit(cmd_parse(&files, quiet))
        }
        _ => usage(),
    }
}

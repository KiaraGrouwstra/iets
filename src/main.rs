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
        "usage: ietsp eval [--strict] [--json] [--workers N] [--timings] [--no-cache] [--cache-dir DIR] [-I prefix=path]... [--arg name expr] [--argstr name str] [-A attr] [--dump-drvs DIR] (-E EXPR | FILE)\n       ietsp parse [-q] FILE...   (FILE may be - to read a list from stdin)\n       ietsp cache-dump READSET   (print a cache entry's read set)\n       ietsp build [--builder URL] [--timings] [--no-build] [--fetch-to DIR] [--no-cache] [--cache-dir DIR] [--out NAME]... [--workers N] [-I prefix=path]... [--arg name expr] [--argstr name str] [-A attr] (-E EXPR | FILE)\n       ietsp build [--builder URL] [--timings] [--fetch-to DIR] [--out NAME]... [--source STOREPATH=LOCAL]... DRV\n       URL: http://host:port (gRPC), daemon, unix:///path, ssh-ng://[user@]host (Nix worker protocol)"
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
}

enum ArgKind {
    Expr(String),
    Str(String),
}

enum Input {
    Expr(String),
    File(String),
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
    };
    let mut i = 0;
    while i < argv.len() {
        let s = argv[i].as_str();
        match s {
            "--strict" => a.strict = true,
            "--json" => a.json = true,
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
            _ => a.input = Some(Input::File(s.to_string())),
        }
        i += 1;
    }
    a
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
        None => usage(),
    };
    if !a.args.is_empty() {
        let mut entries = Vec::new();
        for (name, kind) in &a.args {
            let w = match kind {
                ArgKind::Expr(e) => {
                    let ir = ev.compile_source(
                        b"\xc2\xabarg\xc2\xbb",
                        e.as_bytes().to_vec(),
                        &cwd,
                        None,
                    )?;
                    let root = ev.root_env;
                    ev.arena.mk_thunk(root, ir)
                }
                ArgKind::Str(s) => ev.mk_string(s.as_bytes()),
            };
            let sym = ev.sym(name.as_bytes());
            entries.push((sym, Pos::NONE, w));
        }
        let args = ev.mk_attrs(&mut entries, Pos::NONE)?;
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
    let r = if json {
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
            out.push(b'\n');
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

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = argv.first() else { usage() };
    match cmd.as_str() {
        "build" | "eval" => {
            let rest: Vec<String> = argv[1..].to_vec();
            let is_build = cmd == "build";
            let code = std::thread::Builder::new()
                .stack_size(STACK_SIZE)
                .spawn(move || {
                    if is_build {
                        cmd_build(&rest)
                    } else {
                        run_eval(parse_eval_args(&rest))
                    }
                })
                .expect("spawn evaluator thread")
                .join()
                .unwrap_or(1);
            exit(code)
        }
        "cache-dump" if argv.len() == 2 => exit(cmd_cache_dump(&argv[1])),
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

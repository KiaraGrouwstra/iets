//! The `ietsp` command line: argument parsing shared by the subcommands and
//! one module per subcommand.

pub mod build;
pub mod cache_dump;
pub mod classic;
pub mod devenv;
pub mod eval;
pub mod jobs;
pub mod parse;

use std::process::exit;

use crate::eval::{Error, Evaluator, Pos};
use crate::runtime::Word;
use crate::trace::run::Cache;

pub fn usage() -> ! {
    eprintln!(
        "usage: ietsp eval [--strict] [--json] [--raw] [--workers N] [--timings] [--no-cache] [--no-ir-cache] [--cache-dir DIR] [-I prefix=path]... [--arg name expr] [--argstr name str] [-A attr] [--dump-drvs DIR] (-E EXPR | FILE)\n       ietsp parse [-q] FILE...   (FILE may be - to read a list from stdin)\n       ietsp cache-dump READSET   (print a cache entry's read set)\n       ietsp build [--builder URL] [--timings] [--no-build] [--fetch-to DIR] [--no-cache] [--cache-dir DIR] [--out NAME]... [--workers N] [-I prefix=path]... [--arg name expr] [--argstr name str] [-A attr] (-E EXPR | FILE)\n       ietsp build [--builder URL] [--timings] [--fetch-to DIR] [--out NAME]... [--source STOREPATH=LOCAL]... DRV\n       ietsp eval-jobs [--workers N] [--force-recurse] [--meta] [--provenance] [--show-input-drvs] [--chunks M --chunk N [--chunk-depth D]] [--max-memory-size MB] [-I prefix=path]... [--arg name expr] [--argstr name str] (-E EXPR | FILE | FLAKEREF#attr)\n       ietsp print-dev-env [--builder URL] [--gc-root PATH] [--json] [--workers N] [--no-cache] [--cache-dir DIR] [--timings] [FLAKEREF[#attr]]   (default: ., a daemon URL)\n       URL: daemon, unix:///path, ssh-ng://[user@]host (Nix worker protocol)"
    );
    exit(2)
}

pub struct EvalArgs {
    pub strict: bool,
    pub json: bool,
    pub raw: bool,
    pub includes: Vec<(Vec<u8>, Vec<u8>)>,
    pub args: Vec<(String, ArgKind)>,
    pub input: Option<Input>,
    pub dump_drvs: Option<String>,
    pub attr: Option<String>,
    pub lint_absolute: crate::compile::Lint,
    pub lint_short: crate::compile::Lint,
    pub xp_features: Vec<String>,
    pub workers: usize,
    pub timings: bool,
    pub no_cache: bool,
    /// `--no-ir-cache` or `IETSP_CACHE=0`: compile every import from source.
    pub no_ir_cache: bool,
    pub cache_dir: Option<String>,
    /// `eval-jobs --flake DIR` with no fragment: the whole `callFlake` result,
    /// not its `outputs`.
    pub flake_whole: bool,
}

pub enum ArgKind {
    Expr(String),
    Str(String),
}

pub enum Input {
    Expr(String),
    File(String),
    /// `DIR#attr.path`, see `crate::flake`.
    Flake(String),
}

pub fn parse_include(s: &str) -> (Vec<u8>, Vec<u8>) {
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
        crate::compile::canon_path(&cwd)
    };
    (prefix, abs)
}

pub fn parse_eval_args(argv: &[String]) -> EvalArgs {
    let mut a = EvalArgs {
        strict: false,
        json: false,
        raw: false,
        includes: Vec::new(),
        args: Vec::new(),
        input: None,
        dump_drvs: None,
        attr: None,
        lint_absolute: crate::compile::Lint::Ignore,
        lint_short: crate::compile::Lint::Ignore,
        xp_features: Vec::new(),
        workers: crate::parallel::default_workers(),
        timings: false,
        no_cache: std::env::var_os("IETSP_CACHE").is_some_and(|v| v == "0"),
        no_ir_cache: std::env::var_os("IETSP_CACHE").is_some_and(|v| v == "0"),
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
            "--no-ir-cache" => a.no_ir_cache = true,
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
                let Some(mode) = crate::compile::Lint::parse(v) else {
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
            _ if crate::flake::is_flake_ref(s) => a.input = Some(Input::Flake(s.to_string())),
            _ => a.input = Some(Input::File(s.to_string())),
        }
        i += 1;
    }
    a
}

fn home() -> Option<Vec<u8>> {
    std::env::var_os("HOME").map(|h| h.into_encoded_bytes())
}

pub fn flake_ref(f: &str, cwd: &[u8]) -> crate::error::Result<crate::flake::FlakeRef> {
    crate::flake::parse_flake_ref(f, cwd, home().as_deref())
}

/// Prints an evaluation error. Positions and the store paths inside the
/// messages name the local file the source was copied from, when one is
/// known (docs/provenance.md).
pub fn report(ev: &Evaluator, e: &Error) {
    use crate::provenance::{describe_pos, rewrite_msg};
    let at = |pos: Pos| describe_pos(ev, pos).unwrap_or_else(|| ev.describe_pos(pos));
    eprintln!("error: {}", rewrite_msg(ev, &e.msg));
    if !e.pos.is_none() {
        eprintln!("       at {}", at(e.pos));
    }
    for (pos, msg) in e.trace.iter() {
        let msg = rewrite_msg(ev, msg);
        if pos.is_none() {
            eprintln!("       … {msg}");
        } else {
            eprintln!("       … {msg} at {}", at(*pos));
        }
    }
}

/// Applies the shared `eval` settings and evaluates the input, `--arg`s and `-A` included.
pub fn evaluate(ev: &mut Evaluator, a: EvalArgs) -> Result<Word, Box<Error>> {
    ev.compile_settings.lint_absolute_path_literals = a.lint_absolute;
    ev.compile_settings.lint_short_path_literals = a.lint_short;
    if !a.no_ir_cache {
        ev.ir_cache =
            crate::compile::cache::IrCache::locate(a.cache_dir.as_deref()).map(std::sync::Arc::new);
    }
    if a.xp_features.iter().any(|f| f == "parse-toml-timestamps") {
        crate::builtins::text::set_parse_toml_timestamps(true);
    }
    crate::builtins::set_search_path(ev, full_includes(&a.includes));
    if let Some(dir) = &a.dump_drvs {
        std::fs::create_dir_all(dir).map_err(|e| {
            Error::boxed(
                crate::eval::ErrorKind::Io,
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
            let fr = flake_ref(&f, &cwd).map_err(|m| {
                Error::boxed(crate::eval::ErrorKind::Eval, m.to_string(), Pos::NONE)
            })?;
            if a.flake_whole && fr.attr.is_empty() {
                crate::flake::eval_flake_value(ev, &fr)?
            } else {
                crate::flake::eval_flake(ev, &fr)?
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
                    crate::eval::ErrorKind::MissingAttr,
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
pub fn auto_args(
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

pub fn cwd() -> Vec<u8> {
    std::env::current_dir()
        .map(|p| p.into_os_string().into_encoded_bytes())
        .unwrap_or_else(|_| b"/".to_vec())
}

pub fn abs_path(f: &str, cwd: &[u8]) -> Vec<u8> {
    let p = f.as_bytes();
    if p.starts_with(b"/") {
        return p.to_vec();
    }
    let mut full = cwd.to_vec();
    full.push(b'/');
    full.extend_from_slice(p);
    crate::compile::canon_path(&full)
}

/// `-I` entries first, then `NIX_PATH`, as absolute `(prefix, path)` pairs.
pub fn full_includes(includes: &[(Vec<u8>, Vec<u8>)]) -> Vec<(Vec<u8>, Vec<u8>)> {
    let mut all = includes.to_vec();
    if let Ok(np) = std::env::var("NIX_PATH") {
        for entry in np.split(':').filter(|s| !s.is_empty()) {
            all.push(parse_include(entry));
        }
    }
    all
}

/// The tracing cache for this run, keyed on the eval arguments. `None` when the
/// cache is off or the command cannot be keyed.
pub fn setup_cache(a: &EvalArgs, subcommand: &str, ev: &Evaluator) -> Option<Cache> {
    use crate::trace::key::{Key, has_relative_path};
    if a.no_cache || a.dump_drvs.is_some() {
        return None;
    }
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
            k.field("file-hash", &crate::trace::sha256(&bytes));
        }
        Input::Flake(f) => {
            let fr = flake_ref(f, &cwd).ok()?;
            k.field("flake", f.as_bytes());
            k.field("flake-dir", fr.dir.as_os_str().as_encoded_bytes());
            for name in ["flake.nix", "flake.lock"] {
                let bytes = std::fs::read(fr.dir.join(name)).ok()?;
                k.field(name, &crate::trace::sha256(&bytes));
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
    Cache::open(a.cache_dir.as_deref(), k, a.timings)
}

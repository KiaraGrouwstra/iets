//! The entry points a frontend needs: evaluation arguments, the evaluate
//! call, error reporting, the build target list and the cache key. The
//! `ietsp` command line is one frontend on top of this; docs/library.md
//! lists the whole surface.

pub mod value;

use crate::eval::{Error, Evaluator, Pos};
use crate::runtime::Word;
use crate::store::path::StorePath;
use crate::trace::run::Cache;

/// Everything one evaluation needs. `EvalArgs::new()` holds the defaults;
/// `crate::cli::parse_eval_args` fills it from a command line.
pub struct EvalArgs {
    pub strict: bool,
    pub json: bool,
    pub raw: bool,
    pub includes: Vec<(Vec<u8>, Vec<u8>)>,
    pub args: Vec<(String, ArgKind)>,
    pub input: Option<Input>,
    pub dump_drvs: Option<String>,
    pub attr: Option<String>,
    /// `--lint SELECTOR=LEVEL` in order; applied after `ietsp.toml` and
    /// `IETSP_LINT` (docs/lints.md).
    pub lints: Vec<(String, crate::lint::Level)>,
    /// `--lint-root DIR`: lint roots after the entry directory.
    pub lint_roots: Vec<String>,
    /// `--lint-scope root|all`; `None` keeps `ietsp.toml`'s or the default.
    pub lint_scope: Option<crate::lint::Scope>,
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
    /// The last of `--pure-eval`, `--no-pure-eval`, `--impure` and `--option
    /// pure-eval`; `None` when none was given (docs/pure-eval.md).
    pub pure: Option<bool>,
}

impl Default for EvalArgs {
    fn default() -> EvalArgs {
        let cache = crate::session::Session::current().settings().cache;
        EvalArgs {
            strict: false,
            json: false,
            raw: false,
            includes: Vec::new(),
            args: Vec::new(),
            input: None,
            dump_drvs: None,
            attr: None,
            lints: Vec::new(),
            lint_roots: Vec::new(),
            lint_scope: None,
            xp_features: Vec::new(),
            workers: crate::parallel::default_workers(),
            timings: false,
            no_cache: !cache,
            no_ir_cache: !cache,
            cache_dir: None,
            flake_whole: false,
            pure: None,
        }
    }
}

impl EvalArgs {
    pub fn new() -> EvalArgs {
        EvalArgs::default()
    }

    /// The evaluator these arguments ask for: `pure-eval` fixes its builtins.
    pub fn evaluator(&self) -> Evaluator {
        Evaluator::with(crate::eval::DEFAULT_SYSTEM, self.pure == Some(true))
    }
}

#[derive(Clone)]
pub enum ArgKind {
    Expr(String),
    Str(String),
}

#[derive(Clone)]
pub enum Input {
    Expr(String),
    File(String),
    /// `DIR#attr.path`, see `crate::flake`.
    Flake(String),
}

/// A `-I prefix=path` (or `-I path`) entry as an absolute pair.
pub fn parse_include(s: &str) -> (Vec<u8>, Vec<u8>) {
    let (prefix, path) = s.split_once('=').unwrap_or(("", s));
    (crate::argv::bytes(prefix), abs_path(path, &cwd()))
}

fn home() -> Option<Vec<u8>> {
    std::env::var_os("HOME").map(|h| h.into_encoded_bytes())
}

pub fn flake_ref(f: &str, cwd: &[u8]) -> crate::error::Result<crate::flake::FlakeRef> {
    crate::flake::parse_flake_ref(f, cwd, home().as_deref())
}

/// An evaluation error as a value a host can render itself. Positions and
/// the store paths inside the messages name the local file the source was
/// copied from, when one is known (docs/provenance.md).
pub fn report_of(ev: &Evaluator, e: &Error) -> crate::log::Report {
    use crate::provenance::{describe_pos, rewrite_msg};
    let at = |pos: Pos| describe_pos(ev, pos).unwrap_or_else(|| ev.describe_pos(pos));
    ev.lints.flush();
    // a lint message resolves its own paths (lint::Evaluator::lint_shown_path)
    let msg = if e.kind == crate::eval::ErrorKind::Lint {
        e.msg.clone()
    } else {
        rewrite_msg(ev, &e.msg)
    };
    // "ietsp: " marks what classic mode falls back on; here it only doubles "error: "
    let msg = msg.strip_prefix("ietsp: ").unwrap_or(&msg);
    crate::log::Report::eval(e, msg, &at, &|m| rewrite_msg(ev, m))
}

/// The same report, sent to the session's log, with the exit code.
pub fn report(ev: &Evaluator, e: &Error) -> i32 {
    report_of(ev, e).emit()
}

/// The directory the `ietsp.toml` walk starts from: the entry file's
/// directory, the flake directory, or cwd for an expression.
pub fn entry_dir(a: &EvalArgs) -> std::path::PathBuf {
    use std::os::unix::ffi::OsStrExt;
    let cwd = cwd();
    let bytes = match &a.input {
        Some(Input::File(f)) => crate::eval::dir_of(&abs_path(f, &cwd)).to_vec(),
        Some(Input::Flake(f)) => match flake_ref(f, &cwd) {
            Ok(fr) => return fr.dir,
            Err(_) => cwd,
        },
        _ => cwd,
    };
    std::path::PathBuf::from(std::ffi::OsStr::from_bytes(&bytes))
}

/// The lint configuration of a run (docs/lints.md).
pub fn lint_config(a: &EvalArgs) -> Result<crate::lint::Config, Box<Error>> {
    let cwd = cwd();
    let roots: Vec<Vec<u8>> = a.lint_roots.iter().map(|r| abs_path(r, &cwd)).collect();
    crate::lint::Config::resolve(Some(&entry_dir(a)), &a.lints, &roots, a.lint_scope)
        .map_err(|m| Error::boxed(crate::eval::ErrorKind::Eval, m, Pos::NONE))
}

/// Installs the lint configuration. Call before `parallel::try_run`: helper
/// fibers clone the evaluator then, and every fiber must lint alike.
/// `evaluate` calls it too, so a frontend that runs sequentially may skip it.
pub fn configure_lints(ev: &mut Evaluator, a: &EvalArgs) -> Result<(), Box<Error>> {
    let c = lint_config(a)?;
    if ev.lints.config != c {
        ev.lints = crate::lint::Lints::new(c);
    }
    Ok(())
}

/// Applies the shared `eval` settings and evaluates the input, `--arg`s and `-A` included.
pub fn evaluate(ev: &mut Evaluator, a: EvalArgs) -> Result<Word, Box<Error>> {
    configure_lints(ev, &a)?;
    if !a.no_ir_cache {
        ev.ir_cache = crate::compile::cache::IrCache::locate(a.cache_dir.as_deref(), a.timings)
            .map(std::sync::Arc::new);
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
                let kind = match m {
                    crate::error::Error::Unsupported(_) => crate::eval::ErrorKind::Unsupported,
                    _ => crate::eval::ErrorKind::Eval,
                };
                Error::boxed(kind, m.to_string(), Pos::NONE)
            })?;
            if a.flake_whole && fr.attr.is_empty() {
                crate::flake::eval_flake_value(ev, &fr)?
            } else {
                crate::flake::eval_flake(ev, &fr)?
            }
        }
        None => {
            return Err(Error::boxed(
                crate::eval::ErrorKind::Eval,
                "nothing to evaluate: no expression, file or flake",
                Pos::NONE,
            ));
        }
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
                    ev.compile_source(b"\xc2\xabarg\xc2\xbb", crate::argv::bytes(e), cwd, None)?;
                let root = ev.root_env;
                ev.arena.mk_thunk(root, ir)
            }
            ArgKind::Str(s) => ev.mk_string(&crate::argv::bytes(s)),
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

/// `f` is in `crate::argv`'s encoding.
pub fn abs_path(f: &str, cwd: &[u8]) -> Vec<u8> {
    let p = crate::argv::bytes(f);
    if p.starts_with(b"/") {
        return p;
    }
    let mut full = cwd.to_vec();
    full.push(b'/');
    full.extend_from_slice(&p);
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

/// The derivations to build and the outputs to print: `wanted` plus the
/// outputs the roots' string context named.
pub fn build_targets(
    wanted: &[String],
    roots: &[(StorePath, Option<String>)],
) -> (Vec<StorePath>, Vec<String>) {
    let mut outputs = wanted.to_vec();
    for (_, out) in roots {
        if let Some(o) = out
            && !outputs.contains(o)
        {
            outputs.push(o.clone());
        }
    }
    (roots.iter().map(|(p, _)| p.clone()).collect(), outputs)
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
    if a.pure == Some(true) {
        k.field("pure-eval", b"1");
    }
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
    k.field("lints", lint_config(a).ok()?.key().as_bytes());
    for f in &a.xp_features {
        k.field("xp", f.as_bytes());
    }
    k.field("system", ev.current_system.as_bytes());
    if needs_cwd {
        k.field("cwd", &cwd);
    }
    Cache::open(a.cache_dir.as_deref(), k, a.timings)
}

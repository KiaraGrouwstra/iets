//! Classic mode: ietsp installed under Nix's own command names, with an exec
//! fallback to the real binary for everything it does not implement.
//! See `docs/reference/classic.md`.

pub mod build;
pub mod develop;
pub mod instantiate;
pub mod nix;
pub mod shell;

use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, exit};

use crate::cli::{EvalFlags, abs_path, eval_flag, full_includes};
use crate::config::Config;
use crate::eval::{Error, ErrorKind, EvalResult, Evaluator, Pos};
use crate::remote::worker::{DEFAULT_SOCKET, Options, Transport};
use crate::runtime::Word;

/// The command names ietsp answers to when it is installed as classic Nix.
pub const NAMES: &[&str] = &[
    "nix-instantiate",
    "nix-build",
    "nix-eval-jobs",
    "nix-shell",
    "nix-fast-build",
    "nix",
];

/// Runs the classic command `name`. `argv` is everything after argv[0].
pub fn dispatch(name: &str, argv: &[String]) -> i32 {
    match name {
        "nix-instantiate" => instantiate::main(argv),
        "nix-build" => build::main(argv),
        "nix-shell" => shell::main(argv),
        "nix-eval-jobs" => crate::cli::jobs::cmd_eval_jobs(argv),
        "nix-fast-build" => crate::cli::fastbuild::cmd_fast_build(argv),
        _ => nix::main(argv),
    }
}

/// Execs the real `name` with the same arguments. `IETSP_NO_FALLBACK=1` makes
/// this an error instead, so a test can see which cases fall back.
pub fn fallback(name: &str, argv: &[String], reason: &str) -> ! {
    crate::log::line(&format!("ietsp: falling back to {name}: {reason}"));
    if crate::session::Session::current().settings().no_fallback {
        crate::log::line(&format!("ietsp: IETSP_NO_FALLBACK=1, not running {name}"));
        exit(1)
    }
    let Some(real) = crate::nixbin::real(name) else {
        eprintln!("ietsp: cannot fall back to {name}: not found on PATH");
        exit(1)
    };
    let e = Command::new(&real)
        .arg0(name)
        .args(argv.iter().map(|a| crate::argv::os(a)))
        .exec();
    eprintln!("ietsp: cannot exec '{}': {e}", real.display());
    exit(1)
}

/// The flags `nix-instantiate` and `nix-build` share (`MixEvalArgs` plus the
/// settings aliases of `LegacyArgs`).
#[derive(Default)]
pub struct Common {
    pub flags: EvalFlags,
    pub files: Vec<String>,
    /// `-E`: the positional arguments are expressions, not paths.
    pub from_args: bool,
    pub quiet: bool,
}

/// A cursor over the command line; a flag without its value falls back, because
/// the real nix reports that better than a second usage message would.
pub struct Argv<'a> {
    pub all: &'a [String],
    pub i: usize,
}

impl Argv<'_> {
    pub fn value(&mut self, flag: &str) -> Result<String, String> {
        self.i += 1;
        self.all
            .get(self.i)
            .cloned()
            .ok_or_else(|| format!("'{flag}' requires an argument"))
    }

    fn number(&mut self, flag: &str) -> Result<String, String> {
        let v = self.value(flag)?;
        v.parse::<u64>()
            .map(|n| n.to_string())
            .map_err(|_| format!("'{flag}' wants a number, got '{v}'"))
    }
}

/// `Ok(true)`: consumed. `Ok(false)`: not one of ours. `Err`: fall back.
pub fn common_flag(c: &mut Common, a: &mut Argv, s: &str) -> Result<bool, String> {
    if eval_flag(&mut c.flags, s, || a.value(s))? {
        return Ok(true);
    }
    let set = &mut c.flags.overrides;
    match s {
        "-E" | "--expr" => c.from_args = true,
        "--store" => set.push(("store".into(), a.value(s)?)),
        "-j" | "--max-jobs" => set.push(("max-jobs".into(), a.number(s)?)),
        "--cores" => set.push(("cores".into(), a.number(s)?)),
        "-k" | "--keep-going" => set.push(("keep-going".into(), "true".into())),
        "-K" | "--keep-failed" => set.push(("keep-failed".into(), "true".into())),
        "--quiet" => c.quiet = true,
        "-v" | "--verbose" | "--no-gc-warning" | "--indirect" => {}
        _ => return Ok(false),
    }
    Ok(true)
}

/// Where the builds go, from the `store` setting. `system` is the `system=`
/// default a `grpc://` URL takes.
pub fn transport_of(store: &str, system: &str) -> Result<Transport, String> {
    match store {
        "auto" | "" | "daemon" => Ok(Transport::Unix(PathBuf::from(DEFAULT_SOCKET))),
        s if s.starts_with("unix://") || s.starts_with("ssh-ng://") || s.starts_with("grpc://") => {
            Transport::parse_checked(s, system)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| format!("cannot parse the store URL '{s}'"))
        }
        s => Err(format!(
            "store '{s}' is not supported; ietsp speaks the worker protocol and grpc:// only"
        )),
    }
}

/// `Config::load` plus the transport and `SetOptions` values it implies. The
/// build target is `--store` or `--option store` when the command line gave
/// one, else the `builder` setting, else nix.conf's `store` (docs/reference/settings.md).
pub fn settings(c: &Common) -> Result<(Config, Transport, Options), String> {
    settings_with(c, true)
}

/// `settings` for a shell, which runs here and so needs its inputs in the
/// local store: the `builder` setting is skipped; `--store` still wins.
pub fn shell_settings(c: &Common) -> Result<(Config, Transport, Options), String> {
    settings_with(c, false)
}

fn settings_with(c: &Common, use_builder: bool) -> Result<(Config, Transport, Options), String> {
    let cfg = Config::load(&c.flags.overrides).map_err(|e| e.to_string())?;
    let url = if use_builder {
        let flag = store_given(c).then(|| cfg.store());
        crate::settings::builder(flag.as_deref(), &cfg)
    } else {
        cfg.store()
    };
    let transport = transport_of(&url, &cfg.system())?;
    let options = Options::try_from(&cfg).map_err(|e| e.to_string())?;
    Ok((cfg, transport, options))
}

/// `--store` names the store the outputs belong to. Without it the builder
/// stands in for the local store, as a build hook does, so outputs come back
/// even under `--no-link`.
pub fn store_given(c: &Common) -> bool {
    c.flags.overrides.iter().any(|(k, _)| k == "store")
}

/// The local daemon, where GC roots, out-links and `--add-root` registrations
/// go whatever `builder` says (docs/reference/classic.md).
pub fn local() -> Transport {
    Transport::Unix(PathBuf::from(DEFAULT_SOCKET))
}

/// An evaluator with the search path, the `system` setting and `pure-eval`
/// applied.
pub fn evaluator(c: &Common, cfg: &Config) -> Evaluator {
    let pure = c
        .flags
        .pure
        .unwrap_or_else(|| cfg.bool("pure-eval").ok().flatten().unwrap_or(false));
    let mut ev = Evaluator::with(&cfg.system(), pure);
    crate::builtins::set_search_path(&mut ev, full_includes(&c.flags.includes));
    // lints stay off, as in nix; the root alone feeds the provenance fallback.
    let cwd = crate::api::cwd();
    let entry = match (c.from_args, c.files.first()) {
        (false, Some(f)) => crate::eval::dir_of(&abs_path(f, &cwd)).to_vec(),
        _ => cwd,
    };
    ev.lints.config.add_root(&entry);
    ev
}

/// `lookupFileArg`: `<name>` goes through the search path, anything else is a
/// path relative to the working directory.
pub fn lookup_file_arg(ev: &mut Evaluator, f: &str, cwd: &[u8]) -> EvalResult<Vec<u8>> {
    if f.len() > 2 && f.starts_with('<') && f.ends_with('>') {
        let entries = ev.search_path.clone();
        return ev.find_file(&entries, &f.as_bytes()[1..f.len() - 1], Pos::NONE);
    }
    Ok(abs_path(f, cwd))
}

/// One of the positional arguments, evaluated: an expression under `-E`,
/// otherwise a file (a directory resolves to its `default.nix`).
pub fn eval_input(ev: &mut Evaluator, c: &Common, file: &str, cwd: &[u8]) -> EvalResult<Word> {
    if c.from_args {
        ev.eval_string(file, cwd)
    } else {
        let p = lookup_file_arg(ev, file, cwd)?;
        ev.eval_file(&p, Pos::NONE)
    }
}

/// attr-path.cc:findAlongAttrPath. Auto-calls at every step, selects an
/// attribute by name or a list element by index.
pub fn select(ev: &mut Evaluator, root: Word, attr_path: &str, args: Word) -> EvalResult<Word> {
    let mut v = root;
    let path = crate::api::flake::parse_attr_path(attr_path)
        .map_err(|e| Error::boxed(ErrorKind::Parse, e.to_string(), Pos::NONE))?;
    for attr in path {
        v = ev.auto_call(v, args)?;
        v = ev.force(v)?;
        match attr.parse::<u32>() {
            Ok(idx) => {
                let Some(l) = v.as_list() else {
                    return Err(Error::boxed(
                        ErrorKind::Type,
                        format!(
                            "the expression selected by the selection path '{attr_path}' should be a list"
                        ),
                        Pos::NONE,
                    ));
                };
                let cells = l.cells();
                let Some(cell) = cells.get(idx as usize) else {
                    return Err(Error::boxed(
                        ErrorKind::MissingAttr,
                        format!("list index {idx} in selection path '{attr_path}' is out of range"),
                        Pos::NONE,
                    ));
                };
                v = cell.get();
            }
            Err(_) => {
                let Some(attrs) = v.as_attrs() else {
                    return Err(Error::boxed(
                        ErrorKind::Type,
                        format!(
                            "the expression selected by the selection path '{attr_path}' should be a set"
                        ),
                        Pos::NONE,
                    ));
                };
                if attr.is_empty() {
                    return Err(Error::boxed(
                        ErrorKind::Eval,
                        format!("empty attribute name in selection path '{attr_path}'"),
                        Pos::NONE,
                    ));
                }
                let sym = ev.sym(attr.as_bytes());
                let Some(a) = attrs.get(sym) else {
                    return Err(Error::boxed(
                        ErrorKind::MissingAttr,
                        format!("attribute '{attr}' in selection path '{attr_path}' not found"),
                        Pos::NONE,
                    ));
                };
                v = a.cell.get();
            }
        }
    }
    Ok(v)
}

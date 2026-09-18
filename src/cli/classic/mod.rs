//! Classic mode: ietsp installed under Nix's own command names, with an exec
//! fallback to the real binary for everything it does not implement.
//! See `docs/classic.md`.

pub mod build;
pub mod develop;
pub mod instantiate;
pub mod nix;

use std::collections::HashSet;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, exit};

use crate::cli::{ArgKind, abs_path, full_includes};
use crate::config::Config;
use crate::eval::{Error, ErrorKind, EvalResult, Evaluator, Pos};
use crate::remote::worker::{DEFAULT_SOCKET, Options, Transport};
use crate::runtime::Word;
use crate::store::path::StorePath;

/// The command names ietsp answers to when it is installed as classic Nix.
pub const NAMES: &[&str] = &["nix-instantiate", "nix-build", "nix-eval-jobs", "nix"];

/// Runs the classic command `name`. `argv` is everything after argv[0].
pub fn dispatch(name: &str, argv: &[String]) -> i32 {
    match name {
        "nix-instantiate" => instantiate::main(argv),
        "nix-build" => build::main(argv),
        "nix-eval-jobs" => crate::cli::jobs::cmd_eval_jobs(argv),
        _ => nix::main(argv),
    }
}

/// Execs the real `name` with the same arguments. `IETSP_NO_FALLBACK=1` makes
/// this an error instead, so a test can see which cases fall back.
pub fn fallback(name: &str, argv: &[String], reason: &str) -> ! {
    eprintln!("ietsp: falling back to {name}: {reason}");
    if std::env::var_os("IETSP_NO_FALLBACK").is_some_and(|v| v == "1") {
        eprintln!("ietsp: IETSP_NO_FALLBACK=1, not running {name}");
        exit(1)
    }
    let Some(real) = crate::nixbin::real(name) else {
        eprintln!("ietsp: cannot fall back to {name}: not found on PATH");
        exit(1)
    };
    let e = Command::new(&real).arg0(name).args(argv).exec();
    eprintln!("ietsp: cannot exec '{}': {e}", real.display());
    exit(1)
}

/// An evaluation error that classic mode answers with the real nix.
pub fn is_unsupported(e: &Error) -> bool {
    e.kind == ErrorKind::Unsupported
}

/// The flags `nix-instantiate` and `nix-build` share (`MixEvalArgs` plus the
/// settings aliases of `LegacyArgs`).
#[derive(Default)]
pub struct Common {
    pub files: Vec<String>,
    /// `-E`: the positional arguments are expressions, not paths.
    pub from_args: bool,
    pub attr_paths: Vec<String>,
    pub args: Vec<(String, ArgKind)>,
    pub includes: Vec<(Vec<u8>, Vec<u8>)>,
    /// `--option name value` and the settings the flags alias, in flag order.
    pub overrides: Vec<(String, String)>,
    pub show_trace: bool,
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
    match s {
        "-E" | "--expr" => c.from_args = true,
        "-A" | "--attr" => {
            let v = a.value(s)?;
            c.attr_paths.push(v);
        }
        "--arg" | "--argstr" => {
            let name = a.value(s)?;
            let val = a.value(s)?;
            c.args.push((
                name,
                if s == "--arg" {
                    ArgKind::Expr(val)
                } else {
                    ArgKind::Str(val)
                },
            ));
        }
        "-I" | "--include" => {
            let v = a.value(s)?;
            c.includes.push(crate::cli::parse_include(&v));
        }
        "--option" => {
            let name = a.value(s)?;
            let val = a.value(s)?;
            c.overrides.push((name, val));
        }
        "--store" => {
            let v = a.value(s)?;
            c.overrides.push(("store".into(), v));
        }
        "-j" | "--max-jobs" => {
            let v = a.number(s)?;
            c.overrides.push(("max-jobs".into(), v));
        }
        "--cores" => {
            let v = a.number(s)?;
            c.overrides.push(("cores".into(), v));
        }
        "-k" | "--keep-going" => c.overrides.push(("keep-going".into(), "true".into())),
        "-K" | "--keep-failed" => c.overrides.push(("keep-failed".into(), "true".into())),
        "--show-trace" => c.show_trace = true,
        "--quiet" => c.quiet = true,
        "-v" | "--verbose" | "--no-gc-warning" | "--indirect" => {}
        _ if s.starts_with("-I") && s.len() > 2 => {
            c.includes.push(crate::cli::parse_include(&s[2..]));
        }
        _ => return Ok(false),
    }
    Ok(true)
}

/// Where the worker protocol goes, from the `store` setting.
pub fn transport_of(store: &str) -> Result<Transport, String> {
    match store {
        "auto" | "" | "daemon" => Ok(Transport::Unix(PathBuf::from(DEFAULT_SOCKET))),
        s if s.starts_with("unix://") || s.starts_with("ssh-ng://") => {
            Transport::parse(s).ok_or_else(|| format!("cannot parse the store URL '{s}'"))
        }
        s => Err(format!(
            "store '{s}' is not supported; ietsp speaks the worker protocol only"
        )),
    }
}

/// `Config::load` plus the transport and `SetOptions` values it implies.
pub fn settings(c: &Common) -> Result<(Config, Transport, Options), String> {
    let cfg = Config::load(&c.overrides).map_err(|e| e.to_string())?;
    let transport = transport_of(&cfg.store())?;
    let options = Options::try_from(&cfg).map_err(|e| e.to_string())?;
    Ok((cfg, transport, options))
}

/// An evaluator with the search path and the `system` setting applied.
pub fn evaluator(c: &Common, cfg: &Config) -> Evaluator {
    let mut ev = Evaluator::new();
    let system = cfg.system();
    if system != ev.current_system {
        ev.current_system = Box::leak(system.into_boxed_str());
    }
    crate::builtins::set_search_path(&mut ev, full_includes(&c.includes));
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

/// attr-path.cc:parseAttrPath. `"a.b"` quoting keeps a dot inside a component.
pub fn parse_attr_path(s: &str) -> EvalResult<Vec<String>> {
    let mut res = Vec::new();
    let mut cur = String::new();
    let mut it = s.chars();
    while let Some(ch) = it.next() {
        match ch {
            '.' => res.push(std::mem::take(&mut cur)),
            '"' => loop {
                match it.next() {
                    None => {
                        return Err(Error::boxed(
                            ErrorKind::Parse,
                            format!("missing closing quote in selection path '{s}'"),
                            Pos::NONE,
                        ));
                    }
                    Some('"') => break,
                    Some(c) => cur.push(c),
                }
            },
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        res.push(cur);
    }
    Ok(res)
}

/// attr-path.cc:findAlongAttrPath. Auto-calls at every step, selects an
/// attribute by name or a list element by index.
pub fn select(ev: &mut Evaluator, root: Word, attr_path: &str, args: Word) -> EvalResult<Word> {
    let mut v = root;
    for attr in parse_attr_path(attr_path)? {
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

/// One derivation `nix-instantiate` or `nix-build` was asked for: the drv and
/// the single output `outputName` names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Root {
    pub drv: StorePath,
    pub output: String,
}

impl Root {
    /// How `nix-instantiate` prints it: the plain drv path for `out`.
    pub fn print(&self) -> String {
        if self.output == "out" {
            self.drv.to_string()
        } else {
            format!("{}!{}", self.drv, self.output)
        }
    }
}

/// get-drvs.cc:getDerivations. A derivation is one root; an attrset is walked
/// in lexicographic name order and recursed into only with
/// `recurseForDerivations = true`; a list is always walked.
pub fn derivations(
    ev: &mut Evaluator,
    v: Word,
    args: Word,
    out: &mut Vec<Root>,
    seen: &mut HashSet<(String, String)>,
) -> EvalResult<()> {
    derivations_at(ev, v, args, out, seen, "")
}

/// `derivations`, telling every derivation it finds which attribute path it
/// was reached by (`attr` is the path already walked, from `-A`).
pub fn derivations_at(
    ev: &mut Evaluator,
    v: Word,
    args: Word,
    out: &mut Vec<Root>,
    seen: &mut HashSet<(String, String)>,
    attr: &str,
) -> EvalResult<()> {
    let v = ev.auto_call(v, args)?;
    let v = ev.force(v)?;
    if one_derivation(ev, v, out, seen, attr)? {
        return Ok(());
    }
    if let Some(attrs) = v.as_attrs() {
        let mut names: Vec<(Vec<u8>, Word)> = attrs
            .iter()
            .map(|a| (ev.interner.name(a.sym).to_vec(), a.cell.get()))
            .collect();
        names.sort_by(|a, b| a.0.cmp(&b.0));
        let recurse_sym = ev.sym(b"recurseForDerivations");
        for (name, child) in names {
            if !is_attr_path_component(&name) {
                continue;
            }
            let sub = join_attr(attr, &name);
            let child = ev.force(child)?;
            if one_derivation(ev, child, out, seen, &sub)? {
                continue;
            }
            let Some(ca) = child.as_attrs() else { continue };
            let Some(r) = ca.get(recurse_sym) else {
                continue;
            };
            if ev.force_bool(
                r.cell.get(),
                r.pos,
                "while evaluating the attribute `recurseForDerivations`",
            )? {
                derivations_at(ev, child, args, out, seen, &sub)?;
            }
        }
        return Ok(());
    }
    if let Some(l) = v.as_list() {
        for c in l.cells() {
            derivations_at(ev, c.get(), args, out, seen, attr)?;
        }
        return Ok(());
    }
    Err(Error::boxed(
        ErrorKind::Type,
        "expression does not evaluate to a derivation (or a set or list of those)",
        Pos::NONE,
    ))
}

/// `a.b` from `a` and `b`; `b` alone when the prefix is empty.
fn join_attr(prefix: &str, name: &[u8]) -> String {
    let name = String::from_utf8_lossy(name);
    if prefix.is_empty() {
        name.into_owned()
    } else {
        format!("{prefix}.{name}")
    }
}

/// True when `v` was a derivation and is now in `out`, or was a duplicate.
fn one_derivation(
    ev: &mut Evaluator,
    v: Word,
    out: &mut Vec<Root>,
    seen: &mut HashSet<(String, String)>,
    attr: &str,
) -> EvalResult<bool> {
    let Some(attrs) = v.as_attrs() else {
        return Ok(false);
    };
    let Some(t) = attrs.get(ev.s.type_) else {
        return Ok(false);
    };
    let t = ev.force_string(
        t.cell.get(),
        Pos::NONE,
        "while evaluating the 'type' attribute",
    )?;
    if t.bytes != b"derivation" {
        return Ok(false);
    }
    let Some(dp) = attrs.get(ev.s.drv_path) else {
        return Err(Error::boxed(
            ErrorKind::MissingAttr,
            "derivation without a 'drvPath' attribute",
            Pos::NONE,
        ));
    };
    let dp = ev.force_string(dp.cell.get(), Pos::NONE, "while evaluating 'drvPath'")?;
    let drv = StorePath::parse(&String::from_utf8_lossy(dp.bytes))
        .map_err(|e| Error::boxed(ErrorKind::Eval, e.0, Pos::NONE))?;
    let out_sym = ev.sym(b"outputName");
    let Some(on) = attrs.get(out_sym) else {
        return Err(Error::boxed(
            ErrorKind::MissingAttr,
            format!("derivation '{drv}' lacks an 'outputName' attribute"),
            Pos::NONE,
        ));
    };
    let on = ev.force_string(on.cell.get(), Pos::NONE, "while evaluating 'outputName'")?;
    let output = String::from_utf8_lossy(on.bytes).into_owned();
    if output.is_empty() {
        return Err(Error::boxed(
            ErrorKind::Eval,
            format!("derivation '{drv}' lacks an 'outputName' attribute"),
            Pos::NONE,
        ));
    }
    ev.drv.set_attr(&drv, attr);
    if seen.insert((drv.to_string(), output.clone())) {
        out.push(Root { drv, output });
    }
    Ok(true)
}

/// get-drvs.cc:isAttrPathComponent: `[A-Za-z_][A-Za-z0-9-_+]*`.
fn is_attr_path_component(name: &[u8]) -> bool {
    let Some((first, rest)) = name.split_first() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || *first == b'_') {
        return false;
    }
    rest.iter()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'+'))
}

/// The plan for every root, sources and drvs read from this evaluation.
pub fn plan_roots(
    ev: &Evaluator,
    roots: &[Root],
) -> crate::error::Result<crate::remote::plan::Plan> {
    let drvs = ev.drv.drvs.lock();
    let sources = ev.source_origins();
    let paths: Vec<StorePath> = roots.iter().map(|r| r.drv.clone()).collect();
    crate::remote::plan::plan(
        &paths,
        crate::remote::plan::Inputs {
            drvs: &drvs,
            sources: &sources,
        },
    )
}

/// The plan's locator: the local store, no `--source` overrides.
pub fn locate_store(
    paths: &[StorePath],
) -> crate::error::Result<Vec<Option<crate::remote::plan::SourceInfo>>> {
    crate::store::locate::locate_in_store(paths, &std::collections::HashMap::new())
}

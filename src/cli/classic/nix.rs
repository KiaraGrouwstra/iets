//! `nix` (src/nix): the subcommands ietsp implements — `eval`, `build` and
//! `print-dev-env` — with the same exec fallback as the other classic names
//! for everything else. See `docs/classic.md`.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use super::{Argv, Common, Root, fallback, is_unsupported};
use crate::build::render::Style;
use crate::build::upload::PlanUpload;
use crate::build::{Dispatcher, Target as BuildTarget};
use crate::cli::{ArgKind, abs_path, auto_args, cwd, parse_include, report};
use crate::eval::{Error, ErrorKind, EvalResult, Evaluator, Pos};
use crate::remote::plan::Plan;
use crate::remote::worker::{Options, Transport};
use crate::runtime::{Value, Word};
use crate::store::path::StorePath;

const NAME: &str = "nix";

/// The flags accepted before the subcommand, and how many values each takes.
/// `global_flag` applies them; `find_sub` only needs the arity.
const GLOBALS: &[(&str, usize)] = &[
    ("--impure", 0),
    ("--pure-eval", 0),
    ("--no-pure-eval", 0),
    ("--accept-flake-config", 0),
    ("--show-trace", 0),
    ("--experimental-features", 1),
    ("--extra-experimental-features", 1),
    ("--option", 2),
    ("-I", 1),
    ("--include", 1),
    ("--arg", 2),
    ("--argstr", 2),
    ("--store", 1),
    // The `nix-farm` wrapper passes `--eval-store auto` because the farm
    // refuses the worker-protocol tunnel; ietsp always evaluates locally.
    ("--eval-store", 1),
    ("-j", 1),
    ("--max-jobs", 1),
    ("--cores", 1),
    ("-k", 0),
    ("--keep-going", 0),
];

#[derive(Default)]
struct Nix {
    c: Common,
    json: bool,
    raw: bool,
    expr: Option<String>,
    file: Option<String>,
    installables: Vec<String>,
    /// The `-o`/`--out-link` prefix; `--no-link` clears it.
    out_link: String,
    no_link: bool,
    print_out_paths: bool,
    print_build_logs: bool,
}

/// Runs `nix <subcommand>`. `argv` is everything after `argv[0]`.
pub fn main(argv: &[String]) -> i32 {
    let k = match find_sub(argv) {
        Ok(k) => k,
        Err(reason) => fallback(NAME, argv, &reason),
    };
    let mut rest = argv.to_vec();
    let sub = rest.remove(k);
    match sub.as_str() {
        "eval" => eval_main(argv, &rest),
        "build" => build_main(argv, &rest),
        "print-dev-env" => dev_env_main(argv, &rest),
        "develop" => super::develop::main(argv, &rest),
        s => fallback(NAME, argv, &format!("subcommand '{s}'")),
    }
}

/// The index of the subcommand word: the first argument that is not one of
/// the global flags ietsp accepts.
fn find_sub(argv: &[String]) -> Result<usize, String> {
    let mut i = 0;
    while i < argv.len() {
        let s = &argv[i];
        if !s.starts_with('-') {
            return Ok(i);
        }
        let Some((_, arity)) = GLOBALS.iter().find(|(f, _)| *f == s.as_str()) else {
            return Err(format!("'{s}' is not implemented"));
        };
        i += 1 + arity;
    }
    Err("no subcommand".into())
}

fn number(flag: &str, v: &str) -> Result<String, String> {
    v.parse::<u64>()
        .map(|n| n.to_string())
        .map_err(|_| format!("'{flag}' wants a number, got '{v}'"))
}

/// `Ok(true)`: consumed. `Ok(false)`: not one of ours. `Err`: fall back.
pub(super) fn global_flag(c: &mut Common, a: &mut Argv, s: &str) -> Result<bool, String> {
    let Some((_, arity)) = GLOBALS.iter().find(|(f, _)| *f == s) else {
        return Ok(false);
    };
    let mut v = Vec::new();
    for _ in 0..*arity {
        v.push(a.value(s)?);
    }
    match s {
        "--accept-flake-config" => {}
        _ if crate::cli::pure_flag(s).is_some() => c.pure = crate::cli::pure_flag(s),
        "--experimental-features" | "--extra-experimental-features" => {}
        "--show-trace" => c.show_trace = true,
        "--option" => super::option(c, v[0].clone(), v[1].clone())?,
        "-I" | "--include" => c.includes.push(parse_include(&v[0])),
        "--arg" => c.args.push((v[0].clone(), ArgKind::Expr(v[1].clone()))),
        "--argstr" => c.args.push((v[0].clone(), ArgKind::Str(v[1].clone()))),
        "--store" => c.overrides.push(("store".into(), v[0].clone())),
        "--eval-store" if v[0] == "auto" || v[0] == "daemon" || v[0].is_empty() => {}
        "--eval-store" => {
            return Err(format!("'--eval-store {}' is not implemented", v[0]));
        }
        "-j" | "--max-jobs" => c.overrides.push(("max-jobs".into(), number(s, &v[0])?)),
        "--cores" => c.overrides.push(("cores".into(), number(s, &v[0])?)),
        "-k" | "--keep-going" => c.overrides.push(("keep-going".into(), "true".into())),
        _ => return Ok(false),
    }
    Ok(true)
}

fn parse_eval(argv: &[String]) -> Result<Nix, String> {
    let mut n = Nix::default();
    let mut a = Argv { all: argv, i: 0 };
    while a.i < argv.len() {
        let s = argv[a.i].clone();
        match s.as_str() {
            "--json" => n.json = true,
            "--raw" => n.raw = true,
            "--expr" => n.expr = Some(a.value(&s)?),
            "-f" | "--file" => n.file = Some(a.value(&s)?),
            _ => {
                if !global_flag(&mut n.c, &mut a, &s)? {
                    if s.starts_with('-') {
                        return Err(format!("'{s}' is not implemented"));
                    }
                    n.installables.push(s);
                }
            }
        }
        a.i += 1;
    }
    if n.raw && n.json {
        return Err("--raw and --json are mutually exclusive".into());
    }
    if n.expr.is_some() && n.file.is_some() {
        return Err("'--file' and '--expr' are exclusive".into());
    }
    if n.installables.len() > 1 {
        return Err("'nix eval' takes one installable".into());
    }
    check_args_with_flake(&n)?;
    Ok(n)
}

fn parse_build(argv: &[String]) -> Result<Nix, String> {
    let mut n = Nix {
        out_link: "result".into(),
        ..Nix::default()
    };
    let mut a = Argv { all: argv, i: 0 };
    while a.i < argv.len() {
        let s = argv[a.i].clone();
        match s.as_str() {
            "-o" | "--out-link" => n.out_link = a.value(&s)?,
            "--no-link" => n.no_link = true,
            "--print-out-paths" => n.print_out_paths = true,
            "-L" | "--print-build-logs" => n.print_build_logs = true,
            "--expr" => n.expr = Some(a.value(&s)?),
            "-f" | "--file" => n.file = Some(a.value(&s)?),
            _ => {
                if !global_flag(&mut n.c, &mut a, &s)? {
                    if s.starts_with('-') {
                        return Err(format!("'{s}' is not implemented"));
                    }
                    n.installables.push(s);
                }
            }
        }
        a.i += 1;
    }
    if n.expr.is_some() && n.file.is_some() {
        return Err("'--file' and '--expr' are exclusive".into());
    }
    if n.installables.is_empty() {
        n.installables.push(".".into());
    }
    check_args_with_flake(&n)?;
    Ok(n)
}

/// installable-flake.cc: a flake installable refuses `--arg`/`--argstr`, so
/// the real nix reports that better than we would.
fn check_args_with_flake(n: &Nix) -> Result<(), String> {
    if !n.c.args.is_empty() && n.expr.is_none() && n.file.is_none() {
        return Err("'--arg' and '--argstr' are incompatible with flakes".into());
    }
    Ok(())
}

enum Run {
    Failed(i32),
    Unsupported(String),
    /// `nix eval`: the whole stdout, already formatted.
    Out(Vec<u8>),
    /// `nix build`: what to build, and the closure to upload.
    Ready(Vec<Target>, Box<Plan>),
}

/// `settings` (`shell_settings` without `builder`) plus the `nix` command's
/// `pure-eval`. main.cc: pure unless a flag says otherwise; nix.conf's
/// setting does not change that, it only counts as set. installables.cc:
/// `--file` is impure, and refuses a `pure-eval` set anywhere.
pub(super) fn nix_settings(
    c: &mut Common,
    file: bool,
    builder: bool,
) -> Result<(crate::config::Config, Transport, Options), String> {
    let s = if builder {
        super::settings(c)
    } else {
        super::shell_settings(c)
    }?;
    let set = c.pure.is_some() || s.0.bool("pure-eval").ok().flatten().is_some();
    let pure = c.pure.unwrap_or(true);
    if file && pure && set {
        return Err("'--file' is not compatible with '--pure-eval'".into());
    }
    c.pure = Some(pure && !file);
    Ok(s)
}

fn eval_main(argv: &[String], rest: &[String]) -> i32 {
    let mut n = match parse_eval(rest) {
        Ok(n) => n,
        Err(reason) => fallback(NAME, argv, &reason),
    };
    let (cfg, _, _) = match nix_settings(&mut n.c, n.file.is_some(), true) {
        Ok(t) => t,
        Err(reason) => fallback(NAME, argv, &reason),
    };
    let ev = super::evaluator(&n.c, &cfg);
    let n = Arc::new(n);
    let run = Arc::clone(&n);
    match crate::parallel::run(crate::parallel::default_workers(), ev, move |ev| {
        eval_run(ev, &run)
    }) {
        Run::Failed(code) => code,
        Run::Unsupported(reason) => fallback(NAME, argv, &reason),
        Run::Out(text) => {
            print_bytes(&text);
            0
        }
        Run::Ready(..) => unreachable!("eval does not build"),
    }
}

fn eval_run(ev: &mut Evaluator, n: &Nix) -> Run {
    let cwd = cwd();
    let mut out: Vec<u8> = Vec::new();
    let r = (|| -> Result<(), Box<Error>> {
        let what = n.installables.first().map_or(".", String::as_str);
        let v = resolve(ev, n, what, &cwd)?;
        // installables.cc: the installable's value is forced before printing,
        // so an error at the top level is reported instead of printed.
        let v = ev.force(v)?;
        if n.raw {
            let mut ctx = Vec::new();
            let s = ev.coerce_to_string(
                v,
                &mut ctx,
                Pos::NONE,
                "while generating the eval command output",
                false,
                true,
                true,
            )?;
            out.extend_from_slice(&s);
        } else if n.json {
            let mut ctx = Vec::new();
            crate::print::print_json(ev, v, &mut out, &mut ctx, true, Pos::NONE)?;
            out.push(b'\n');
        } else {
            let mut seen = HashSet::new();
            print_eval(ev, v, &mut out, &mut seen, 0)?;
            out.push(b'\n');
        }
        Ok(())
    })();
    match r {
        Ok(()) => Run::Out(out),
        Err(e) if is_unsupported(&e) => Run::Unsupported(e.msg.clone()),
        Err(e) => {
            report(ev, &e);
            Run::Failed(1)
        }
    }
}

fn build_main(argv: &[String], rest: &[String]) -> i32 {
    let mut n = match parse_build(rest) {
        Ok(n) => n,
        Err(reason) => fallback(NAME, argv, &reason),
    };
    let (cfg, transport, options) = match nix_settings(&mut n.c, n.file.is_some(), true) {
        Ok(t) => t,
        Err(reason) => fallback(NAME, argv, &reason),
    };
    let ev = super::evaluator(&n.c, &cfg);
    let n = Arc::new(n);
    let run = Arc::clone(&n);
    match crate::parallel::run(crate::parallel::default_workers(), ev, move |ev| {
        build_run(ev, &run)
    }) {
        Run::Failed(code) => code,
        Run::Unsupported(reason) => fallback(NAME, argv, &reason),
        Run::Out(_) => unreachable!("build prints its own output"),
        Run::Ready(targets, plan) => realise(&n, &transport, &options, &targets, &plan),
    }
}

fn build_run(ev: &mut Evaluator, n: &Nix) -> Run {
    let cwd = cwd();
    let mut targets: Vec<Target> = Vec::new();
    let r = (|| -> Result<(), Box<Error>> {
        for what in &n.installables {
            let v = resolve(ev, n, what, &cwd)?;
            targets.push(target(ev, v, what, n.expr.is_none() && n.file.is_none())?);
        }
        Ok(())
    })();
    if let Err(e) = r {
        if is_unsupported(&e) {
            return Run::Unsupported(e.msg.clone());
        }
        report(ev, &e);
        return Run::Failed(1);
    }
    let roots = flat_roots(&targets);
    match super::plan_roots(ev, &roots) {
        Ok(p) => Run::Ready(targets, Box::new(p)),
        Err(e) => {
            eprintln!("error: {e}");
            Run::Failed(1)
        }
    }
}

fn dev_env_main(argv: &[String], rest: &[String]) -> i32 {
    let mut pass: Vec<String> = Vec::new();
    let mut flakes = 0;
    for s in rest {
        if s.starts_with('-') && s != "--json" {
            fallback(NAME, argv, &format!("'{s}' is not implemented"));
        }
        if !s.starts_with('-') {
            flakes += 1;
        }
        pass.push(s.clone());
    }
    if flakes > 1 {
        fallback(NAME, argv, "'nix print-dev-env' takes one installable");
    }
    // no flag reached here, so main.cc's default stands
    pass.push("--pure-eval".into());
    crate::cli::devenv::cmd_print_dev_env(&pass)
}

// ---- installables ------------------------------------------------------------

/// installable-flake.cc:getActualAttrPaths with installables.cc's defaults:
/// the `packages.<system>.` and `legacyPackages.<system>.` prefixes on the
/// fragment, then the fragment itself; no fragment means the default package.
fn attr_paths(fragment: &str, system: &str) -> Vec<String> {
    if fragment.is_empty() {
        return vec![
            format!("packages.{system}.default"),
            format!("defaultPackage.{system}"),
        ];
    }
    if let Some(rest) = fragment.strip_prefix('.') {
        return vec![rest.to_string()];
    }
    vec![
        format!("packages.{system}.{fragment}"),
        format!("legacyPackages.{system}.{fragment}"),
        fragment.to_string(),
    ]
}

/// The value one installable names: an attribute of the `--expr`/`--file`
/// value, or a flake output attribute.
fn resolve(ev: &mut Evaluator, n: &Nix, what: &str, cwd: &[u8]) -> EvalResult<Word> {
    let args = auto_args(ev, &n.c.args, cwd)?;
    // installable-attr-path.cc:parse: `.` is the whole value.
    let attr = if what == "." { "" } else { what };
    if let Some(e) = &n.expr {
        let root = ev.eval_string(e, cwd)?;
        return super::select(ev, root, attr, args);
    }
    if let Some(f) = &n.file {
        let p = super::lookup_file_arg(ev, f, cwd)?;
        let root = ev.eval_file(&p, Pos::NONE)?;
        return super::select(ev, root, attr, args);
    }
    if what.contains('^') || what.as_bytes().starts_with(&ev.store_dir) {
        return Err(Error::boxed(
            ErrorKind::Unsupported,
            format!("the installable '{what}' is a store path"),
            Pos::NONE,
        ));
    }
    let (loc, fragment) = what.split_once('#').unwrap_or((what, ""));
    let fr = crate::cli::flake_ref(loc, cwd)
        .map_err(|e| Error::boxed(ErrorKind::Unsupported, e.to_string(), Pos::NONE))?;
    let outputs = crate::flake::eval_flake(ev, &fr)?;
    let candidates = attr_paths(fragment, ev.current_system);
    for c in &candidates {
        let path = crate::flake::parse_attr_path(c)
            .map_err(|m| Error::boxed(ErrorKind::Eval, m.to_string(), Pos::NONE))?;
        if let Some(v) = crate::flake::select(ev, outputs, &path)? {
            return Ok(v);
        }
    }
    Err(Error::boxed(
        ErrorKind::MissingAttr,
        format!(
            "flake '{loc}' does not provide attribute {}",
            crate::devenv::show_attr_paths(&candidates)
        ),
        Pos::NONE,
    ))
}

/// One installable's derivation and the outputs `nix build` installs.
struct Target {
    drv: StorePath,
    outputs: Vec<String>,
}

/// The `Root`s the plan and `BuildPathsWithResults` need, in target order.
fn flat_roots(targets: &[Target]) -> Vec<Root> {
    let mut roots = Vec::new();
    let mut seen = HashSet::new();
    for t in targets {
        for o in &t.outputs {
            if seen.insert((t.drv.to_string(), o.clone())) {
                roots.push(Root {
                    drv: t.drv.clone(),
                    output: o.clone(),
                });
            }
        }
    }
    roots
}

/// The derivation an installable names and its outputs to install
/// (get-drvs.cc:queryOutputs, installable-flake.cc:toDerivedPaths):
/// `outputSpecified` picks one, else `meta.outputsToInstall`, else every
/// output for an attr-path installable and `out` for a flake one.
fn target(ev: &mut Evaluator, v: Word, what: &str, flake: bool) -> EvalResult<Target> {
    let v = ev.force(v)?;
    let unsupported = |msg: String| Error::boxed(ErrorKind::Unsupported, msg, Pos::NONE);
    let Some(attrs) = v.as_attrs() else {
        return Err(unsupported(format!(
            "the installable '{what}' is not a derivation"
        )));
    };
    let is_drv = match attrs.get(ev.s.type_) {
        Some(t) => {
            ev.force_string(
                t.cell.get(),
                Pos::NONE,
                "while evaluating the 'type' attribute",
            )?
            .bytes
                == b"derivation"
        }
        None => false,
    };
    if !is_drv {
        return Err(unsupported(format!(
            "the installable '{what}' is not a derivation"
        )));
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

    let name_of = |ev: &mut Evaluator, w: Word, ctx: &str| -> EvalResult<String> {
        Ok(String::from_utf8_lossy(ev.force_string(w, Pos::NONE, ctx)?.bytes).into_owned())
    };
    let specified = match attrs.get(ev.sym(b"outputSpecified")) {
        Some(a) => ev.force_bool(
            a.cell.get(),
            a.pos,
            "while evaluating the 'outputSpecified' attribute of a derivation",
        )?,
        None => false,
    };
    if specified {
        let Some(on) = attrs.get(ev.sym(b"outputName")) else {
            return Err(Error::boxed(
                ErrorKind::MissingAttr,
                format!("derivation '{drv}' lacks an 'outputName' attribute"),
                Pos::NONE,
            ));
        };
        let o = name_of(ev, on.cell.get(), "while evaluating 'outputName'")?;
        return Ok(Target {
            drv,
            outputs: vec![o],
        });
    }
    if let Some(m) = attrs.get(ev.sym(b"meta")) {
        let meta = ev.force(m.cell.get())?;
        if let Some(ma) = meta.as_attrs()
            && let Some(oti) = ma.get(ev.sym(b"outputsToInstall"))
        {
            let l = ev.force(oti.cell.get())?;
            let Some(l) = l.as_list() else {
                return Err(Error::boxed(
                    ErrorKind::Type,
                    "this derivation has bad 'meta.outputsToInstall'",
                    Pos::NONE,
                ));
            };
            let mut outputs = Vec::new();
            for c in l.cells() {
                outputs.push(name_of(
                    ev,
                    c.get(),
                    "while evaluating 'meta.outputsToInstall'",
                )?);
            }
            outputs.sort();
            return Ok(Target { drv, outputs });
        }
    }
    if flake {
        return Ok(Target {
            drv,
            outputs: vec!["out".into()],
        });
    }
    let mut outputs = Vec::new();
    if let Some(o) = attrs.get(ev.sym(b"outputs")) {
        let l = ev.force(o.cell.get())?;
        let Some(l) = l.as_list() else {
            return Err(Error::boxed(
                ErrorKind::Type,
                "the 'outputs' attribute of a derivation is not a list",
                Pos::NONE,
            ));
        };
        for c in l.cells() {
            outputs.push(name_of(
                ev,
                c.get(),
                "while evaluating the name of an output of a derivation",
            )?);
        }
    }
    if outputs.is_empty() {
        outputs.push("out".into());
    }
    outputs.sort();
    Ok(Target { drv, outputs })
}

// ---- building ----------------------------------------------------------------

/// Uploads the closure, builds every target in one call, makes the symlinks
/// (command.cc:createOutLinks) and prints with `--print-out-paths`.
fn realise(
    n: &Nix,
    transport: &Transport,
    options: &Options,
    targets: &[Target],
    plan: &Plan,
) -> i32 {
    let roots = flat_roots(targets);
    let style = Style {
        quiet: !n.print_build_logs,
        ..Style::detect()
    };
    let mut d = match Dispatcher::open(
        transport,
        options,
        crate::provenance::Provenance::off(),
        style,
    ) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    let mut batch: Vec<BuildTarget> = Vec::new();
    for r in &roots {
        match batch.iter_mut().find(|t| t.drv == r.drv) {
            Some(t) => {
                if !t.outputs.contains(&r.output) {
                    t.outputs.push(r.output.clone());
                }
            }
            None => batch.push(BuildTarget::new(
                r.drv.name(),
                r.drv.clone(),
                vec![r.output.clone()],
            )),
        }
    }
    let mut up = PlanUpload::new(plan, &super::locate_store);
    let outcomes = match d.submit(&mut up, batch) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    if d.failed {
        return 1;
    }
    let maps: HashMap<String, BTreeMap<String, Option<String>>> = outcomes
        .iter()
        .map(|o| {
            (
                o.drv.to_string(),
                o.outputs
                    .iter()
                    .map(|(k, v)| (k.clone(), Some(v.clone())))
                    .collect(),
            )
        })
        .collect();

    let cwd = cwd();
    let mut out = String::new();
    let mut links: Vec<(String, Option<String>)> = Vec::new();
    for (i, t) in targets.iter().enumerate() {
        let drv = t.drv.to_string();
        for output in &t.outputs {
            let path = maps
                .get(&drv)
                .and_then(|m| m.get(output).cloned())
                .flatten();
            let Some(path) = path else {
                eprintln!("error: derivation '{drv}' has no output '{output}'");
                return 1;
            };
            if n.no_link {
                if !super::store_given(&n.c) {
                    links.push((path.clone(), None));
                }
            } else {
                // build.cc: the n-th installable adds `-n` from 1 on, an
                // output other than `out` adds `-<output>`.
                let mut link = n.out_link.clone();
                if i > 0 {
                    link.push_str(&format!("-{i}"));
                }
                if output != "out" {
                    link.push('-');
                    link.push_str(output);
                }
                let link = String::from_utf8_lossy(&abs_path(&link, &cwd)).into_owned();
                links.push((path.clone(), Some(link)));
            }
            if n.print_out_paths {
                out.push_str(&path);
                out.push('\n');
            }
        }
    }
    if let Err(e) = d.out_links(&links) {
        eprintln!("error: {e}");
        return 1;
    }
    print_bytes(out.as_bytes());
    0
}

// ---- printing ----------------------------------------------------------------

fn print_bytes(b: &[u8]) {
    use std::io::Write;
    let mut out = std::io::stdout();
    let _ = out.write_all(b);
    let _ = out.flush();
}

/// An error the printer shows in place of the value. `Unsupported` must
/// escape so the caller can fall back to the real nix instead.
fn printable(e: &Error) -> bool {
    !matches!(e.kind, ErrorKind::Unsupported | ErrorKind::StackOverflow)
}

/// `nix eval`'s printer (print.cc:Printer with `force`, `derivationPaths`
/// and `trackRepeated`): forces as it goes, prints a derivation as its drv
/// path and a failing value as `«error: …»`.
fn print_eval(
    ev: &mut Evaluator,
    w: Word,
    out: &mut Vec<u8>,
    seen: &mut HashSet<u64>,
    depth: usize,
) -> EvalResult<()> {
    if depth > crate::eval::MAX_CALL_DEPTH as usize {
        return Err(Error::boxed(
            ErrorKind::StackOverflow,
            "stack overflow; max-call-depth exceeded",
            Pos::NONE,
        ));
    }
    let w = match ev.force(w) {
        Ok(w) => w,
        Err(e) if printable(&e) => {
            out.extend_from_slice("«error: ".as_bytes());
            out.extend_from_slice(e.msg.as_bytes());
            out.extend_from_slice("»".as_bytes());
            return Ok(());
        }
        Err(e) => return Err(e),
    };
    match w.view() {
        Value::Attrs(a) => {
            // Nix keys the seen set on the `Bindings` and shares one empty
            // `Bindings`, so the second empty attrset is «repeated».
            let key = if a.is_empty() { 0 } else { w.0 };
            if !seen.insert(key) {
                out.extend_from_slice("«repeated»".as_bytes());
                return Ok(());
            }
            if let Some(drv) = derivation_path(ev, w)? {
                out.extend_from_slice("«derivation".as_bytes());
                if !drv.is_empty() {
                    out.push(b' ');
                    out.extend_from_slice(&drv);
                }
                out.extend_from_slice("»".as_bytes());
                return Ok(());
            }
            out.extend_from_slice(b"{ ");
            for attr in crate::print::lexicographic_order(ev, a) {
                crate::print::print_attr_name(out, ev.interner.name(attr.sym));
                out.extend_from_slice(b" = ");
                print_eval(ev, attr.cell.get(), out, seen, depth + 1)?;
                out.extend_from_slice(b"; ");
            }
            out.push(b'}');
        }
        Value::List(l) => {
            if l.len > 0 && !seen.insert(w.0) {
                out.extend_from_slice("«repeated»".as_bytes());
                return Ok(());
            }
            out.extend_from_slice(b"[ ");
            for c in l.cells() {
                print_eval(ev, c.get(), out, seen, depth + 1)?;
                out.push(b' ');
            }
            out.push(b']');
        }
        Value::PrimOp(p) => {
            out.extend_from_slice("«primop ".as_bytes());
            out.extend_from_slice(primop_name(p.name).as_bytes());
            out.extend_from_slice("»".as_bytes());
        }
        Value::PrimOpApp(p) => {
            out.extend_from_slice("«partially applied primop ".as_bytes());
            out.extend_from_slice(primop_name(p.op.name).as_bytes());
            out.extend_from_slice("»".as_bytes());
        }
        Value::Lambda(_) => {
            out.extend_from_slice(crate::print::show_value_short(ev, w).as_bytes());
        }
        _ => {
            let mut one = HashSet::new();
            crate::print::print_ambiguous(ev, w, 1, out, &mut one, depth)?;
        }
    }
    Ok(())
}

/// eval.cc:addPrimOp drops a leading `__` from a primop's name, so that is
/// the name print.cc:printFunction shows.
fn primop_name(name: &str) -> &str {
    name.strip_prefix("__").unwrap_or(name)
}

/// print.cc:printDerivation: `Some(drvPath)` when the attrset is a
/// derivation, which the printer shows instead of the attributes.
fn derivation_path(ev: &mut Evaluator, w: Word) -> EvalResult<Option<Vec<u8>>> {
    let Some(attrs) = w.as_attrs() else {
        return Ok(None);
    };
    let Some(t) = attrs.get(ev.s.type_) else {
        return Ok(None);
    };
    let t = ev.force_string(
        t.cell.get(),
        Pos::NONE,
        "while evaluating the 'type' attribute",
    )?;
    if t.bytes != b"derivation" {
        return Ok(None);
    }
    let Some(dp) = attrs.get(ev.s.drv_path) else {
        return Ok(Some(Vec::new()));
    };
    let mut ctx = Vec::new();
    let s = ev.coerce_to_string(
        dp.cell.get(),
        &mut ctx,
        dp.pos,
        "while evaluating the drvPath of a derivation",
        false,
        false,
        true,
    )?;
    Ok(Some(s.to_vec()))
}

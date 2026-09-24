//! `nix` (src/nix): the subcommands ietsp implements — `eval`, `build`,
//! `print-dev-env`, `develop` and `flake prefetch-inputs` — with the same exec
//! fallback as the other classic names for everything else. See
//! `docs/classic.md`.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use super::{Argv, Common, Root, fallback, is_unsupported};
use crate::api::installable;
use crate::build::render::Style;
use crate::build::upload::PlanUpload;
use crate::build::{Dispatcher, Target as BuildTarget};
use crate::cli::{abs_path, auto_args, cwd, report};
use crate::eval::{Error, ErrorKind, EvalResult, Evaluator, Pos};
use crate::remote::plan::Plan;
use crate::remote::worker::{Options, Transport};
use crate::runtime::{Value, Word};
use crate::store::path::StorePath;

const NAME: &str = "nix";
/// filetransfer.hh: the default of `http-connections`.
const HTTP_CONNECTIONS: usize = 25;

/// The flags accepted before the subcommand, and how many values each takes.
/// `global_flag` applies them; `find_sub` only needs the arity.
const GLOBALS: &[(&str, usize)] = &[
    ("--impure", 0),
    ("--pure-eval", 0),
    ("--no-pure-eval", 0),
    ("--accept-flake-config", 0),
    ("--show-trace", 0),
    ("--log-format", 1),
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
        "search" => crate::cli::search::classic_main(argv, &rest),
        "flake" if rest.get(k).is_some_and(|s| s == "prefetch-inputs") => {
            rest.remove(k);
            prefetch_inputs_main(argv, &rest)
        }
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

/// `Ok(true)`: consumed. `Ok(false)`: not one of ours. `Err`: fall back.
pub fn global_flag(c: &mut Common, a: &mut Argv, s: &str) -> Result<bool, String> {
    if crate::config::determinate_flag(s, || a.value(s))? {
        return Ok(true);
    }
    if !GLOBALS.iter().any(|(f, _)| *f == s) {
        return Ok(false);
    }
    match s {
        "--accept-flake-config" => {}
        "--experimental-features" | "--extra-experimental-features" => {
            a.value(s)?;
        }
        "--eval-store" => match a.value(s)?.as_str() {
            "auto" | "daemon" | "" => {}
            v => return Err(format!("'--eval-store {v}' is not implemented")),
        },
        _ => return super::common_flag(c, a, s),
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
    if !n.c.flags.args.is_empty() && n.expr.is_none() && n.file.is_none() {
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
pub fn nix_settings(
    c: &mut Common,
    file: bool,
    builder: bool,
) -> Result<(crate::config::Config, Transport, Options), String> {
    let s = if builder {
        super::settings(c)
    } else {
        super::shell_settings(c)
    }?;
    let set = c.flags.pure.is_some() || s.0.bool("pure-eval").ok().flatten().is_some();
    let pure = c.flags.pure.unwrap_or(true);
    if file && pure && set {
        return Err("'--file' is not compatible with '--pure-eval'".into());
    }
    c.flags.pure = Some(pure && !file);
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
    match crate::cli::run_parallel(crate::cli::Run::on(ev), move |ev, _| eval_run(ev, &run)) {
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
        Err(e) => Run::Failed(report(ev, &e)),
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
    match crate::cli::run_parallel(crate::cli::Run::on(ev), move |ev, _| build_run(ev, &run)) {
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
        return Run::Failed(report(ev, &e));
    }
    let roots = flat_roots(&targets);
    match super::plan_roots(ev, &roots) {
        Ok(p) => Run::Ready(targets, Box::new(p)),
        Err(e) => Run::Failed(e.report()),
    }
}

/// flake-prefetch-inputs.cc: every locked input the lock reaches, fetched in
/// parallel the way an evaluation fetches it (`fetch::ensure_readable`), on
/// as many threads as `http-connections`' default. A lock that is missing or
/// incomplete needs locking, which the real nix does.
fn prefetch_inputs_main(argv: &[String], rest: &[String]) -> i32 {
    use crate::flake::fetch::{AttrVal, Attrs, ensure_readable, parse_nar_hash, source_store_path};
    let mut c = Common::default();
    let mut refs = Vec::new();
    let mut a = Argv { all: rest, i: 0 };
    while a.i < rest.len() {
        let s = rest[a.i].clone();
        match global_flag(&mut c, &mut a, &s) {
            Ok(true) => {}
            Ok(false) if !s.starts_with('-') => refs.push(s),
            Ok(false) => fallback(NAME, argv, &format!("'{s}' is not implemented")),
            Err(why) => fallback(NAME, argv, &why),
        }
        a.i += 1;
    }
    if refs.len() > 1 {
        fallback(NAME, argv, "'nix flake prefetch-inputs' takes one flake");
    }
    let fr = crate::api::flake_ref(refs.first().map_or(".", String::as_str), &cwd())
        .unwrap_or_else(|e| fallback(NAME, argv, &e.to_string()));
    let lock = std::fs::read(fr.dir.join("flake.lock"))
        .map_err(|e| e.to_string())
        .and_then(|b| crate::flake::lock::check(&b).map_err(|e| e.to_string()))
        .unwrap_or_else(|why| fallback(NAME, argv, &format!("flake.lock: {why}")));
    let mut todo = Vec::new();
    for (name, locked) in lock.reached_inputs() {
        let mut attrs: Attrs = locked
            .iter()
            .filter_map(|(k, v)| {
                let v = match v {
                    serde_json::Value::String(s) => AttrVal::Str(s.clone()),
                    serde_json::Value::Bool(b) => AttrVal::Bool(*b),
                    v => AttrVal::Int(v.as_u64()?),
                };
                Some((k.clone(), v))
            })
            .collect();
        match (locked["type"].as_str(), locked["narHash"].as_str()) {
            // a path input is read in place, and a relative one is the root's
            (Some("path"), _) => {}
            (Some("github" | "gitlab" | "sourcehut" | "tarball" | "git" | "hg"), Some(h)) => {
                let (h, sp) = parse_nar_hash(h)
                    .and_then(|h| Ok((h.to_sri(), source_store_path(&h)?)))
                    .unwrap_or_else(|e| fallback(NAME, argv, &format!("input '{name}': {e}")));
                attrs.insert("narHash".into(), AttrVal::Str(h.clone()));
                todo.push((name.to_string(), h, sp, attrs));
            }
            (t, _) => fallback(NAME, argv, &format!("input '{name}' of type {t:?}")),
        }
    }
    let system = crate::config::Config::load(&c.flags.overrides)
        .unwrap_or_else(|e| fallback(NAME, argv, &e.to_string()))
        .system();
    let threads = todo.len().min(HTTP_CONNECTIONS);
    let queue = std::sync::Mutex::new(todo.into_iter());
    let failed = std::sync::atomic::AtomicBool::new(false);
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(crate::session::bind(|| {
                while let Some((name, hash, sp, attrs)) = queue.lock().unwrap().next() {
                    let mut ev = Evaluator::with(&system, true);
                    ev.input_names.insert(hash, name);
                    if let Err(e) = ensure_readable(&mut ev, &sp, &attrs, Pos::NONE) {
                        report(&ev, &e);
                        failed.store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                }
            }));
        }
    });
    i32::from(failed.into_inner())
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

/// The value one installable names: an attribute of the `--expr`/`--file`
/// value, or a flake output attribute.
fn resolve(ev: &mut Evaluator, n: &Nix, what: &str, cwd: &[u8]) -> EvalResult<Word> {
    let args = auto_args(ev, &n.c.flags.args, cwd)?;
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
    let mut found = installable::from_outputs(
        ev,
        outputs,
        loc,
        installable::Prefixes::Package,
        fragment,
        installable::Want::First,
    )?;
    Ok(found.remove(0).value)
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
        Err(e) => return e.report(),
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
        Err(e) => return e.report(),
    };
    // a daemon client nix exits 1 on a failed build, whatever the status
    if d.exit != 0 {
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
                return crate::error::Error::Store(format!(
                    "derivation '{drv}' has no output '{output}'"
                ))
                .report();
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
        return e.report();
    }
    print_bytes(out.as_bytes());
    0
}

// ---- printing ----------------------------------------------------------------

pub(crate) fn print_bytes(b: &[u8]) {
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

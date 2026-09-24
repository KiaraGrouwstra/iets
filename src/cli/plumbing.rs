//! `ietsp plumbing OP`: the machine-first commands. One operation each,
//! JSON on stdout, a stable contract (docs/plumbing.md).

use std::collections::HashMap;

use crate::api::build::{self as apibuild, Dispatch};
use crate::api::installable::{self, Prefixes, Want};
use crate::api::render::Report;
use crate::build::Target;
use crate::build::render::Style;
use crate::cli::help::{self, arg_error, value};
use crate::cli::{EvalArgs, Input, evaluate, parse_eval_args, report, run_for};
use crate::error::Error as CrateError;
use crate::eval::{Error, Evaluator};
use crate::runtime::Word;
use crate::store::locate::drv_roots;
use crate::store::path::StorePath;

pub const HELP: &str = "\
usage: ietsp plumbing OP [FLAGS] ...

The machine-first operations the user-facing commands are written against.
Each one writes JSON on stdout (docs/plumbing.md).

operations:
  resolve   an installable becomes derivation roots
  plan      derivation roots become an upload plan
  missing   a plan and a builder become what will be built and fetched
  build     derivation roots are built; outcomes come back
  fetch     a URL or a fixed-output derivation becomes a hash

Every op that takes roots names them as DRV or DRV!OUT[,OUT], or reads a
previous op's stdout with --roots -.

usage: ietsp plumbing resolve [FLAGS] (-E EXPR | FILE | FLAKEREF[#FRAGMENT])

Print {\"attrPaths\", \"roots\"}: the attribute paths that were tried, and
one object per derivation root with \"drv\", \"output\" and \"attr\".

  --prefixes WHICH      the attribute paths a flake fragment is tried at:
                        package (the default), dev-shell, package-set, or
                        none for the fragment itself
  --want first|all      stop at the first path that is there, or take all
  --no-drvs             report the attribute paths only

usage: ietsp plumbing plan [--roots -] DRV...

Print {\"drvs\", \"sources\", \"storeDrvs\"}: the closure to upload, in
dependency order, references first.

usage: ietsp plumbing missing [FLAGS] [--roots -] DRV...

Print {\"willBuild\", \"willSubstitute\", \"unknown\", \"downloadSize\",
\"narSize\"}: what building the roots on the builder would take.

  --builder URL         where the builds go; the builder setting by default
  --check-cache-status  add \"roots\": one {\"drv\", \"cacheStatus\",
                        \"neededBuilds\", \"neededSubstitutes\"} per root

usage: ietsp plumbing build [FLAGS] [--roots -] DRV...

Build the roots and print {\"outcomes\"}: one {\"drv\", \"success\",
\"outputs\", \"reason\", \"logTail\"} each. The exit code is docs/cli.md's.

  --builder URL         where the builds go; the builder setting by default
  --out NAME            an output to build besides the roots' own
  --source STOREPATH=LOCAL   upload STOREPATH from a local tree
  --fetch-to DIR        restore every output into DIR
  --gc-root DIR         register each output under DIR on the local daemon,
                        and report the links as \"gcRoots\"
  --no-build            register the closure and stop

usage: ietsp plumbing fetch [FLAGS] URL
       ietsp plumbing fetch [FLAGS] (-E EXPR | FILE | FLAKEREF[#FRAGMENT])

Print {\"hash\", \"url\"} for what URL serves, or {\"hash\", \"drvPath\"} for
the hash a fixed-output derivation produces. Nothing is added to a store.

  --unpack              hash the NAR of the unpacked tarball (fetchzip)
  --hash-type ALGO      md5, sha1, sha256 (default) or sha512
";

pub fn main(argv: &[String]) -> i32 {
    match argv.first().map(String::as_str) {
        Some("resolve") => resolve(&argv[1..]),
        Some("plan") => op_plan(&argv[1..]),
        Some("missing") => op_missing(&argv[1..]),
        Some("build") => op_build(&argv[1..]),
        Some("fetch") => op_fetch(&argv[1..]),
        w => help::unknown_subcommand(w, &["resolve", "plan", "missing", "build", "fetch"]),
    }
}

struct Opts {
    /// `None` is `--prefixes none`: the fragment names the path itself.
    prefixes: Option<Prefixes>,
    want: Want,
    drvs: bool,
}

fn resolve(argv: &[String]) -> i32 {
    let mut o = Opts {
        prefixes: Some(Prefixes::Package),
        want: Want::First,
        drvs: true,
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
            "--prefixes" => {
                let v = next();
                o.prefixes = match v.as_str() {
                    "none" => None,
                    _ => Some(Prefixes::parse(&v).unwrap_or_else(|| {
                        arg_error(format!(
                            "--prefixes wants none, package, dev-shell or package-set, got '{v}'"
                        ))
                    })),
                };
            }
            "--want" => {
                let v = next();
                o.want = match v.as_str() {
                    "first" => Want::First,
                    "all" => Want::All,
                    _ => arg_error(format!("--want wants first or all, got '{v}'")),
                };
            }
            "--no-drvs" => o.drvs = false,
            s => rest.push(s.to_string()),
        }
        i += 1;
    }
    let a = parse_eval_args(&rest);
    if a.input.is_none() {
        arg_error("nothing to resolve: give -E EXPR, FILE or FLAKEREF[#FRAGMENT]")
    }
    crate::cli::run_parallel(run_for(&a), move |ev, _| match run(ev, a, o) {
        Ok(()) => 0,
        Err(e) => report(ev, &e),
    })
}

fn run(ev: &mut Evaluator, a: EvalArgs, o: Opts) -> Result<(), Box<Error>> {
    let flake = match (&a.input, o.prefixes) {
        (Some(Input::Flake(f)), Some(p)) => {
            let (loc, frag) = f.split_once('#').unwrap_or((f.as_str(), ""));
            Some((loc.to_string(), frag.to_string(), p))
        }
        _ => None,
    };
    let attr = a.attr.clone();
    let mut paths: Vec<String> = Vec::new();
    let mut found: Vec<(Option<String>, Word)> = Vec::new();
    match flake {
        None => found.push((attr, evaluate(ev, a)?)),
        Some((loc, frag, p)) => {
            paths = installable::attr_paths(p, &frag, ev.current_system);
            let outputs = evaluate(
                ev,
                EvalArgs {
                    input: Some(Input::Flake(loc.clone())),
                    attr: None,
                    ..a
                },
            )?;
            for f in installable::from_outputs(ev, outputs, &loc, p, &frag, o.want)? {
                found.push((Some(f.path.join(".")), f.value));
            }
        }
    }
    let mut roots = Vec::new();
    for (attr, v) in found {
        if !o.drvs {
            break;
        }
        let mut rs = Vec::new();
        drv_roots(ev, v, &mut rs)?;
        for (drv, output) in rs {
            roots.push(serde_json::json!({
                "drv": drv.to_string(),
                "output": output,
                "attr": attr,
            }));
        }
    }
    let v = serde_json::json!({ "attrPaths": paths, "roots": roots });
    crate::cli::render::bytes(&crate::cli::render::json(&v, false));
    Ok(())
}

// ---- roots -------------------------------------------------------------------

/// The roots an op works on, one target per derivation. An argument is `DRV`
/// or `DRV!OUT[,OUT]`; `--roots -` reads a previous op's stdout instead, the
/// `roots` of a `resolve` or the `drvs` of a `plan`. No output name means
/// every output, as `!*` does in the worker protocol.
fn targets(args: &[String], stdin: bool) -> Vec<Target> {
    let mut out: Vec<Target> = Vec::new();
    let mut add = |drv: &str, output: Option<&str>| {
        let drv = StorePath::parse(drv)
            .unwrap_or_else(|e| arg_error(format!("'{drv}' is not a store path: {}", e.0)));
        let wanted: Vec<String> = (output.into_iter())
            .flat_map(|o| o.split(',').filter(|o| !o.is_empty()).map(String::from))
            .collect();
        match out.iter_mut().find(|t| t.drv == drv) {
            Some(t) => {
                for o in wanted {
                    if !t.outputs.contains(&o) {
                        t.outputs.push(o);
                    }
                }
            }
            None => {
                let name = drv.name().to_string();
                out.push(Target::new(&name, drv, wanted));
            }
        }
    };
    if stdin {
        let mut text = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut text)
            .unwrap_or_else(|e| arg_error(format!("--roots -: cannot read stdin: {e}")));
        let v: serde_json::Value = serde_json::from_str(&text)
            .unwrap_or_else(|e| arg_error(format!("--roots -: not JSON: {e}")));
        let bad = || arg_error("--roots -: no \"roots\" and no \"drvs\" on stdin");
        if let Some(rs) = v.get("roots").and_then(|r| r.as_array()) {
            for r in rs {
                let Some(drv) = r.get("drv").and_then(serde_json::Value::as_str) else {
                    bad()
                };
                add(drv, r.get("output").and_then(serde_json::Value::as_str));
            }
        } else if let Some(ds) = v.get("drvs").and_then(|d| d.as_array()) {
            for d in ds {
                match d.as_str() {
                    Some(drv) => add(drv, None),
                    None => bad(),
                }
            }
        } else {
            bad()
        }
    }
    for a in args {
        let (drv, output) = match a.split_once('!') {
            Some((d, o)) => (d, Some(o)),
            None => (a.as_str(), None),
        };
        add(drv, output);
    }
    if out.is_empty() {
        arg_error("nothing to do: give DRV... or --roots -")
    }
    out
}

fn paths_of(targets: &[Target]) -> Vec<StorePath> {
    targets.iter().map(|t| t.drv.clone()).collect()
}

/// The plan of roots that are in `/nix/store`: this run evaluated nothing, so
/// every derivation and source is read from there.
fn plan_of(targets: &[Target]) -> crate::error::Result<crate::remote::plan::Plan> {
    apibuild::plan_with(&paths_of(targets), &HashMap::new(), &HashMap::new())
}

/// `--roots -` plus the flags `f` accepts; what is left are the root words.
fn split_flags(
    argv: &[String],
    mut f: impl FnMut(&str, &mut dyn FnMut() -> String) -> bool,
) -> (Vec<String>, bool) {
    let mut rest: Vec<String> = Vec::new();
    let mut stdin = false;
    let mut i = 0;
    while i < argv.len() {
        let s = argv[i].clone();
        let mut next = || {
            i += 1;
            value(argv, i, &s).clone()
        };
        if s == "--roots" {
            if next() != "-" {
                arg_error("--roots takes '-': the roots come from stdin")
            }
            stdin = true;
        } else if !f(&s, &mut next) {
            if s.starts_with('-') {
                help::unknown_flag(&s)
            }
            rest.push(s);
        }
        i += 1;
    }
    (rest, stdin)
}

// ---- the ops -----------------------------------------------------------------

fn op_plan(argv: &[String]) -> i32 {
    let (args, stdin) = split_flags(argv, |_, _| false);
    let targets = targets(&args, stdin);
    let plan = match plan_of(&targets) {
        Ok(p) => p,
        Err(e) => return e.report(),
    };
    let drvs: Vec<String> = (plan.drvs.iter())
        .map(|o| o.store_path().to_string())
        .collect();
    let sources: Vec<String> = plan.sources.iter().map(ToString::to_string).collect();
    println!(
        "{}",
        serde_json::json!({
            "drvs": drvs, "sources": sources, "storeDrvs": plan.store_drvs,
        })
    );
    0
}

fn op_missing(argv: &[String]) -> i32 {
    let mut url: Option<String> = None;
    let mut status = false;
    let (args, stdin) = split_flags(argv, |s, next| match s {
        "--builder" => {
            url = Some(next());
            true
        }
        "--check-cache-status" => {
            status = true;
            true
        }
        _ => false,
    });
    let targets = targets(&args, stdin);
    match missing(&targets, url.as_deref(), status) {
        Ok(v) => {
            crate::cli::render::bytes(&crate::cli::render::json(&v, false));
            0
        }
        Err(e) => e.report(),
    }
}

fn missing(
    targets: &[Target],
    url: Option<&str>,
    status: bool,
) -> crate::error::Result<serde_json::Value> {
    let cfg = apibuild::config();
    let transport = apibuild::transport(url, &cfg)?;
    let opts = crate::remote::worker::Options::default();
    let plan = plan_of(targets)?;
    let prov = crate::provenance::Provenance::off();
    let mut d = Dispatch::open(&transport, &opts, plan, prov, Style::detect())?;
    let m = d.missing(targets)?;
    let mut v = crate::api::render::Missing::of(&m, d.plan()).json();
    if status {
        v["roots"] = serde_json::Value::Array(cache_status(&transport, targets)?);
    }
    Ok(v)
}

/// `--check-cache-status`, per root: the verdict `eval-jobs` reports, from the
/// derivations in `/nix/store`.
fn cache_status(
    transport: &crate::remote::worker::Transport,
    targets: &[Target],
) -> crate::error::Result<Vec<serde_json::Value>> {
    use crate::jobs::line::{Job, Line};
    let mut lines: Vec<Line> = Vec::new();
    for t in targets {
        let drv = crate::store::drv::load_derivation_from_store(&t.drv)
            .map_err(|e| CrateError::Store(e.0))?;
        let mut outputs = std::collections::BTreeMap::new();
        for (name, o) in &drv.outputs {
            let p = o
                .path(&drv.name, name)
                .map_err(|e| CrateError::Store(e.0))?;
            outputs.insert(name.clone(), p);
        }
        lines.push(Line::Job(Job {
            attr: t.attr.clone(),
            attr_path: Vec::new(),
            drv_path: t.drv.clone(),
            outputs,
            constituents: Vec::new(),
            named_constituents: None,
            glob_constituents: false,
            extra: std::collections::BTreeMap::new(),
        }));
    }
    crate::jobs::cache::Resolver::open(transport)?.resolve(&mut lines)?;
    let raw = |j: &Job, k: &str| {
        (j.extra.get(k))
            .and_then(|b| serde_json::from_slice(b).ok())
            .unwrap_or(serde_json::Value::Null)
    };
    Ok(lines
        .iter()
        .filter_map(|l| match l {
            Line::Job(j) => Some(serde_json::json!({
                "drv": j.drv_path.to_string(),
                "cacheStatus": raw(j, "cacheStatus"),
                "neededBuilds": raw(j, "neededBuilds"),
                "neededSubstitutes": raw(j, "neededSubstitutes"),
            })),
            Line::Error(_) => None,
        })
        .collect())
}

struct BuildOpts {
    url: Option<String>,
    outputs: Vec<String>,
    sources: HashMap<String, std::path::PathBuf>,
    fetch_to: Option<std::path::PathBuf>,
    gc_root: Option<std::path::PathBuf>,
    no_build: bool,
}

fn op_build(argv: &[String]) -> i32 {
    let mut o = BuildOpts {
        url: None,
        outputs: Vec::new(),
        sources: HashMap::new(),
        fetch_to: None,
        gc_root: None,
        no_build: false,
    };
    let (args, stdin) = split_flags(argv, |s, next| match s {
        "--builder" => {
            o.url = Some(next());
            true
        }
        "--out" => {
            o.outputs.push(next());
            true
        }
        "--fetch-to" => {
            o.fetch_to = Some(next().into());
            true
        }
        "--gc-root" => {
            o.gc_root = Some(next().into());
            true
        }
        "--no-build" => {
            o.no_build = true;
            true
        }
        "--source" => {
            let spec = next();
            let Some((store, local)) = spec.split_once('=') else {
                arg_error(format!("--source wants STOREPATH=LOCAL, got '{spec}'"))
            };
            o.sources.insert(store.to_string(), local.into());
            true
        }
        _ => false,
    });
    let mut targets = targets(&args, stdin);
    for t in &mut targets {
        for name in &o.outputs {
            if !t.outputs.contains(name) {
                t.outputs.push(name.clone());
            }
        }
    }
    match build(&o, targets) {
        Ok(code) => code,
        Err(e) => e.report(),
    }
}

fn build(o: &BuildOpts, targets: Vec<Target>) -> crate::error::Result<i32> {
    let cfg = apibuild::config();
    let transport = apibuild::transport(o.url.as_deref(), &cfg)?;
    let opts = crate::remote::worker::Options::default();
    let plan = plan_of(&targets)?;
    let prov = crate::provenance::Provenance::off();
    let mut d = Dispatch::open(&transport, &opts, plan, prov, Style::detect())?
        .overrides(o.sources.clone());
    if o.no_build {
        d.upload_only(&paths_of(&targets))?;
        println!("{}", serde_json::json!({ "outcomes": [] }));
        return Ok(0);
    }
    let outcomes = d.submit(targets)?;
    if let Some(dir) = &o.fetch_to {
        for path in outcomes.iter().flat_map(|c| c.outputs.values()) {
            d.d.fetch(path, dir)?;
        }
    }
    let mut roots: Vec<String> = Vec::new();
    if let Some(dir) = &o.gc_root {
        // the links go on the local daemon whatever the builder is, which is
        // what brings the outputs back here (docs/classic.md)
        let links: Vec<(String, Option<String>)> = (outcomes.iter())
            .flat_map(|c| c.outputs.values())
            .map(|p| {
                let link = dir.join(
                    StorePath::parse(p).map_or_else(|_| p.clone(), |sp| sp.base_name().to_string()),
                );
                let link = link.to_string_lossy().into_owned();
                roots.push(link.clone());
                (p.clone(), Some(link))
            })
            .collect();
        d.d.out_links(&links)?;
    }
    let mut v = serde_json::json!({
        "outcomes": outcomes.iter().map(|c| serde_json::json!({
            "drv": c.drv.to_string(),
            "success": c.success,
            "outputs": c.outputs,
            "reason": c.reason,
            "logTail": c.log_tail,
        })).collect::<Vec<_>>(),
    });
    if o.gc_root.is_some() {
        v["gcRoots"] = serde_json::json!(roots);
    }
    println!("{v}");
    Ok(d.d.exit)
}

// ---- fetch -------------------------------------------------------------------

/// A URL is a positional this op fetches itself; anything else is an
/// installable, which `parse_eval_args` reads.
pub fn is_url(s: &str) -> bool {
    ["https://", "http://", "file://"]
        .iter()
        .any(|p| s.starts_with(p))
}

/// One line on stdout: the hash and where it came from, as JSON or as the
/// hash alone.
pub fn hash_line(json: bool, hash: &str, key: &'static str, value: &str) -> i32 {
    let f = crate::api::render::Fetched {
        hash: hash.to_string(),
        key,
        origin: value.to_string(),
    };
    crate::cli::render::emit(&f, json);
    0
}

/// The hash the one fixed-output derivation an installable names really
/// produces. `ietsp prefetch` is the other frontend.
pub fn hash_of_drv(ev: &mut Evaluator, a: EvalArgs, json: bool) -> i32 {
    let mut roots = Vec::new();
    if let Err(e) = evaluate(ev, a).and_then(|v| drv_roots(ev, v, &mut roots)) {
        return report(ev, &e);
    }
    let [(root, _)] = roots.as_slice() else {
        let msg = format!("fetch wants one derivation, got {}", roots.len());
        return crate::log::Report::new("usage", msg).emit();
    };
    match crate::api::fetch::drv_hash(ev, root) {
        Ok((got, p)) => hash_line(json, &got, "drvPath", &p.to_full_string()),
        Err(e) => e.report(),
    }
}

/// The flags `fetch` and `ietsp prefetch` share, and what is left of the argv.
pub fn fetch_flags(argv: &[String]) -> (bool, crate::store::hash::HashAlgo, Vec<String>) {
    let (mut unpack, mut algo) = (false, crate::store::hash::HashAlgo::Sha256);
    let mut rest: Vec<String> = Vec::new();
    let mut i = 0;
    while i < argv.len() {
        let s = argv[i].as_str();
        let mut next = || {
            i += 1;
            value(argv, i, s).clone()
        };
        match s {
            "--unpack" => unpack = true,
            "--hash-type" => {
                let v = next();
                algo = crate::store::hash::HashAlgo::parse(&v)
                    .unwrap_or_else(|| arg_error(format!("unknown hash algorithm '{v}'")));
            }
            s => rest.push(s.to_string()),
        }
        i += 1;
    }
    (unpack, algo, rest)
}

/// A URL or an installable becomes a hash: the whole of both frontends past
/// their own flags.
pub fn fetch(
    unpack: bool,
    algo: crate::store::hash::HashAlgo,
    rest: Vec<String>,
    json: bool,
) -> i32 {
    let Some(at) = rest.iter().position(|s| is_url(s)) else {
        if rest.is_empty() {
            arg_error("nothing to fetch: give a URL, -E EXPR, FILE or FLAKEREF#ATTR")
        }
        if unpack {
            arg_error("--unpack applies to a URL; a derivation says how it is hashed")
        }
        let a = parse_eval_args(&rest);
        return crate::cli::run_parallel(run_for(&a), move |ev, _| hash_of_drv(ev, a, json));
    };
    let mut rest = rest;
    let url = rest.remove(at);
    // what is left are flags only (--log-format, --show-trace, …)
    parse_eval_args(&rest);
    match crate::api::fetch::url_hash(&url, unpack, algo) {
        Ok(h) => hash_line(json, &h.to_sri(), "url", &url),
        Err(e) => e.report(),
    }
}

fn op_fetch(argv: &[String]) -> i32 {
    let (unpack, algo, rest) = fetch_flags(argv);
    fetch(unpack, algo, rest, true)
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_url_is_the_positional_fetch_takes_itself() {
        assert!(super::is_url("file:///tmp/a.tar.gz"));
        assert!(super::is_url("https://example.org/a.tar.gz"));
        assert!(!super::is_url("github:numtide/treefmt"));
        assert!(!super::is_url("./default.nix"));
    }
}

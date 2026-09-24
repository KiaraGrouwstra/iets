//! `ietsp plumbing OP`: the machine-first commands. One operation each,
//! JSON on stdout, a stable contract (docs/plumbing.md).

use crate::api::installable::{self, Prefixes, Want};
use crate::cli::help::{self, arg_error, value};
use crate::cli::{EvalArgs, Input, evaluate, parse_eval_args, report, run_for};
use crate::eval::{Error, Evaluator};
use crate::runtime::Word;
use crate::store::locate::drv_roots;

pub const HELP: &str = "\
usage: ietsp plumbing OP [FLAGS] ...

The machine-first operations the user-facing commands are written against.
Each one writes JSON on stdout (docs/plumbing.md).

operations:
  resolve   an installable becomes derivation roots

usage: ietsp plumbing resolve [FLAGS] (-E EXPR | FILE | FLAKEREF[#FRAGMENT])

Print {\"attrPaths\", \"roots\"}: the attribute paths that were tried, and
one object per derivation root with \"drv\", \"output\" and \"attr\".

  --prefixes WHICH      the attribute paths a flake fragment is tried at:
                        package (the default), dev-shell, package-set, or
                        none for the fragment itself
  --want first|all      stop at the first path that is there, or take all
  --no-drvs             report the attribute paths only
";

pub fn main(argv: &[String]) -> i32 {
    match argv.first().map(String::as_str) {
        Some("resolve") => resolve(&argv[1..]),
        w => help::unknown_subcommand(w, &["resolve"]),
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
    println!(
        "{}",
        serde_json::json!({ "attrPaths": paths, "roots": roots })
    );
    Ok(())
}

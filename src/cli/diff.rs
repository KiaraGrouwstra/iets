//! `ietsp diff A B`: what differs between two derivations (docs/diff.md).

use crate::cli::help::{arg_error, unknown_flag, value};
use crate::cli::{EvalArgs, EvalFlags, Input, eval_flag, evaluate, report};
use crate::error::{Error, Result};
use crate::store::drv::{Derivation, DrvError, load_derivation_from_store};
use crate::store::locate::drv_roots;
use crate::store::path::StorePath;

struct Opts {
    json: bool,
    depth: usize,
}

/// One of the two things being compared: a `.drv` in the store, or an
/// evaluation that must give exactly one derivation.
enum Side {
    Drv(StorePath),
    Eval(Box<EvalArgs>),
}

/// 0 when the two are equal, 1 when they differ, 2 for a usage error or any
/// other failure (docs/cli.md).
pub fn cmd_diff(argv: &[String]) -> i32 {
    let (o, sides) = parse(argv);
    let (aterms, roots) = match resolve(sides) {
        Ok(r) => r,
        Err(code) => return code.max(2),
    };
    // a derivation this run evaluated, else the one in /nix/store
    let mut load = |p: &StorePath| -> std::result::Result<Derivation, DrvError> {
        match aterms.get(p) {
            Some(t) => {
                Derivation::parse(&String::from_utf8_lossy(t), Derivation::name_from_path(p)?)
            }
            None => load_derivation_from_store(p),
        }
    };
    match run(&o, &mut load, &roots) {
        Ok(code) => code,
        Err(e) => e.report().max(2),
    }
}

fn parse(argv: &[String]) -> (Opts, Vec<Side>) {
    let mut o = Opts {
        json: false,
        depth: 2,
    };
    let mut f = EvalFlags::default();
    let mut pos: Vec<String> = Vec::new();
    let mut expr: Option<String> = None;
    let mut i = 0;
    while i < argv.len() {
        let s = argv[i].as_str();
        let mut next = || {
            i += 1;
            value(argv, i, s).clone()
        };
        match s {
            _ if eval_flag(&mut f, s, || Ok(next())).unwrap_or_else(|e| arg_error(e)) => {}
            "--json" => o.json = true,
            "--all" => o.depth = usize::MAX,
            "--depth" => {
                let v = next();
                o.depth = v
                    .parse()
                    .unwrap_or_else(|_| arg_error(format!("--depth wants a number, got '{v}'")));
            }
            "-E" | "--expr" => expr = Some(next()),
            _ if s.starts_with('-') && s != "-" => unknown_flag(s),
            _ => pos.push(s.to_string()),
        }
        i += 1;
    }
    // one side: the positional `p` (a .drv in the store, a flake reference
    // or a file), or `-E EXPR`, with the attribute `attr` selected
    let side = |p: Option<&String>, attr: Option<String>| match (p, &expr) {
        (Some(p), _) if p.ends_with(".drv") => match StorePath::from_base_name(base_name(p)) {
            Ok(sp) if sp.is_derivation() => Side::Drv(sp),
            Ok(sp) => arg_error(format!("'{sp}' is not a derivation path")),
            Err(e) => arg_error(e.0),
        },
        (Some(_), Some(_)) => arg_error("-E and FILE cannot be combined"),
        (p, e) => Side::Eval(Box::new(EvalArgs {
            input: match (p, e) {
                (Some(p), _) if crate::flake::is_flake_ref(p) => Some(Input::Flake(p.clone())),
                (Some(p), _) => Some(Input::File(p.clone())),
                (None, Some(e)) => Some(Input::Expr(e.clone())),
                (None, None) => None,
            },
            attr,
            includes: f.includes.clone(),
            args: f.args.clone(),
            pure: f.pure,
            ..EvalArgs::new()
        })),
    };
    let sides = match (f.attrs.len(), pos.len()) {
        (2, 0..=1) => (f.attrs.iter())
            .map(|at| side(pos.first(), Some(at.clone())))
            .collect(),
        (0, 2) => vec![side(Some(&pos[0]), None), side(Some(&pos[1]), None)],
        _ => {
            arg_error("give two derivations (A B), or one expression, file or flake with -A X -A Y")
        }
    };
    (o, sides)
}

/// `/nix/store/<hash>-<name>.drv`, `./<hash>-<name>.drv` or the base name.
fn base_name(arg: &str) -> &str {
    arg.rsplit('/').next().unwrap_or(arg)
}

fn run(o: &Opts, load: &mut crate::store::drv::DrvLoader<'_>, roots: &[StorePath]) -> Result<i32> {
    let (a, b) = (&roots[0], &roots[1]);
    let d = crate::store::drvdiff::diff(load, a, b, o.depth)?;
    if o.json {
        let mut v = d.json();
        v["equal"] = d.is_empty().into();
        println!("{v}");
    } else if d.is_empty() {
        println!("{a} and {b} are the same derivation");
    } else {
        print!("{}", d.text());
    }
    Ok(i32::from(!d.is_empty()))
}

type Aterms = std::collections::HashMap<StorePath, std::sync::Arc<Vec<u8>>>;

/// The two root derivations, and the ATerms an evaluation produced for the
/// derivations under them; the rest are read from `/nix/store`.
fn resolve(sides: Vec<Side>) -> std::result::Result<(Aterms, Vec<StorePath>), i32> {
    let first = sides.iter().find_map(|s| match s {
        Side::Eval(a) => Some((a.workers, a.evaluator())),
        Side::Drv(_) => None,
    });
    let Some((workers, ev)) = first else {
        let paths = sides
            .into_iter()
            .map(|s| match s {
                Side::Drv(p) => p,
                Side::Eval(_) => unreachable!("no evaluated side"),
            })
            .collect();
        return Ok((Aterms::new(), paths));
    };
    crate::parallel::run(workers, ev, move |ev| {
        let mut roots = Vec::new();
        for s in sides {
            let a = match s {
                Side::Drv(p) => {
                    roots.push(p);
                    continue;
                }
                Side::Eval(a) => *a,
            };
            let mut found = Vec::new();
            let r = evaluate(ev, a).and_then(|v| drv_roots(ev, v, &mut found));
            if let Err(e) = r {
                return Err(report(ev, &e));
            }
            let [(p, _)] = &found[..] else {
                let n = found.len();
                return Err(Error::Usage(format!(
                    "each side must be one derivation, this one is {n}"
                ))
                .report());
            };
            roots.push(p.clone());
        }
        let aterms = (ev.drv.drvs.lock().iter())
            .filter_map(|(p, i)| i.aterm.clone().map(|t| (p.clone(), t)))
            .collect();
        Ok((aterms, roots))
    })
}

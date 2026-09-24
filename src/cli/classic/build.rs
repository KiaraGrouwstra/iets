//! `nix-build` (nix-build.cc). Evaluates, uploads the closure, builds every
//! root in one `BuildPathsWithResults`, makes the `result` symlinks and prints
//! the output paths.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use super::{Argv, Common, Root, common_flag, fallback};
use crate::build::render::Style;
use crate::build::upload::PlanUpload;
use crate::build::{Dispatcher, Target};
use crate::cli::{Outcome, abs_path, auto_args, cwd};
use crate::eval::Evaluator;
use crate::provenance::Provenance;
use crate::remote::plan::Plan;
use crate::remote::worker::{Options, Transport};

const NAME: &str = "nix-build";

#[derive(Default)]
struct Build {
    c: Common,
    out_link: String,
    no_out_link: bool,
}

fn parse(argv: &[String]) -> Result<Build, String> {
    let mut b = Build {
        out_link: "./result".into(),
        ..Build::default()
    };
    let mut a = Argv { all: argv, i: 0 };
    while a.i < argv.len() {
        let s = argv[a.i].clone();
        match s.as_str() {
            "-o" | "--out-link" => b.out_link = a.value("--out-link")?,
            "--no-out-link" | "--no-link" => b.no_out_link = true,
            "--add-drv-link" => {}
            "-Q" | "--no-build-output" => b.c.quiet = true,
            "-" => return Err("reading the expression from stdin is not implemented".into()),
            "--dry-run" | "--drv-link" | "--run-env" | "--command" | "--run" | "--repair"
            | "--check" | "--pure" | "--exclude" | "--keep" | "-p" | "--packages" => {
                return Err(format!("'{s}' is not implemented"));
            }
            _ => {
                if !common_flag(&mut b.c, &mut a, &s)? {
                    if s.starts_with('-') {
                        return Err(format!("'{s}' is not implemented"));
                    }
                    b.c.files.push(s);
                }
            }
        }
        a.i += 1;
    }
    if b.c.files.is_empty() && !b.c.from_args {
        b.c.files.push(".".into());
    }
    if b.c.flags.attrs.is_empty() {
        b.c.flags.attrs.push(String::new());
    }
    Ok(b)
}

enum Built {
    Ready(Vec<Root>, Box<Plan>, Box<Provenance>),
    Empty,
}

pub fn main(argv: &[String]) -> i32 {
    let b = match parse(argv) {
        Ok(b) => b,
        Err(reason) => fallback(NAME, argv, &reason),
    };
    let (cfg, transport, options) = match super::settings(&b.c) {
        Ok(t) => t,
        Err(reason) => fallback(NAME, argv, &reason),
    };
    let ev = super::evaluator(&b.c, &cfg);
    let b = Arc::new(b);
    let build = Arc::clone(&b);
    match crate::cli::run_reported(crate::cli::Run::on(ev), move |ev, _| run(ev, &build)) {
        Outcome::Failed(code) => code,
        Outcome::Unsupported(reason) => fallback(NAME, argv, &reason),
        Outcome::Ok(Built::Empty) => 0,
        Outcome::Ok(Built::Ready(roots, plan, prov)) => {
            realise(&b, &transport, &options, &roots, &plan, &prov)
        }
    }
}

fn run(ev: &mut Evaluator, b: &Build) -> crate::error::Result<Built> {
    let cwd = cwd();
    let mut roots: Vec<Root> = Vec::new();
    let mut seen = HashSet::new();
    let args = auto_args(ev, &b.c.flags.args, &cwd)?;
    for file in &b.c.files {
        let root = super::eval_input(ev, &b.c, file, &cwd)?;
        for attr in &b.c.flags.attrs {
            let v = super::select(ev, root, attr, args)?;
            super::derivations_at(ev, v, args, &mut roots, &mut seen, attr)?;
        }
    }
    if roots.is_empty() {
        return Ok(Built::Empty);
    }
    let prov = Provenance::snapshot(ev);
    let plan = super::plan_roots(ev, &roots)?;
    Ok(Built::Ready(roots, Box::new(plan), Box::new(prov)))
}

/// Uploads the closure, builds every root in one call, links and prints.
fn realise(
    b: &Build,
    transport: &Transport,
    options: &Options,
    roots: &[Root],
    plan: &Plan,
    prov: &Provenance,
) -> i32 {
    let style = Style {
        quiet: b.c.quiet,
        ..Style::detect()
    };
    let mut d = match Dispatcher::open(transport, options, prov.clone(), style) {
        Ok(d) => d,
        Err(e) => return e.report(),
    };
    // nix-build asks for every root in one op, one output per root; roots
    // that share a derivation become one target with both outputs.
    let mut targets: Vec<Target> = Vec::new();
    for r in roots {
        match targets.iter_mut().find(|t| t.drv == r.drv) {
            Some(t) => {
                if !t.outputs.contains(&r.output) {
                    t.outputs.push(r.output.clone());
                }
            }
            None => {
                // The bare attribute: the dispatcher adds the position, once.
                let name = prov
                    .attr_of(&r.drv.to_string())
                    .unwrap_or_else(|| crate::provenance::drv_name(&r.drv.to_string()));
                targets.push(Target::new(&name, r.drv.clone(), vec![r.output.clone()]));
            }
        }
    }
    let mut up = PlanUpload::new(plan, &super::locate_store);
    let outcomes = match d.submit(&mut up, targets) {
        Ok(o) => o,
        Err(e) => return e.report(),
    };
    // a daemon client nix exits 1 on a failed build, whatever the status
    if d.exit != 0 {
        return 1;
    }
    let maps: HashMap<String, std::collections::BTreeMap<String, Option<String>>> = outcomes
        .iter()
        .map(|o| {
            (
                o.drv.to_string(),
                o.outputs
                    .iter()
                    .map(|(n, p)| (n.clone(), Some(p.clone())))
                    .collect(),
            )
        })
        .collect();

    // nix-build.cc: the n-th distinct drv gets the suffix `-n` from 2 on; an
    // output other than `out` adds `-<output>`.
    let mut counter: HashMap<String, usize> = HashMap::new();
    for r in roots {
        let n = counter.len();
        counter.entry(r.drv.to_string()).or_insert(n);
    }
    let cwd = cwd();
    let mut out = String::new();
    let mut links: Vec<(String, Option<String>)> = Vec::new();
    for r in roots {
        let drv = r.drv.to_string();
        let path = maps
            .get(&drv)
            .and_then(|m| m.get(&r.output).cloned())
            .flatten();
        let Some(path) = path else {
            return crate::error::Error::Store(format!(
                "derivation '{drv}' has no output '{}'",
                r.output
            ))
            .report();
        };
        if b.no_out_link {
            if !super::store_given(&b.c) {
                links.push((path.clone(), None));
            }
        } else {
            let mut link = b.out_link.clone();
            let c = counter[&drv];
            if c > 0 {
                link.push_str(&format!("-{}", c + 1));
            }
            if r.output != "out" {
                link.push('-');
                link.push_str(&r.output);
            }
            let link = String::from_utf8_lossy(&abs_path(&link, &cwd)).into_owned();
            links.push((path.clone(), Some(link)));
        }
        out.push_str(&path);
        out.push('\n');
    }
    if let Err(e) = d.out_links(&links) {
        return e.report();
    }
    use std::io::Write;
    let mut stdout = std::io::stdout();
    let _ = stdout.write_all(out.as_bytes());
    let _ = stdout.flush();
    0
}

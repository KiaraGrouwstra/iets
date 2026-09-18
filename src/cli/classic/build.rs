//! `nix-build` (nix-build.cc). Evaluates, uploads the closure, builds every
//! root in one `BuildPathsWithResults`, makes the `result` symlinks and prints
//! the output paths.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use super::{Argv, Common, Root, common_flag, fallback, is_unsupported};
use crate::cli::{abs_path, auto_args, cwd, report};
use crate::eval::{Error, Evaluator};
use crate::remote::event::Event;
use crate::remote::plan::Plan;
use crate::remote::worker::{Connection, Options, Transport};

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
    if b.c.attr_paths.is_empty() {
        b.c.attr_paths.push(String::new());
    }
    Ok(b)
}

enum Run {
    Failed(i32),
    Unsupported(String),
    Ready(Vec<Root>, Box<Plan>),
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
    match crate::parallel::run(crate::parallel::default_workers(), ev, move |ev| {
        run(ev, &build)
    }) {
        Run::Failed(code) => code,
        Run::Unsupported(reason) => fallback(NAME, argv, &reason),
        Run::Empty => 0,
        Run::Ready(roots, plan) => realise(&b, &transport, &options, &roots, &plan),
    }
}

fn run(ev: &mut Evaluator, b: &Build) -> Run {
    let cwd = cwd();
    let mut roots: Vec<Root> = Vec::new();
    let mut seen = HashSet::new();
    let r = (|| -> Result<(), Box<Error>> {
        let args = auto_args(ev, &b.c.args, &cwd)?;
        for file in &b.c.files {
            let root = super::eval_input(ev, &b.c, file, &cwd)?;
            for attr in &b.c.attr_paths {
                let v = super::select(ev, root, attr, args)?;
                super::derivations(ev, v, args, &mut roots, &mut seen)?;
            }
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
    if roots.is_empty() {
        return Run::Empty;
    }
    match super::plan_roots(ev, &roots) {
        Ok(p) => Run::Ready(roots, Box::new(p)),
        Err(e) => {
            eprintln!("error: {e}");
            Run::Failed(1)
        }
    }
}

/// Uploads the closure, builds every root in one call, links and prints.
fn realise(
    b: &Build,
    transport: &Transport,
    options: &Options,
    roots: &[Root],
    plan: &Plan,
) -> i32 {
    let quiet = b.c.quiet;
    let failed = std::cell::Cell::new(false);
    let mut on_event = |e: Event| match e {
        Event::Started(s) => eprintln!("building '{}'...", s.drv_path),
        Event::Log(l) if !quiet => eprintln!("{}", String::from_utf8_lossy(&l.line)),
        Event::Failed(f) => {
            failed.set(true);
            eprintln!("error: build of '{}' failed: {}", f.drv_path, f.reason);
        }
        _ => {}
    };
    let mut conn = match Connection::open_with(transport, options, &mut on_event) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    let upload = crate::remote::worker::build::build_on(
        &mut conn,
        plan,
        &roots[0].drv,
        &[],
        true,
        None,
        &mut on_event,
    );
    if let Err(e) = upload {
        eprintln!("error: {e}");
        return 1;
    }

    // nix-build asks for every root in one buildPaths, one output per root.
    let targets: Vec<String> = roots
        .iter()
        .map(|r| format!("{}!{}", r.drv, r.output))
        .collect();
    let results = match conn.build_paths_with_results(&targets, &mut on_event) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    for r in &results {
        if !r.success() {
            failed.set(true);
            eprintln!("error: build failed: status {}: {}", r.status, r.msg);
        }
    }
    if failed.get() {
        return 1;
    }

    let mut maps: HashMap<String, std::collections::BTreeMap<String, Option<String>>> =
        HashMap::new();
    for r in roots {
        let drv = r.drv.to_string();
        if maps.contains_key(&drv) {
            continue;
        }
        match conn.query_derivation_output_map(&drv, &mut on_event) {
            Ok(m) => {
                maps.insert(drv, m);
            }
            Err(e) => {
                eprintln!("error: {e}");
                return 1;
            }
        }
    }

    // nix-build.cc: the n-th distinct drv gets the suffix `-n` from 2 on; an
    // output other than `out` adds `-<output>`.
    let mut counter: HashMap<String, usize> = HashMap::new();
    for r in roots {
        let n = counter.len();
        counter.entry(r.drv.to_string()).or_insert(n);
    }
    let cwd = cwd();
    let mut out = String::new();
    for r in roots {
        let drv = r.drv.to_string();
        let path = maps
            .get(&drv)
            .and_then(|m| m.get(&r.output).cloned())
            .flatten();
        let Some(path) = path else {
            eprintln!("error: derivation '{drv}' has no output '{}'", r.output);
            return 1;
        };
        if !b.no_out_link {
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
            if let Err(e) = conn.add_perm_root(&path, std::path::Path::new(&link)) {
                eprintln!("error: {e}");
                return 1;
            }
        }
        out.push_str(&path);
        out.push('\n');
    }
    use std::io::Write;
    let mut stdout = std::io::stdout();
    let _ = stdout.write_all(out.as_bytes());
    let _ = stdout.flush();
    0
}

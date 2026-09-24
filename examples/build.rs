//! Evaluate to derivations, plan the upload, build through the dispatcher,
//! print the output paths.
//!
//! `cargo run --example build -- [--builder URL] [-I prefix=path]... [-A attr] (-E EXPR | FILE)`
//!
//! Uses the library only (`ietsp::api`), never `ietsp::cli`.

use std::collections::HashMap;

use ietsp::api::{self, EvalArgs, Input};
use ietsp::build::render::Style;
use ietsp::build::upload::PlanUpload;
use ietsp::build::{Dispatcher, Target};
use ietsp::eval::Evaluator;
use ietsp::remote::{plan, worker};
use ietsp::store::locate::{drv_roots, locate_in_store};
use ietsp::store::path::StorePath;

fn main() -> std::process::ExitCode {
    // a frontend owns the run state and says where messages go
    let session = ietsp::session::Session::new();
    session.log.write_to_stderr();
    let _entered = session.enter();
    let mut url = "daemon".to_string();
    let mut a = EvalArgs::new();
    let mut argv = std::env::args().skip(1);
    while let Some(arg) = argv.next() {
        let mut next = || {
            argv.next()
                .unwrap_or_else(|| die(&format!("{arg} needs a value")))
        };
        match arg.as_str() {
            "--builder" => url = next(),
            "-I" | "--include" => a.includes.push(api::parse_include(&next())),
            "-E" | "--expr" => a.input = Some(Input::Expr(next())),
            "-A" | "--attr" => a.attr = Some(next()),
            f if f.starts_with('-') && f != "-" => die(&format!("unknown flag {f}")),
            f => a.input = Some(Input::File(f.to_string())),
        }
    }
    if a.input.is_none() {
        die("give -E EXPR or a FILE");
    }

    let workers = a.workers;
    let r = ietsp::parallel::try_run(workers, Evaluator::new(), move |ev| {
        eval_and_build(ev, a, url)
    });
    match r {
        Ok(c) => std::process::ExitCode::from(c),
        Err(e) => {
            eprintln!("error: {e}");
            std::process::ExitCode::from(1)
        }
    }
}

fn eval_and_build(ev: &mut Evaluator, a: EvalArgs, url: String) -> u8 {
    let mut roots = Vec::new();
    if let Err(e) = api::evaluate(ev, a).and_then(|v| drv_roots(ev, v, &mut roots)) {
        api::report(ev, &e);
        return 1;
    }
    // `--out` would add wanted outputs; the roots' string context is enough here.
    let (root_paths, outputs) = api::build_targets(&[], &roots);
    let prov = ietsp::provenance::Provenance::snapshot(ev);

    // Every derivation and source this evaluation created, straight from
    // memory: the plan reads `/nix/store` only for what it did not create.
    let drvs = ev.drv.drvs.lock();
    let sources = ev.known.origins();
    let inputs = plan::Inputs {
        drvs: &drvs,
        sources: &sources,
    };
    let no_overrides = HashMap::new();
    let locate = |paths: &[StorePath]| locate_in_store(paths, &no_overrides);
    let plan = match plan::plan(&root_paths, inputs) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    let Some(transport) = worker::Transport::parse(&url) else {
        eprintln!("error: unsupported builder '{url}'");
        return 2;
    };

    let mut d = match Dispatcher::open(
        &transport,
        &worker::Options::default(),
        prov,
        Style::detect(),
    ) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    let targets: Vec<Target> = root_paths
        .iter()
        .map(|r| Target::new(r.name(), r.clone(), outputs.clone()))
        .collect();
    let outcomes = match d.submit(&mut PlanUpload::new(&plan, &locate), targets) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    for o in &outcomes {
        for path in o.outputs.values() {
            println!("{path}");
        }
    }
    d.exit as u8
}

fn die(msg: &str) -> ! {
    eprintln!("usage: build [--builder URL] [-I prefix=path]... [-A attr] (-E EXPR | FILE): {msg}");
    std::process::exit(2)
}

//! Evaluate to derivations, plan the upload, build through a Nix worker,
//! print the events and the output paths.
//!
//! `cargo run --example build -- [--builder URL] [-I prefix=path]... [-A attr] (-E EXPR | FILE)`
//!
//! Uses the library only (`ietsp::api`), never `ietsp::cli`.

use std::collections::HashMap;

use ietsp::api::{self, EvalArgs, Input};
use ietsp::eval::Evaluator;
use ietsp::remote::event::Event;
use ietsp::remote::{plan, worker};
use ietsp::store::locate::{drv_roots, locate_in_store};
use ietsp::store::path::StorePath;

fn main() -> std::process::ExitCode {
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
    let sources = ev.source_origins();
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

    let mut built: Vec<(String, String)> = Vec::new();
    let mut failed = false;
    for root in &root_paths {
        let mut on_event = |e: Event| match e {
            Event::Started(s) => match prov.started(&s.drv_path) {
                Some(l) => eprintln!("{l}"),
                None => eprintln!("building {}", s.drv_path),
            },
            Event::Log(l) => {
                let line = prov.rewrite(&l.line);
                eprintln!("{}", String::from_utf8_lossy(&line));
            }
            Event::Built(b) => built.push((b.output, b.store_path)),
            Event::Failed(f) => {
                failed = true;
                eprintln!("error: build of {} failed: {}", f.drv_path, f.reason);
            }
            Event::Done(d) => failed |= !d.success,
            Event::Phase(_) | Event::Stopped(_) => {}
        };
        let r = worker::build::build(
            &transport,
            &plan,
            &locate,
            root,
            &outputs,
            false,
            None,
            &mut on_event,
        );
        if let Err(e) = r {
            eprintln!("error: {e}");
            return 1;
        }
    }
    for (name, path) in &built {
        if outputs.is_empty() || outputs.contains(name) {
            println!("{path}");
        }
    }
    u8::from(failed)
}

fn die(msg: &str) -> ! {
    eprintln!("usage: build [--builder URL] [-I prefix=path]... [-A attr] (-E EXPR | FILE): {msg}");
    std::process::exit(2)
}

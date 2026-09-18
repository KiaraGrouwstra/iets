//! `ietsp build`: evaluate to derivations and build them through a daemon.

use std::collections::HashMap;

use crate::cli::{EvalArgs, Input, evaluate, parse_eval_args, report, usage};
use crate::error::Error as CrateError;
use crate::eval::Evaluator;
use crate::store::locate::{drv_roots, locate_in_store};
use crate::trace::run::Cache;

struct BuildArgs {
    url: String,
    timings: bool,
    no_build: bool,
    fetch_to: Option<std::path::PathBuf>,
    outputs: Vec<String>,
    sources: HashMap<String, std::path::PathBuf>,
}

/// `ietsp build`: evaluate to derivations and ship their closure from memory, or (legacy)
/// take a `.drv` path and read its closure from `/nix/store`.
pub fn cmd_build(argv: &[String]) -> i32 {
    let mut b = BuildArgs {
        url: "daemon".to_string(),
        timings: false,
        no_build: false,
        fetch_to: None,
        outputs: Vec::new(),
        sources: HashMap::new(),
    };
    let mut rest: Vec<String> = Vec::new();
    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
            "--builder" => {
                i += 1;
                b.url = argv.get(i).cloned().unwrap_or_else(|| usage());
            }
            "--timings" => b.timings = true,
            "--no-build" => b.no_build = true,
            "--fetch-to" => {
                i += 1;
                b.fetch_to = Some(std::path::PathBuf::from(
                    argv.get(i).cloned().unwrap_or_else(|| usage()),
                ));
            }
            "--out" => {
                i += 1;
                b.outputs
                    .push(argv.get(i).cloned().unwrap_or_else(|| usage()));
            }
            "--source" => {
                i += 1;
                let spec = argv.get(i).unwrap_or_else(|| usage());
                let (store, local) = spec.split_once('=').unwrap_or_else(|| usage());
                b.sources
                    .insert(store.to_string(), std::path::PathBuf::from(local));
            }
            s => rest.push(s.to_string()),
        }
        i += 1;
    }
    let a = parse_eval_args(&rest);
    if let Some(Input::File(f)) = &a.input
        && f.ends_with(".drv")
    {
        return build_drv_file(&b, f);
    }

    let t0 = std::time::Instant::now();
    let mut ev = Evaluator::new();
    let a = EvalArgs {
        timings: a.timings || b.timings,
        ..a
    };
    let cache = super::setup_cache(&a, "build", &ev);
    if let Some(c) = &cache
        && c.lookup()
        && let Some(hit) = c.read_build()
    {
        let (roots, drvs, sources) = hit;
        if b.timings {
            eprintln!(
                "eval: skipped, {} derivations and {} sources from the cache, {:.3}s",
                drvs.len(),
                sources.len(),
                t0.elapsed().as_secs_f64()
            );
        }
        let (root_paths, outputs) = build_targets(&b, &roots);
        let inputs = crate::remote::plan::Inputs {
            drvs: &drvs,
            sources: &sources,
        };
        return build_roots(&b, &root_paths, &outputs, inputs);
    }
    if cache.is_some() {
        ev.trace = crate::trace::Trace::enabled();
    }
    // parallel: eval and build run as the main fiber
    crate::parallel::run(a.workers, ev, move |ev| build_eval(ev, &b, a, t0, cache))
}

/// The drv paths to build and the outputs to print: `--out` plus the outputs the roots' context named.
fn build_targets(
    b: &BuildArgs,
    roots: &[(crate::store::path::StorePath, Option<String>)],
) -> (Vec<crate::store::path::StorePath>, Vec<String>) {
    let mut outputs = b.outputs.clone();
    for (_, out) in roots {
        if let Some(o) = out
            && !outputs.contains(o)
        {
            outputs.push(o.clone());
        }
    }
    (roots.iter().map(|(p, _)| p.clone()).collect(), outputs)
}

fn build_eval(
    ev: &mut Evaluator,
    b: &BuildArgs,
    a: EvalArgs,
    t0: std::time::Instant,
    cache: Option<Cache>,
) -> i32 {
    let mut roots = Vec::new();
    let r = evaluate(ev, a).and_then(|v| drv_roots(ev, v, &mut roots));
    if let Err(e) = r {
        report(ev, &e);
        return 1;
    }
    let eval_time = t0.elapsed();
    if b.timings {
        eprintln!(
            "eval: {} derivations, {} sources, {:.3}s",
            ev.drv.nr_derivations,
            ev.source_origins().len(),
            eval_time.as_secs_f64()
        );
    }
    if let Some(c) = &cache {
        c.write(ev, &crate::trace::wire::encode_roots(&roots), true);
    }
    let (root_paths, outputs) = build_targets(b, &roots);
    let drvs = ev.drv.drvs.lock();
    let sources = ev.source_origins();
    let inputs = crate::remote::plan::Inputs {
        drvs: &drvs,
        sources: &sources,
    };
    build_roots(b, &root_paths, &outputs, inputs)
}

/// Legacy mode: `DRV` is a `.drv` store path, or a copy of the file named
/// `<hash>-<name>.drv`; its closure is read from `/nix/store`.
fn build_drv_file(b: &BuildArgs, root_arg: &str) -> i32 {
    use crate::builtins::derivation::DrvInfo;
    use crate::store::drv::Derivation;
    use crate::store::path::StorePath;
    let base = std::path::Path::new(root_arg)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let root = match StorePath::from_base_name(&base) {
        Ok(p) if p.is_derivation() => p,
        Ok(p) => {
            eprintln!("error: '{p}' is not a derivation");
            return 1;
        }
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    // The root may be a copy outside the store; its inputs are read from `/nix/store`.
    let info = std::fs::read_to_string(root_arg)
        .map_err(CrateError::from)
        .and_then(|text| {
            let name = Derivation::name_from_path(&root)?;
            let drv = Derivation::parse(&text, name)?;
            let mut references = drv.input_srcs;
            references.extend(drv.input_drvs.into_keys());
            Ok(DrvInfo {
                outputs: drv.outputs.into_keys().collect(),
                references,
                aterm: Some(text.into_bytes()),
                pos: None,
                attr: None,
            })
        });
    let info = match info {
        Ok(i) => i,
        Err(e) => {
            eprintln!("error: cannot read derivation '{root_arg}': {e}");
            return 1;
        }
    };
    let drvs = HashMap::from([(root.clone(), info)]);
    let sources = HashMap::new();
    build_roots(
        b,
        &[root],
        &b.outputs,
        crate::remote::plan::Inputs {
            drvs: &drvs,
            sources: &sources,
        },
    )
}

fn build_roots(
    b: &BuildArgs,
    roots: &[crate::store::path::StorePath],
    outputs: &[String],
    inputs: crate::remote::plan::Inputs<'_>,
) -> i32 {
    use crate::remote::event::Event;
    use crate::store::path::StorePath;

    let Some(transport) = crate::remote::worker::Transport::parse(&b.url) else {
        eprintln!(
            "error: unsupported builder '{}': expected daemon, unix:///path, or ssh-ng://[user@]host",
            b.url
        );
        return 2;
    };
    let t0 = std::time::Instant::now();
    let locate = |paths: &[StorePath]| locate_in_store(paths, &b.sources);
    let plan = match crate::remote::plan::plan(roots, inputs, &locate) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    let plan_time = t0.elapsed();
    if b.timings && (plan.store_drvs > 0 || plan.store_sources > 0) {
        eprintln!(
            "plan: {} drvs and {} sources not from this eval, read from /nix/store",
            plan.store_drvs, plan.store_sources
        );
    }

    let mut built = Vec::new();
    let mut failed = false;
    let roots = if b.no_build { &roots[..1] } else { roots };
    for root in roots {
        let mut on_event = |ev: Event| match ev {
            Event::Started(s) => eprintln!("building {}", s.drv_path),
            Event::Log(l) => {
                let line = String::from_utf8_lossy(&l.line);
                match StorePath::parse(&l.drv_path) {
                    Ok(p) => eprintln!("{}> {line}", p.name()),
                    Err(_) => eprintln!("{line}"),
                }
            }
            Event::Built(b) => built.push((b.output, b.store_path)),
            Event::Failed(f) => {
                failed = true;
                eprintln!("error: build of {} failed: {}", f.drv_path, f.reason);
            }
            Event::Done(d) => failed |= !d.success,
        };
        let report = crate::remote::worker::build::build(
            &transport,
            &plan,
            root,
            outputs,
            b.no_build,
            b.fetch_to.as_deref(),
            &mut on_event,
        )
        .map(|t| t.report(&plan, plan_time));
        match report {
            Ok(r) => {
                if b.timings {
                    eprintln!("{r}");
                }
            }
            Err(e) => {
                eprintln!("error: {e}");
                return 1;
            }
        }
    }
    let wanted: Vec<&(String, String)> = built
        .iter()
        .filter(|(name, _)| outputs.is_empty() || outputs.contains(name))
        .collect();
    for (_, path) in &wanted {
        println!("{path}");
    }
    if failed { 1 } else { 0 }
}

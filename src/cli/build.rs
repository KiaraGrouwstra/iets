//! `ietsp build`: evaluate to derivations and build them through a daemon.

use std::collections::HashMap;
use std::sync::Arc;

use crate::cli::help::{arg_error, value};
use crate::cli::{EvalArgs, Input, build_targets, evaluate, parse_eval_args, report};
use crate::error::Error as CrateError;
use crate::eval::Evaluator;
use crate::store::locate::{drv_roots, locate_in_store};
use crate::trace::run::Cache;

struct BuildArgs {
    url: Option<String>,
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
        url: None,
        timings: false,
        no_build: false,
        fetch_to: None,
        outputs: Vec::new(),
        sources: HashMap::new(),
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
            "--builder" => b.url = Some(next()),
            "--timings" => b.timings = true,
            "--no-build" => b.no_build = true,
            "--fetch-to" => b.fetch_to = Some(next().into()),
            "--out" => b.outputs.push(next()),
            "--source" => {
                let spec = next();
                let Some((store, local)) = spec.split_once('=') else {
                    arg_error(format!("--source wants STOREPATH=LOCAL, got '{spec}'"))
                };
                b.sources.insert(store.to_string(), local.into());
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
    let a = EvalArgs {
        timings: a.timings || b.timings,
        ..a
    };
    let mut ev = a.evaluator();
    if let Err(e) = super::configure_lints(&mut ev, &a) {
        report(&ev, &e);
        return 1;
    }
    let cache = super::setup_cache(&a, "build", &ev);
    if let Some(c) = &cache
        && c.lookup()
        && let Some(hit) = c.read_build()
    {
        c.replay_warnings();
        let (roots, drvs, sources) = hit;
        if b.timings {
            eprintln!(
                "eval: skipped, {} derivations and {} sources from the cache, {:.3}s",
                drvs.len(),
                sources.len(),
                t0.elapsed().as_secs_f64()
            );
        }
        let (root_paths, outputs) = build_targets(&b.outputs, &roots);
        let prov = crate::provenance::Provenance::from_map(
            crate::provenance::SourceMap::from_sources(&sources),
        );
        let inputs = crate::remote::plan::Inputs {
            drvs: &drvs,
            sources: &sources,
        };
        return build_roots(&b, &root_paths, &outputs, inputs, &prov);
    }
    if cache.is_some() {
        ev.trace = crate::trace::Trace::enabled();
    }
    // parallel: eval and build run as the main fiber
    crate::parallel::run(a.workers, ev, move |ev| build_eval(ev, &b, a, t0, cache))
}

/// The Nix settings, for `system` and the `store` that `--builder` defaults to;
/// `ietsp build` takes no `--option`. An unreadable nix.conf leaves the
/// defaults, as it did when nothing here read one.
fn config() -> crate::config::Config {
    crate::config::Config::load(&[]).unwrap_or_else(|_| crate::config::Config::empty())
}

fn build_eval(
    ev: &mut Evaluator,
    b: &BuildArgs,
    a: EvalArgs,
    t0: std::time::Instant,
    cache: Option<Cache>,
) -> i32 {
    let mut roots = Vec::new();
    let target = a.attr.clone().or_else(|| match &a.input {
        Some(Input::Flake(f)) => f.split_once('#').map(|(_, frag)| frag.to_string()),
        _ => None,
    });
    let r = evaluate(ev, a).and_then(|v| drv_roots(ev, v, &mut roots));
    if let Err(e) = r {
        report(ev, &e);
        return 1;
    }
    if let Some(t) = &target {
        for (p, _) in &roots {
            ev.drv.set_attr(p, t);
        }
    }
    let eval_time = t0.elapsed();
    if b.timings {
        eprintln!(
            "eval: {} derivations, {} sources, {:.3}s",
            ev.drv.nr_derivations,
            ev.known.origins().len(),
            eval_time.as_secs_f64()
        );
    }
    if let Some(c) = &cache {
        c.write(ev, &crate::trace::wire::encode_roots(&roots), true);
    }
    let (root_paths, outputs) = build_targets(&b.outputs, &roots);
    let prov = crate::provenance::Provenance::snapshot(ev);
    let drvs = ev.drv.drvs.lock();
    let sources = ev.known.origins();
    let inputs = crate::remote::plan::Inputs {
        drvs: &drvs,
        sources: &sources,
    };
    build_roots(b, &root_paths, &outputs, inputs, &prov)
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
                aterm: Some(Arc::new(text.into_bytes())),
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
    let prov = crate::provenance::Provenance::from_map(crate::provenance::SourceMap::from_sources(
        &sources,
    ));
    build_roots(
        b,
        &[root],
        &b.outputs,
        crate::remote::plan::Inputs {
            drvs: &drvs,
            sources: &sources,
        },
        &prov,
    )
}

fn build_roots(
    b: &BuildArgs,
    roots: &[crate::store::path::StorePath],
    outputs: &[String],
    inputs: crate::remote::plan::Inputs<'_>,
    prov: &crate::provenance::Provenance,
) -> i32 {
    use crate::build::render::Style;
    use crate::build::upload::PlanUpload;
    use crate::build::{Dispatcher, Target};
    use crate::store::path::StorePath;

    let cfg = config();
    let url = crate::settings::builder(b.url.as_deref(), &cfg);
    let transport = match crate::remote::worker::Transport::parse_checked(&url, &cfg.system()) {
        Ok(Some(t)) => t,
        Ok(None) => {
            eprintln!(
                "error: unsupported builder '{url}': expected daemon, unix:///path, ssh-ng://[user@]host or grpc://host:port"
            );
            return 2;
        }
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    let t0 = std::time::Instant::now();
    let plan = match crate::remote::plan::plan(roots, inputs) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    let plan_time = t0.elapsed();
    if b.timings && plan.store_drvs > 0 {
        eprintln!(
            "plan: {} drvs not from this eval, read from /nix/store",
            plan.store_drvs
        );
    }
    let locate = |paths: &[StorePath]| locate_in_store(paths, &b.sources);

    let mut up = PlanUpload::new(&plan, &locate);
    let mut d = match Dispatcher::open(
        &transport,
        &crate::remote::worker::Options::default(),
        prov.clone(),
        Style::detect(),
    ) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };

    if b.no_build {
        if let Err(e) = d.upload_only(&mut up, &roots[..1]) {
            eprintln!("error: {e}");
            return 1;
        }
        timings(b, &d, &plan, plan_time);
        return 0;
    }

    let targets: Vec<Target> = roots
        .iter()
        .map(|r| {
            // The bare attribute: the dispatcher adds the position, once.
            let name = prov
                .attr_of(&r.to_string())
                .unwrap_or_else(|| crate::provenance::drv_name(&r.to_string()));
            Target::new(&name, r.clone(), outputs.to_vec())
        })
        .collect();
    let outcomes = match d.submit(&mut up, targets) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    if let Some(dir) = &b.fetch_to {
        for o in &outcomes {
            for path in o.outputs.values() {
                if let Err(e) = d.fetch(path, dir) {
                    eprintln!("error: {e}");
                    return 1;
                }
            }
        }
    }
    timings(b, &d, &plan, plan_time);
    for o in &outcomes {
        for path in o.outputs.values() {
            println!("{path}");
        }
    }
    if d.failed { 1 } else { 0 }
}

/// `--timings`: one line per phase of the flow, as the backend measured it.
fn timings(
    b: &BuildArgs,
    d: &crate::build::Dispatcher,
    plan: &crate::remote::plan::Plan,
    plan_time: std::time::Duration,
) {
    if b.timings {
        eprintln!("{}", d.report(plan, plan_time));
    }
}

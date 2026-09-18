//! `grpc://` as one of the dispatcher's backends (`build::backend`).
//!
//! Where the worker protocol sends one `BuildPathsWithResults` per ready set,
//! this fans the set out. A farm builds one derivation per `BuildDerivation`
//! call, so the client is the scheduler: the batch's whole derivation closure,
//! dependencies first, up to `min(max-builds, max-jobs)` in flight. Every
//! call knows its derivation, so its `log_line` and `phase` events carry the
//! drv path and the renderer keys its per-build buffers on them. A non-farm
//! store resolves the closure itself, so there one `BuildPaths` takes the
//! whole ready set.
//!
//! Outputs land in the farm's binary cache, not in a local store, so an
//! out-link needs them registered locally first (docs/remote-build.md).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use crate::build::backend::{Backend, Batch, Phase, Status, local};
use crate::build::fanout;
use crate::error::{Error, Result};
use crate::remote::event::{Event, Started};
use crate::remote::plan::{Object, Plan};
use crate::remote::worker::{Connection, Options, failed_event};
use crate::store::drv::Derivation;
use crate::store::path::StorePath;

use super::build::{self, Objects, Timings};
use super::drvwire::{BuildResult, basic_input_srcs};
use super::{Client, GrpcConfig};

pub struct Grpc {
    c: Client,
    t: Timings,
    /// `-j`: the other cap on how many builds are in flight.
    max_jobs: usize,
    /// Everything this run uploaded, so a `NOT_FOUND` retry can re-send one
    /// derivation's closure.
    objects: Objects,
    /// The derivations the submitted batches named, for their output paths.
    drvs: BTreeMap<StorePath, Derivation>,
}

impl Grpc {
    pub fn open(cfg: &GrpcConfig, options: &Options) -> Result<Grpc> {
        let c = Client::open(cfg)?;
        Ok(Grpc {
            t: Timings {
                farm: c.info.farm,
                trusted: c.info.trusted,
                calls: 1,
                ..Timings::default()
            },
            c,
            max_jobs: options.max_jobs.max(1) as usize,
            objects: Objects::default(),
            drvs: BTreeMap::new(),
        })
    }
}

/// Non-farm: one `BuildPaths` over the whole ready set. The store walks the
/// closure, so the log lines it streams name no derivation; the renderer folds
/// them into the running build when there is just one.
fn store_build(
    c: &mut Client,
    t: &mut Timings,
    batch: &Batch<'_>,
    on_event: &mut dyn FnMut(Event),
) -> Result<HashMap<String, Status>> {
    // The daemon parses these with `DerivedPath::parse`, whose separator is
    // `^`, not the worker protocol's legacy `!` (derived-path.cc).
    let targets: Vec<String> = batch
        .jobs
        .iter()
        .map(|j| format!("{}^{}", j.drv, j.spec()))
        .collect();
    for j in batch.jobs {
        on_event(Event::Started(Started {
            drv_path: j.drv.to_string(),
        }));
    }
    let results = build::build_paths(c, targets, t, on_event)?;
    t.drvs += batch.jobs.len();
    let mut out: HashMap<String, Status> = HashMap::new();
    for (path, res) in &results {
        let drv = path
            .split(['^', '!'])
            .next()
            .unwrap_or(path.as_str())
            .to_string();
        let e = out.entry(drv).or_insert_with(Status::ok);
        if !res.success() {
            *e = Status::failed(format!("status {}: {}", res.status, res.error_msg));
        } else if matches!(res.status, 2 | 13) {
            t.drvs_cached += 1;
        }
    }
    Ok(out)
}

/// Farm mode: `BuildDerivation` per derivation of the batch's closure, at most
/// `min(max-builds, -j)` in flight, a derivation only once every input
/// derivation of it is done.
///
/// Derivations whose outputs the store already reports valid are skipped. The
/// C++ plugin sends them anyway and lets the niks3 claim answer `AlreadyValid`,
/// which costs one RPC per derivation; one extra `QueryValidPaths` over the
/// output paths is cheaper and asks the same store.
fn farm_build(
    c: &mut Client,
    t: &mut Timings,
    objects: &Objects,
    drvs: &BTreeMap<StorePath, Derivation>,
    max_jobs: usize,
    batch: &Batch<'_>,
    on_event: &mut dyn FnMut(Event),
) -> Result<HashMap<String, Status>> {
    let mut out: HashMap<String, Status> = batch
        .jobs
        .iter()
        .map(|j| (j.drv.to_string(), Status::ok()))
        .collect();
    let mut out_names: Vec<String> = Vec::new();
    let mut out_of: Vec<&StorePath> = Vec::new();
    for (drv_path, drv) in drvs {
        for (name, o) in &drv.outputs {
            let Some(p) = o.path(&drv.name, name)? else {
                return Err(Error::Plan(format!(
                    "'{drv_path}': output '{name}' has no statically known path; the build \
                     farm needs statically known input and output paths"
                )));
            };
            out_names.push(p.base_name().to_string());
            out_of.push(drv_path);
        }
    }
    let valid = build::query_valid_paths(c, &out_names, t)?;
    let mut needs: BTreeSet<&StorePath> = BTreeSet::new();
    for (name, drv_path) in out_names.iter().zip(&out_of) {
        if !valid.contains(name) {
            needs.insert(drv_path);
        }
    }

    let mut jobs: Vec<build::Job> = Vec::new();
    let mut index: HashMap<&StorePath, usize> = HashMap::new();
    for (drv_path, drv) in drvs {
        if !needs.contains(drv_path) {
            continue;
        }
        let srcs = basic_input_srcs(drv_path, drv, &|dep, output| {
            build::output_path(drvs, dep, output)
        })?;
        index.insert(drv_path, jobs.len());
        jobs.push(build::Job::new(drv_path.clone(), drv.clone(), srcs));
    }
    t.drvs += jobs.len();
    t.drvs_cached += drvs.len() - jobs.len();
    if jobs.is_empty() {
        return Ok(out);
    }
    let graph: Vec<fanout::Job> = jobs
        .iter()
        .map(|j| fanout::Job {
            deps: j
                .drv
                .input_drvs
                .keys()
                .filter_map(|d| index.get(d).copied())
                .collect(),
        })
        .collect();

    let client: &Client = c;
    let jobs = &jobs;
    let calls = AtomicUsize::new(0);
    let build = |i: usize, tx: &Sender<Event>| -> BuildResult {
        let job = &jobs[i];
        let _ = tx.send(Event::Started(Started {
            drv_path: job.drv_path.to_string(),
        }));
        match build::build_derivation(client, job, objects, tx) {
            Ok((res, n)) => {
                calls.fetch_add(n, Ordering::Relaxed);
                res
            }
            // 9 = MiscFailure. nix reports only the top level result, so name
            // the leaf that broke.
            Err(e) => BuildResult {
                status: 9,
                error_msg: e.to_string(),
                ..BuildResult::default()
            },
        }
    };
    let work = fanout::Work {
        build: &build,
        ok: &BuildResult::success,
        // 10 = DependencyFailed.
        blocked: &|_, dep| BuildResult {
            status: 10,
            error_msg: format!("dependency '{}' failed", jobs[dep].drv_path),
            ..BuildResult::default()
        },
    };
    let limit = client.cfg.max_builds.min(max_jobs);
    let results = fanout::run(&graph, limit, &work, on_event);
    t.calls += calls.load(Ordering::Relaxed);

    for (i, res) in results.iter().enumerate() {
        let drv = jobs[i].drv_path.to_string();
        let reason = match res {
            Some(res) if res.success() => continue,
            Some(res) => format!("status {}: {}", res.status, res.error_msg),
            None => "the farm never reached this derivation".to_string(),
        };
        match out.get_mut(&drv) {
            Some(slot) => *slot = Status::failed(reason),
            // A dependency, not something the caller asked for: the renderer
            // still wants its verdict.
            None => on_event(failed_event(&drv, &reason)),
        }
    }
    Ok(out)
}

impl Backend for Grpc {
    fn query_valid(
        &mut self,
        paths: &[&StorePath],
        on_event: &mut dyn FnMut(Event),
    ) -> Result<HashSet<String>> {
        let _ = on_event;
        if paths.is_empty() {
            return Ok(HashSet::new());
        }
        let names: Vec<String> = paths.iter().map(|p| p.base_name().to_string()).collect();
        self.t.query_paths += names.len();
        let t0 = Instant::now();
        let valid = build::query_valid_paths(&mut self.c, &names, &mut self.t)?;
        self.t.query += t0.elapsed();
        Ok(valid)
    }

    fn substitute(&mut self, paths: &[StorePath], on_event: &mut dyn FnMut(Event)) -> Result<()> {
        if paths.is_empty() {
            return Ok(());
        }
        self.t.substitute_paths += paths.len();
        let t0 = Instant::now();
        let r = build::substitute(&mut self.c, paths, &mut self.t, on_event);
        self.t.substitute += t0.elapsed();
        r
    }

    fn add_objects(&mut self, objects: &[&Object], on_event: &mut dyn FnMut(Event)) -> Result<u64> {
        let _ = on_event;
        if objects.is_empty() {
            return Ok(0);
        }
        let t0 = Instant::now();
        let bytes = build::add_multiple(&mut self.c, objects, &mut self.t)?;
        self.t.add += t0.elapsed();
        self.t.add_paths += objects.len();
        self.t.add_bytes += bytes;
        self.objects.extend(objects);
        Ok(bytes)
    }

    fn submit(
        &mut self,
        batch: &Batch<'_>,
        on_event: &mut dyn FnMut(Event),
    ) -> Result<HashMap<String, Status>> {
        // Both modes report the outputs from the derivations themselves; a farm
        // also schedules them, so it needs the whole closure.
        let farm = self.c.info.farm;
        let drvs = (batch.drvs)(farm)?;
        self.drvs.extend(build::drv_table(&drvs)?);

        let t0 = Instant::now();
        let r = if farm {
            farm_build(
                &mut self.c,
                &mut self.t,
                &self.objects,
                &self.drvs,
                self.max_jobs,
                batch,
                on_event,
            )
        } else {
            store_build(&mut self.c, &mut self.t, batch, on_event)
        };
        self.t.build += t0.elapsed();
        r
    }

    fn outputs(
        &mut self,
        drv: &str,
        on_event: &mut dyn FnMut(Event),
    ) -> Result<BTreeMap<String, Option<String>>> {
        let _ = on_event;
        let t0 = Instant::now();
        let path = StorePath::parse(drv)?;
        let d = self
            .drvs
            .get(&path)
            .ok_or_else(|| Error::Plan(format!("'{drv}' is not in the batch's closure")))?;
        let mut out = BTreeMap::new();
        for (name, o) in &d.outputs {
            let Some(p) = o.path(&d.name, name)? else {
                return Err(Error::Plan(format!(
                    "'{drv}': output '{name}' has no statically known path"
                )));
            };
            out.insert(name.clone(), Some(p.to_string()));
        }
        self.t.outputs += t0.elapsed();
        Ok(out)
    }

    fn fetch(&mut self, store_path: &str, dir: &Path) -> Result<()> {
        let t0 = Instant::now();
        let paths = [StorePath::parse(store_path)?];
        build::fetch_into_dir(&mut self.c, &paths, dir, &mut self.t)?;
        self.t.fetch += t0.elapsed();
        self.t.fetched += 1;
        Ok(())
    }

    fn add_root(&mut self, store_path: &str, link: &Path) -> Result<()> {
        self.out_links(&[(store_path.to_string(), link.display().to_string())])
    }

    /// The store published the outputs to its cache, so they are copied into
    /// the local store before the symlink that names them.
    fn out_links(&mut self, links: &[(String, String)]) -> Result<()> {
        if links.is_empty() {
            return Ok(());
        }
        let mut paths: Vec<StorePath> = Vec::with_capacity(links.len());
        for (p, _) in links {
            paths.push(StorePath::parse(p)?);
        }
        let mut conn = Connection::open(&local(), &mut |_| {})?;
        build::register(&mut self.c, &paths, &mut conn, &mut |_| {})?;
        for (path, link) in links {
            conn.add_perm_root(path, Path::new(link))?;
        }
        Ok(())
    }

    fn note_connect(&mut self, d: Duration) {
        self.t.connect = d;
    }

    fn note_upload(&mut self, p: Phase) {
        self.t.missing += p.missing;
        self.t.resolve += p.resolve;
        self.t.resolve_paths += p.resolve_paths;
        self.t.store_sources += p.store_sources;
    }

    fn report(&self, plan: &Plan, plan_time: Duration) -> String {
        self.t.report(plan, plan_time)
    }
}

//! The `grpc://` client flow, mirroring `worker/build.rs`:
//! `StoreInfo` once, one `QueryValidPaths` over the plan, one
//! `AddMultipleToStore`, then builds — `BuildDerivation` per derivation in
//! farm mode, `BuildPaths` otherwise — and `FetchNars` for the outputs.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::io::{self, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use tokio_stream::StreamExt;
use tokio_stream::wrappers::ReceiverStream;

use crate::error::{Error, Result};
use crate::store::drv::Derivation;
use crate::store::nar::restore_path;
use crate::store::path::StorePath;

use super::super::event::{Done, Event, Log, Started};
use super::super::plan::{Object, Plan};
use super::super::worker::build::write_object;
use super::super::worker::wire::write_u64;
use super::super::worker::{built_event, failed_event};
use super::drvwire::{
    BUILD_PROTOCOL, BuildResult, PathInfo, basic_input_srcs, read_keyed_build_results,
    write_basic_derivation,
};
use super::{Client, GrpcConfig, Headers, c_request, first_line, gone_away, proto};

/// What one `build` call did, in the shape `--timings` prints.
#[derive(Debug, Default, Clone)]
pub struct Timings {
    pub connect: Duration,
    pub query: Duration,
    pub add: Duration,
    pub build: Duration,
    pub outputs: Duration,
    pub fetch: Duration,
    pub farm: bool,
    pub trusted: Option<bool>,
    pub query_paths: usize,
    pub missing: usize,
    pub add_paths: usize,
    pub add_bytes: u64,
    pub drvs: usize,
    pub drvs_cached: usize,
    pub fetched: usize,
    pub calls: usize,
}

impl Timings {
    pub fn report(&self, plan: &Plan, plan_time: Duration) -> String {
        let trusted = match self.trusted {
            Some(true) => "trusted",
            Some(false) => "untrusted",
            None => "trust unknown",
        };
        let mode = if self.farm { "farm" } else { "store" };
        let mut s = format!(
            "plan: {} sources, {} drvs, {:.3}s\nconnect: gRPC {mode}, {trusted}, {:.3}s\nquery: {} paths, {} missing, {:.3}s\nadd: {} paths, {} bytes, {:.3}s\nbuild: {} drvs, {} already valid, {:.3}s\noutputs: {:.3}s",
            plan.sources(),
            plan.drvs(),
            plan_time.as_secs_f64(),
            self.connect.as_secs_f64(),
            self.query_paths,
            self.missing,
            self.query.as_secs_f64(),
            self.add_paths,
            self.add_bytes,
            self.add.as_secs_f64(),
            self.drvs,
            self.drvs_cached,
            self.build.as_secs_f64(),
            self.outputs.as_secs_f64(),
        );
        if self.fetched > 0 {
            s.push_str(&format!(
                "\nfetch: {} outputs, {:.3}s",
                self.fetched,
                self.fetch.as_secs_f64()
            ));
        }
        s.push_str(&format!("\ngrpc: {} calls", self.calls));
        s
    }
}

/// Uploads what the store lacks, builds `root`, reports its outputs and
/// restores the wanted ones into `fetch_to`. With `upload_only` the call stops
/// after the upload.
pub fn build(
    cfg: &GrpcConfig,
    plan: &Plan,
    root: &StorePath,
    outputs: &[String],
    upload_only: bool,
    fetch_to: Option<&Path>,
    on_event: &mut dyn FnMut(Event),
) -> Result<Timings> {
    let mut c = Client::open(cfg)?;
    let mut t = Timings::default();
    let built = build_on(&mut c, plan, root, outputs, upload_only, &mut t, on_event)?;

    if let (Some(dir), Some(built)) = (fetch_to, &built) {
        let t5 = Instant::now();
        std::fs::create_dir_all(dir)
            .map_err(|e| Error::io(format!("cannot create {}", dir.display()), e))?;
        let wanted: Vec<StorePath> = built.iter().map(|(_, p)| p.clone()).collect();
        for p in &wanted {
            let dest = dir.join(p.base_name());
            if dest.exists() {
                return Err(Error::Usage(format!(
                    "fetch: '{}' already exists",
                    dest.display()
                )));
            }
        }
        fetch_into_dir(&mut c, &wanted, dir, &mut t)?;
        t.fetched = wanted.len();
        t.fetch = t5.elapsed();
    }
    on_event(Event::Done(Done {
        success: built.is_some(),
    }));
    Ok(t)
}

/// `build` without the fetch step. `None` means a build failed; otherwise the
/// root's wanted outputs and their store paths.
pub fn build_on(
    c: &mut Client,
    plan: &Plan,
    root: &StorePath,
    outputs: &[String],
    upload_only: bool,
    t: &mut Timings,
    on_event: &mut dyn FnMut(Event),
) -> Result<Option<Vec<(String, StorePath)>>> {
    t.farm = c.info.farm;
    t.trusted = c.info.trusted;
    t.calls += 1;
    let drvs = drv_table(plan)?;

    let t1 = Instant::now();
    let names: Vec<String> = plan
        .objects
        .iter()
        .map(|o| o.store_path().base_name().to_string())
        .collect();
    t.query_paths = names.len();
    let valid = query_valid_paths(c, &names, t)?;
    let missing: Vec<&Object> = plan
        .objects
        .iter()
        .filter(|o| !valid.contains(o.store_path().base_name()))
        .collect();
    t.missing = missing.len();
    t.query = t1.elapsed();

    let t2 = Instant::now();
    if !missing.is_empty() {
        t.add_bytes = add_multiple(c, &missing, t)?;
        t.add_paths = missing.len();
    }
    t.add = t2.elapsed();
    if upload_only {
        return Ok(Some(Vec::new()));
    }

    let t3 = Instant::now();
    let ok = if c.info.farm {
        farm_build(c, plan, &drvs, t, on_event)?
    } else {
        store_build(c, root, outputs, t, on_event)?
    };
    t.build = t3.elapsed();
    if !ok {
        return Ok(None);
    }

    let t4 = Instant::now();
    let root_drv = drvs
        .get(root)
        .ok_or_else(|| Error::Plan(format!("'{root}' is not in the plan")))?;
    let mut built: Vec<(String, StorePath)> = Vec::new();
    for (name, out) in &root_drv.outputs {
        if !outputs.is_empty() && !outputs.contains(name) {
            continue;
        }
        let Some(path) = out.path(&root_drv.name, name)? else {
            return Err(Error::Plan(format!(
                "'{root}': output '{name}' has no statically known path"
            )));
        };
        on_event(built_event(&root.to_string(), name, &path.to_string()));
        built.push((name.clone(), path));
    }
    t.outputs = t4.elapsed();
    Ok(Some(built))
}

/// Every derivation of the plan, parsed from its ATerm. The plan holds the
/// whole closure of the roots, so the map has an entry for every input
/// derivation.
pub fn drv_table(plan: &Plan) -> Result<BTreeMap<StorePath, Derivation>> {
    let mut out = BTreeMap::new();
    for o in &plan.objects {
        if let Object::Drv { path, aterm, .. } = o {
            let text = std::str::from_utf8(aterm)
                .map_err(|_| Error::Plan(format!("derivation '{path}' is not UTF-8")))?;
            let drv = Derivation::parse(text, Derivation::name_from_path(path)?)?;
            out.insert(path.clone(), drv);
        }
    }
    Ok(out)
}

/// The store path of `drv_path!output`, statically.
fn output_path(
    drvs: &BTreeMap<StorePath, Derivation>,
    drv_path: &StorePath,
    output: &str,
) -> Option<StorePath> {
    let drv = drvs.get(drv_path)?;
    drv.outputs
        .get(output)?
        .path(&drv.name, output)
        .ok()
        .flatten()
}

// ---- the batched RPCs --------------------------------------------------------

/// `QueryValidPaths` on store path base names, in one call.
fn query_valid_paths(c: &mut Client, names: &[String], t: &mut Timings) -> Result<HashSet<String>> {
    if names.is_empty() {
        return Ok(HashSet::new());
    }
    t.calls += 1;
    let req = proto::QueryValidPathsRequest {
        paths: names.to_vec(),
        substitute: false,
    };
    let reply = c.call("QueryValidPaths", |mut stub, h| {
        let req = req.clone();
        async move { stub.query_valid_paths(c_request(req, &h)).await }
    })?;
    Ok(reply.paths.into_iter().collect())
}

/// `QueryPathInfos`: the 1.16 `UnkeyedValidPathInfo` of each path the store
/// has, signatures included. In farm mode the worker substitutes a path it
/// lacks from the cache first, so this also answers for cached outputs.
fn query_path_infos(
    c: &mut Client,
    paths: &[StorePath],
    t: &mut Timings,
) -> Result<HashMap<StorePath, PathInfo>> {
    if paths.is_empty() {
        return Ok(HashMap::new());
    }
    t.calls += 1;
    let req = proto::QueryPathInfosRequest {
        paths: paths.iter().map(|p| p.base_name().to_string()).collect(),
    };
    let reply = c.call("QueryPathInfos", |mut stub, h| {
        let req = req.clone();
        async move { stub.query_path_infos(c_request(req, &h)).await }
    })?;
    let mut out = HashMap::new();
    for entry in reply.infos {
        let info = PathInfo::decode(&entry.path, &entry.info)?;
        out.insert(info.path.clone(), info);
    }
    Ok(out)
}

/// Ships `AddMultipleChunk`s to the channel, one message per `CHUNK` of
/// compressed bytes (pump.hh:`ZstdWriterSink`).
struct ChunkShip {
    tx: tokio::sync::mpsc::Sender<proto::AddMultipleChunk>,
    buf: Vec<u8>,
    sent: Arc<AtomicU64>,
}

/// pump.hh:`kChunkSize`.
const CHUNK: usize = 256 << 10;

impl ChunkShip {
    fn ship(&mut self) -> io::Result<()> {
        if self.buf.is_empty() {
            return Ok(());
        }
        let data = std::mem::take(&mut self.buf);
        self.sent.fetch_add(data.len() as u64, Ordering::Relaxed);
        self.tx
            .blocking_send(proto::AddMultipleChunk {
                repair: false,
                check_sigs: false,
                data,
            })
            .map_err(|_| io::Error::other("gRPC stream closed by peer during bulk import"))
    }
}

impl Write for ChunkShip {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.buf.extend_from_slice(data);
        if self.buf.len() >= CHUNK {
            self.ship()?;
        }
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.ship()
    }
}

/// `AddMultipleToStore`: one zstd stream whose decompressed bytes are the
/// worker-protocol `AddMultipleToStore` framing (a count, then each
/// `ValidPathInfo` at 1.16 followed by its NAR) — the same `write_object` the
/// worker transport writes. Like the C++ sink, the stream is flushed once, at
/// the end; the reader decodes a partial frame fine.
///
/// `check_sigs` is false because every path we send is content-addressed, so
/// the daemon recomputes its address. A caller holding only a `write` token
/// gets `CheckSigs` forced on server-side anyway (daemon/main.cc), which those
/// paths still pass.
fn add_multiple(c: &mut Client, objects: &[&Object], t: &mut Timings) -> Result<u64> {
    let deadline = Instant::now() + Duration::from_secs(c.cfg.connect_timeout);
    let mut pause = Duration::from_millis(500);
    loop {
        t.calls += 1;
        let headers = c.headers()?;
        match upload(c, objects, &headers) {
            Ok(n) => return Ok(n),
            Err(s) => {
                if !gone_away(&s) || Instant::now() + pause > deadline {
                    return Err(c.status_error(&s, "AddMultipleToStore"));
                }
                eprintln!("{}: {}, retrying", c.cfg.authority, first_line(s.message()));
                std::thread::sleep(pause);
                pause = (pause * 2).min(Duration::from_secs(4));
            }
        }
    }
}

/// One `AddMultipleToStore` attempt. The NAR dump is synchronous and borrows
/// the plan, so it runs on a scoped thread while this one drives the stream.
fn upload(
    c: &Client,
    objects: &[&Object],
    headers: &Headers,
) -> std::result::Result<u64, tonic::Status> {
    let (tx, rx) = tokio::sync::mpsc::channel::<proto::AddMultipleChunk>(4);
    let mut stub = c.stub.clone();
    let rt = &c.rt;
    let n = objects.len();
    let sent = Arc::new(AtomicU64::new(0));
    std::thread::scope(|s| {
        let producer = {
            let tx = tx.clone();
            let sent = Arc::clone(&sent);
            s.spawn(move || -> io::Result<()> {
                // store.cc sends the flags alone first; the zstd stream
                // follows in its own messages.
                tx.blocking_send(proto::AddMultipleChunk {
                    repair: false,
                    check_sigs: false,
                    data: Vec::new(),
                })
                .map_err(|_| io::Error::other("gRPC stream closed by peer"))?;
                let ship = ChunkShip {
                    tx,
                    buf: Vec::with_capacity(CHUNK + (1 << 16)),
                    sent,
                };
                let mut enc = zstd::stream::write::Encoder::new(ship, 1)?;
                write_u64(&mut enc, n as u64)?;
                for o in objects {
                    write_object(&mut enc, o)?;
                }
                enc.finish()?.flush()
            })
        };
        drop(tx);
        let reply = rt.block_on(async {
            stub.add_multiple_to_store(c_request(ReceiverStream::new(rx), headers))
                .await
        });
        let written = producer.join().map_err(|_| {
            tonic::Status::internal("the AddMultipleToStore producer thread panicked")
        })?;
        match reply {
            // The server status explains a broken stream better than the local
            // write error does (transfer.cc:addMultipleToStoreOnce).
            Ok(_) => written
                .map(|()| sent.load(Ordering::Relaxed))
                .map_err(|e| tonic::Status::internal(e.to_string())),
            Err(status) => Err(status),
        }
    })
}

// ---- builds -----------------------------------------------------------------

/// Non-farm: one `BuildPaths` on `<root>!<outputs>`, whose reply carries the
/// `vector<KeyedBuildResult>` bytes.
fn store_build(
    c: &mut Client,
    root: &StorePath,
    outputs: &[String],
    t: &mut Timings,
    on_event: &mut dyn FnMut(Event),
) -> Result<bool> {
    let spec = if outputs.is_empty() {
        "*".to_string()
    } else {
        let names: BTreeSet<&str> = outputs.iter().map(String::as_str).collect();
        names.into_iter().collect::<Vec<_>>().join(",")
    };
    let req = proto::BuildPathsRequest {
        targets: vec![format!("{root}!{spec}")],
        build_mode: 0,
        protocol: BUILD_PROTOCOL,
    };
    t.calls += 1;
    let headers = c.headers()?;
    let mut stub = c.stub.clone();
    let mut stream =
        c.rt.block_on(async { stub.build_paths(c_request(req, &headers)).await })
            .map_err(|s| c.status_error(&s, "BuildPaths"))?
            .into_inner();

    let drv = root.to_string();
    on_event(Event::Started(Started {
        drv_path: drv.clone(),
    }));
    let mut results: Option<Vec<(String, BuildResult)>> = None;
    while let Some(msg) =
        c.rt.block_on(stream.next())
            .transpose()
            .map_err(|s| c.status_error(&s, "BuildPaths"))?
    {
        match msg.event {
            Some(proto::build_paths_chunk::Event::LogLine(line)) => {
                on_event(Event::Log(Log {
                    drv_path: drv.clone(),
                    line,
                }));
            }
            Some(proto::build_paths_chunk::Event::Phase(phase)) => {
                on_event(Event::Log(Log {
                    drv_path: drv.clone(),
                    line: format!("phase: {phase}").into_bytes(),
                }));
            }
            Some(proto::build_paths_chunk::Event::Done(done)) => {
                results = Some(read_keyed_build_results(&mut done.results.as_slice())?);
            }
            None => {}
        }
    }
    let results = results
        .ok_or_else(|| Error::Protocol("gRPC BuildPaths stream ended without a result".into()))?;
    t.drvs = 1;
    let mut ok = true;
    for (path, res) in &results {
        if !res.success() {
            ok = false;
            on_event(failed_event(
                path,
                &format!("status {}: {}", res.status, res.error_msg),
            ));
        } else if matches!(res.status, 2 | 13) {
            t.drvs_cached = 1;
        }
    }
    Ok(ok)
}

/// One derivation the farm must build.
struct Job {
    drv_path: StorePath,
    drv: Derivation,
    input_srcs: BTreeSet<StorePath>,
    features: Vec<String>,
    dependants: Vec<usize>,
    deps: usize,
}

/// store.hh:`FarmRun`: the ready queue with one result slot per job.
struct Run {
    ready: VecDeque<usize>,
    remaining: usize,
    waiting: Vec<usize>,
    results: Vec<Option<BuildResult>>,
    failed_input: Vec<Option<StorePath>>,
    calls: usize,
}

/// Farm mode: `BuildDerivation` per derivation, at most `max-builds` in
/// flight, and a derivation only once every input derivation of it is done.
///
/// Derivations whose outputs the store already reports valid are skipped. The
/// C++ plugin sends them anyway and lets the niks3 claim answer `AlreadyValid`,
/// which costs one RPC per derivation; one extra `QueryValidPaths` over the
/// output paths is cheaper and asks the same store.
fn farm_build(
    c: &mut Client,
    plan: &Plan,
    drvs: &BTreeMap<StorePath, Derivation>,
    t: &mut Timings,
    on_event: &mut dyn FnMut(Event),
) -> Result<bool> {
    let mut out_names: Vec<String> = Vec::new();
    let mut out_of: Vec<StorePath> = Vec::new();
    for (drv_path, drv) in drvs {
        for (name, out) in &drv.outputs {
            let Some(p) = out.path(&drv.name, name)? else {
                return Err(Error::Plan(format!(
                    "'{drv_path}': output '{name}' has no statically known path; the build \
                     farm needs statically known input and output paths"
                )));
            };
            out_names.push(p.base_name().to_string());
            out_of.push(drv_path.clone());
        }
    }
    let valid_outputs = query_valid_paths(c, &out_names, t)?;
    let mut needs_build: HashSet<&StorePath> = HashSet::new();
    for (name, drv_path) in out_names.iter().zip(&out_of) {
        if !valid_outputs.contains(name) {
            needs_build.insert(drvs.get_key_value(drv_path).expect("in the table").0);
        }
    }

    let mut jobs: Vec<Job> = Vec::new();
    let mut index: HashMap<&StorePath, usize> = HashMap::new();
    for (drv_path, drv) in drvs {
        if !needs_build.contains(drv_path) {
            continue;
        }
        let srcs = basic_input_srcs(drv_path, drv, &|dep, output| output_path(drvs, dep, output))?;
        index.insert(drv_path, jobs.len());
        jobs.push(Job {
            drv_path: drv_path.clone(),
            drv: drv.clone(),
            input_srcs: srcs,
            features: required_system_features(drv),
            dependants: Vec::new(),
            deps: 0,
        });
    }
    t.drvs = jobs.len();
    t.drvs_cached = drvs.len() - jobs.len();
    if jobs.is_empty() {
        return Ok(true);
    }
    for i in 0..jobs.len() {
        let deps: Vec<usize> = jobs[i]
            .drv
            .input_drvs
            .keys()
            .filter_map(|d| index.get(d).copied())
            .collect();
        jobs[i].deps = deps.len();
        for d in deps {
            jobs[d].dependants.push(i);
        }
    }

    let run = Mutex::new(Run {
        ready: (0..jobs.len()).filter(|&i| jobs[i].deps == 0).collect(),
        remaining: jobs.len(),
        waiting: jobs.iter().map(|j| j.deps).collect(),
        results: vec![None; jobs.len()],
        failed_input: vec![None; jobs.len()],
        calls: 0,
    });
    let wake = Condvar::new();
    let threads = c.cfg.max_builds.min(jobs.len());
    let plan_index = PlanIndex::new(plan);
    let client: &Client = c;

    std::thread::scope(|s| {
        let (tx, rx) = std::sync::mpsc::channel::<Event>();
        for _ in 0..threads {
            let tx = tx.clone();
            let run = &run;
            let wake = &wake;
            let jobs = &jobs;
            let plan_index = &plan_index;
            s.spawn(move || {
                while let Some(i) = next_job(run, wake) {
                    let job = &jobs[i];
                    let failed_input = run.lock().unwrap().failed_input[i].clone();
                    let res = match failed_input {
                        // 10 = DependencyFailed.
                        Some(dep) => BuildResult {
                            status: 10,
                            error_msg: format!("dependency '{dep}' failed"),
                            ..BuildResult::default()
                        },
                        None => {
                            let _ = tx.send(Event::Started(Started {
                                drv_path: job.drv_path.to_string(),
                            }));
                            match build_derivation(client, job, plan_index, &tx) {
                                Ok((res, calls)) => {
                                    run.lock().unwrap().calls += calls;
                                    res
                                }
                                // 9 = MiscFailure. nix reports only the top
                                // level result, so name the leaf that broke.
                                Err(e) => BuildResult {
                                    status: 9,
                                    error_msg: e.to_string(),
                                    ..BuildResult::default()
                                },
                            }
                        }
                    };
                    finish_job(run, wake, jobs, i, res);
                }
            });
        }
        drop(tx);
        for ev in rx {
            on_event(ev);
        }
    });

    let g = run.into_inner().unwrap();
    t.calls += g.calls;
    let mut ok = true;
    for (i, res) in g.results.iter().enumerate() {
        match res {
            Some(res) if res.success() => {}
            Some(res) => {
                ok = false;
                on_event(failed_event(
                    &jobs[i].drv_path.to_string(),
                    &format!("status {}: {}", res.status, res.error_msg),
                ));
            }
            None => ok = false,
        }
    }
    Ok(ok)
}

fn next_job(run: &Mutex<Run>, wake: &Condvar) -> Option<usize> {
    let mut g = run.lock().unwrap();
    loop {
        if let Some(i) = g.ready.pop_front() {
            return Some(i);
        }
        if g.remaining == 0 {
            return None;
        }
        g = wake.wait(g).unwrap();
    }
}

fn finish_job(run: &Mutex<Run>, wake: &Condvar, jobs: &[Job], i: usize, res: BuildResult) {
    let mut g = run.lock().unwrap();
    let succeeded = res.success();
    g.results[i] = Some(res);
    g.remaining -= 1;
    for &d in &jobs[i].dependants {
        if !succeeded {
            g.failed_input[d] = Some(jobs[i].drv_path.clone());
        }
        g.waiting[d] -= 1;
        if g.waiting[d] == 0 {
            g.ready.push_back(d);
        }
    }
    wake.notify_all();
}

/// nix-compat.hh:`requiredSystemFeatures`, which the balancer routes on: the
/// `requiredSystemFeatures` environment variable, space separated, or the same
/// key of `__json` when the derivation has structured attributes.
fn required_system_features(drv: &Derivation) -> Vec<String> {
    if let Some(json) = &drv.structured_attrs {
        let mut out: BTreeSet<String> = BTreeSet::new();
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(json)
            && let Some(list) = v.get("requiredSystemFeatures").and_then(|l| l.as_array())
        {
            for f in list {
                if let Some(s) = f.as_str() {
                    out.insert(s.to_string());
                }
            }
        }
        return out.into_iter().collect();
    }
    drv.env_get("requiredSystemFeatures")
        .unwrap_or("")
        .split_whitespace()
        .map(str::to_string)
        .collect::<BTreeSet<String>>()
        .into_iter()
        .collect()
}

/// `BuildDerivation` for one derivation, with the retries store.cc does:
/// `NOT_FOUND` means the worker lacks the `.drv` closure, so upload it and ask
/// once more; `UNAVAILABLE` means another worker should take it, so salt the
/// `x-nix-drv` header the balancer hashes and try again, up to
/// `unavailable-retries` times. Returns the result and the RPCs it cost.
fn build_derivation(
    c: &Client,
    job: &Job,
    plan: &PlanIndex<'_>,
    tx: &std::sync::mpsc::Sender<Event>,
) -> Result<(BuildResult, usize)> {
    let mut drv_bytes = Vec::new();
    write_basic_derivation(&mut drv_bytes, &job.drv, &job.input_srcs)
        .map_err(|e| Error::io("serialising a derivation", e))?;
    let req = proto::BuildDerivationRequest {
        // `StorePath::to_string()` on the C++ side: the base name.
        drv_path: job.drv_path.base_name().to_string(),
        drv: drv_bytes,
        build_mode: 0,
        protocol: BUILD_PROTOCOL,
    };
    let mut headers = c.headers()?;
    // store.cc:routingFor. `x-nix-drv` is the hash part only.
    headers.retain(|(k, _)| *k != "x-nix-system");
    headers.push(("x-nix-drv", job.drv_path.hash_part().to_string()));
    headers.push(("x-nix-system", job.drv.platform.clone()));
    headers.push(("x-nix-features", job.features.join(",")));

    let mut attempt = 0u32;
    let mut calls = 0usize;
    loop {
        calls += 1;
        match try_build_derivation(c, &req, &headers, &job.drv_path, tx) {
            Ok(r) => return Ok((r, calls)),
            Err(s)
                if matches!(
                    s.code(),
                    tonic::Code::NotFound | tonic::Code::FailedPrecondition
                ) =>
            {
                calls += upload_closure(c, plan, &job.drv_path, &headers)?;
                calls += 1;
                return try_build_derivation(c, &req, &headers, &job.drv_path, tx)
                    .map(|r| (r, calls))
                    .map_err(|s| c.status_error(&s, "BuildDerivation"));
            }
            Err(s)
                if s.code() == tonic::Code::Unavailable && attempt < c.cfg.unavailable_retries =>
            {
                eprintln!("{}, retrying elsewhere", first_line(s.message()));
                std::thread::sleep(Duration::from_secs(u64::from(attempt) + 1));
                attempt += 1;
                let salted = format!("{}-{attempt}", job.drv_path.hash_part());
                for h in &mut headers {
                    if h.0 == "x-nix-drv" {
                        h.1 = salted.clone();
                    }
                }
            }
            Err(s) => return Err(c.status_error(&s, "BuildDerivation")),
        }
    }
}

fn try_build_derivation(
    c: &Client,
    req: &proto::BuildDerivationRequest,
    headers: &Headers,
    drv_path: &StorePath,
    tx: &std::sync::mpsc::Sender<Event>,
) -> std::result::Result<BuildResult, tonic::Status> {
    let mut stub = c.stub.clone();
    let req = c_request(req.clone(), headers);
    let mut stream =
        c.rt.block_on(async { stub.build_derivation(req).await })?
            .into_inner();
    let drv = drv_path.to_string();
    let mut res: Option<BuildResult> = None;
    while let Some(msg) = c.rt.block_on(stream.next()).transpose()? {
        match msg.event {
            Some(proto::build_derivation_chunk::Event::LogLine(line)) => {
                let _ = tx.send(Event::Log(Log {
                    drv_path: drv.clone(),
                    line,
                }));
            }
            Some(proto::build_derivation_chunk::Event::Phase(phase)) => {
                let _ = tx.send(Event::Log(Log {
                    drv_path: drv.clone(),
                    line: format!("phase: {phase}").into_bytes(),
                }));
            }
            Some(proto::build_derivation_chunk::Event::Done(done)) => {
                res = Some(
                    BuildResult::read(&mut done.result.as_slice())
                        .map_err(|e| tonic::Status::internal(e.to_string()))?,
                );
            }
            None => {}
        }
    }
    res.ok_or_else(|| tonic::Status::internal("gRPC BuildDerivation stream ended without a result"))
}

/// The plan by store path base name, so a `NOT_FOUND` retry can re-send just
/// the closure of one derivation (store.cc:`uploadDrvClosure`).
struct PlanIndex<'a> {
    by_name: HashMap<&'a str, &'a Object>,
}

impl<'a> PlanIndex<'a> {
    fn new(plan: &'a Plan) -> PlanIndex<'a> {
        PlanIndex {
            by_name: plan
                .objects
                .iter()
                .map(|o| (o.store_path().base_name(), o))
                .collect(),
        }
    }

    /// `drv_path` and everything it references, references before referrers.
    fn closure(&self, drv_path: &StorePath) -> Vec<&'a Object> {
        let mut out = Vec::new();
        let mut seen: HashSet<&'a str> = HashSet::new();
        let mut stack = vec![(drv_path.base_name().to_string(), false)];
        while let Some((name, expanded)) = stack.pop() {
            let Some(o) = self.by_name.get(name.as_str()).copied() else {
                continue;
            };
            if expanded {
                out.push(o);
                continue;
            }
            if !seen.insert(o.store_path().base_name()) {
                continue;
            }
            stack.push((name, true));
            if let Object::Drv { references, .. } = o {
                for r in references {
                    stack.push((r.base_name().to_string(), false));
                }
            }
        }
        out
    }
}

/// store.cc:`uploadDrvClosure`: `QueryValidPaths` on the closure with the same
/// routing headers, then `AddMultipleToStore` of what the worker lacks.
fn upload_closure(
    c: &Client,
    plan: &PlanIndex<'_>,
    drv_path: &StorePath,
    headers: &Headers,
) -> Result<usize> {
    let closure = plan.closure(drv_path);
    if closure.is_empty() {
        return Ok(0);
    }
    let mut stub = c.stub.clone();
    let req = proto::QueryValidPathsRequest {
        paths: closure
            .iter()
            .map(|o| o.store_path().base_name().to_string())
            .collect(),
        substitute: false,
    };
    let valid: HashSet<String> =
        c.rt.block_on(async { stub.query_valid_paths(c_request(req, headers)).await })
            .map_err(|s| c.status_error(&s, "QueryValidPaths"))?
            .into_inner()
            .paths
            .into_iter()
            .collect();
    let missing: Vec<&Object> = closure
        .into_iter()
        .filter(|o| !valid.contains(o.store_path().base_name()))
        .collect();
    if missing.is_empty() {
        return Ok(1);
    }
    upload(c, &missing, headers)
        .map(|_| 2)
        .map_err(|s| c.status_error(&s, "AddMultipleToStore"))
}

// ---- fetching ---------------------------------------------------------------

/// A `FetchNars` stream: one zstd stream across every path, flushed at each
/// frame, so a frame's decompressed bytes belong to `paths[path_index]`.
struct NarDecoder {
    dec: zstd::stream::write::Decoder<'static, Vec<u8>>,
}

impl NarDecoder {
    fn new() -> io::Result<NarDecoder> {
        Ok(NarDecoder {
            dec: zstd::stream::write::Decoder::new(Vec::new())?,
        })
    }

    /// The decompressed bytes of one frame.
    fn feed(&mut self, data: &[u8]) -> io::Result<Vec<u8>> {
        self.dec.write_all(data)?;
        self.dec.flush()?;
        Ok(std::mem::take(self.dec.get_mut()))
    }
}

/// What `fetch_nars` hands each frame to: the path index, the frame's
/// decompressed bytes, and whether it is that path's last frame.
type OnFrame<'a> = &'a mut dyn FnMut(usize, &[u8], bool) -> io::Result<()>;

/// `FetchNars(paths)`, frame by frame.
fn fetch_nars(c: &Client, paths: &[StorePath], on_frame: OnFrame<'_>) -> Result<()> {
    let headers = c.headers()?;
    let mut stub = c.stub.clone();
    let req = proto::FetchNarsRequest {
        paths: paths.iter().map(|p| p.base_name().to_string()).collect(),
    };
    let mut stream =
        c.rt.block_on(async { stub.fetch_nars(c_request(req, &headers)).await })
            .map_err(|s| c.status_error(&s, "FetchNars"))?
            .into_inner();
    let mut dec = NarDecoder::new().map_err(|e| Error::io("zstd", e))?;
    while let Some(frame) =
        c.rt.block_on(stream.next())
            .transpose()
            .map_err(|s| c.status_error(&s, "FetchNars"))?
    {
        let i = frame.path_index as usize;
        if i >= paths.len() {
            return Err(Error::Protocol(format!(
                "FetchNars sent a frame for path {i}, only {} were asked for",
                paths.len()
            )));
        }
        let bytes = if frame.data.is_empty() {
            Vec::new()
        } else {
            dec.feed(&frame.data).map_err(|e| Error::io("zstd", e))?
        };
        on_frame(i, &bytes, frame.eof).map_err(|e| Error::io("restoring a NAR", e))?;
    }
    Ok(())
}

/// `--fetch-to DIR`: the farm never copies outputs into a local store, so the
/// NARs come back over `FetchNars`. Each is spooled beside its destination, so
/// the restore streams from disk instead of holding the whole NAR in memory.
fn fetch_into_dir(c: &mut Client, paths: &[StorePath], dir: &Path, t: &mut Timings) -> Result<()> {
    t.calls += 1;
    let spool_path = |i: usize| dir.join(format!(".nar-{i}"));
    let mut open: Option<(usize, std::fs::File)> = None;
    let r = fetch_nars(c, paths, &mut |i, bytes, eof| {
        if open.as_ref().is_none_or(|(j, _)| *j != i) {
            open = Some((i, std::fs::File::create(spool_path(i))?));
        }
        open.as_mut().expect("opened").1.write_all(bytes)?;
        if eof {
            open = None;
            let p = spool_path(i);
            let mut f = std::fs::File::open(&p)?;
            let out = restore_path(&mut f, &dir.join(paths[i].base_name()));
            drop(f);
            let _ = std::fs::remove_file(&p);
            return out;
        }
        Ok(())
    });
    for i in 0..paths.len() {
        let _ = std::fs::remove_file(spool_path(i));
    }
    r
}

/// Registers `paths` and their closure in the local nix-daemon so an out-link
/// points at a real local store path: `QueryPathInfos` on the remote for the
/// closure, then `FetchNars` for what the local store lacks, written straight
/// into one worker-protocol `AddMultipleToStore` with each path's
/// `ValidPathInfo` — signatures included, so the daemon accepts what the cache
/// signed. This is what `nix-farm`'s `nix copy --from <cache>` does.
pub fn register(
    cfg: &GrpcConfig,
    paths: &[StorePath],
    conn: &mut super::super::worker::Connection,
    on_event: &mut dyn FnMut(Event),
) -> Result<usize> {
    let mut c = Client::open(cfg)?;
    let mut t = Timings::default();
    let infos = remote_closure(&mut c, paths, &mut t)?;
    let all: Vec<String> = infos.keys().map(StorePath::to_string).collect();
    let valid: HashSet<String> = conn
        .query_valid_paths(&all, on_event)?
        .into_iter()
        .collect();
    let ordered: Vec<StorePath> = topo_sort(&infos)
        .into_iter()
        .filter(|p| !valid.contains(&p.to_string()))
        .collect();
    if ordered.is_empty() {
        return Ok(0);
    }
    let n = ordered.len();
    let cref = &c;
    let infos_ref = &infos;
    let order = &ordered;
    let mut body = |w: &mut dyn Write| -> io::Result<()> {
        write_u64(w, n as u64)?;
        let mut started = vec![false; n];
        fetch_nars(cref, order, &mut |i, bytes, _eof| {
            if !started[i] {
                started[i] = true;
                infos_ref[&order[i]].write(w)?;
            }
            w.write_all(bytes)
        })
        .map_err(io::Error::other)
    };
    conn.add_multiple_to_store(&mut body, on_event)?;
    Ok(n)
}

/// The closure of `paths` on the remote store, as path infos.
fn remote_closure(
    c: &mut Client,
    paths: &[StorePath],
    t: &mut Timings,
) -> Result<HashMap<StorePath, PathInfo>> {
    let mut infos: HashMap<StorePath, PathInfo> = HashMap::new();
    let mut queue: Vec<StorePath> = paths.to_vec();
    while !queue.is_empty() {
        let batch: Vec<StorePath> = std::mem::take(&mut queue)
            .into_iter()
            .filter(|p| !infos.contains_key(p))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        if batch.is_empty() {
            break;
        }
        let got = query_path_infos(c, &batch, t)?;
        for p in &batch {
            let Some(info) = got.get(p) else {
                return Err(Error::Protocol(format!(
                    "'{p}' has no path info on '{}'",
                    c.cfg.authority
                )));
            };
            for r in &info.references {
                let rp = StorePath::parse(r)?;
                if !infos.contains_key(&rp) {
                    queue.push(rp);
                }
            }
            infos.insert(p.clone(), info.clone());
        }
    }
    Ok(infos)
}

/// References before referrers, so `AddMultipleToStore` can register each path
/// as it arrives.
fn topo_sort(infos: &HashMap<StorePath, PathInfo>) -> Vec<StorePath> {
    let mut out = Vec::with_capacity(infos.len());
    let mut seen: HashSet<StorePath> = HashSet::new();
    let mut order: Vec<&StorePath> = infos.keys().collect();
    order.sort();
    for root in order {
        let mut stack = vec![(root.clone(), false)];
        while let Some((p, expanded)) = stack.pop() {
            if expanded {
                out.push(p);
                continue;
            }
            if !seen.insert(p.clone()) {
                continue;
            }
            stack.push((p.clone(), true));
            if let Some(info) = infos.get(&p) {
                for r in &info.references {
                    if let Ok(rp) = StorePath::parse(r)
                        && rp != p
                        && !seen.contains(&rp)
                    {
                        stack.push((rp, false));
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(name: &str, refs: &[&str]) -> PathInfo {
        PathInfo {
            path: StorePath::from_base_name(name).unwrap(),
            deriver: String::new(),
            nar_hash: "0".repeat(64),
            references: refs.iter().map(|r| format!("/nix/store/{r}")).collect(),
            registration_time: 0,
            nar_size: 0,
            ultimate: false,
            sigs: vec![],
            ca: String::new(),
        }
    }

    #[test]
    fn topo_sort_puts_references_first() {
        let a = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-a";
        let b = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-b";
        let c = "cccccccccccccccccccccccccccccccc-c";
        let mut infos = HashMap::new();
        for i in [info(a, &[b]), info(b, &[c]), info(c, &[])] {
            infos.insert(i.path.clone(), i);
        }
        let order: Vec<String> = topo_sort(&infos)
            .into_iter()
            .map(|p| p.base_name().to_string())
            .collect();
        assert_eq!(order, vec![c.to_string(), b.to_string(), a.to_string()]);
    }

    #[test]
    fn a_self_reference_does_not_loop() {
        let a = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-a";
        let mut infos = HashMap::new();
        let i = info(a, &[a]);
        infos.insert(i.path.clone(), i);
        assert_eq!(topo_sort(&infos).len(), 1);
    }

    #[test]
    fn required_features_come_from_the_env_or_structured_attrs() {
        let mut drv = Derivation {
            name: "x".into(),
            platform: "x86_64-linux".into(),
            ..Derivation::default()
        };
        drv.env_set("requiredSystemFeatures", "kvm big-parallel".into());
        assert_eq!(
            required_system_features(&drv),
            vec!["big-parallel".to_string(), "kvm".to_string()]
        );
        drv.structured_attrs = Some(r#"{"requiredSystemFeatures":["kvm"]}"#.into());
        assert_eq!(required_system_features(&drv), vec!["kvm".to_string()]);
    }
}

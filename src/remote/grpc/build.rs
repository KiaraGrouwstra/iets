//! The `grpc://` protocol calls, mirroring `worker/build.rs`: one
//! `QueryValidPaths`, one `AddMultipleToStore` of what the store lacks, builds
//! — `BuildDerivation` per derivation in farm mode, `BuildPaths` otherwise —
//! and `FetchNars` for the outputs. `backend.rs` drives them as the
//! dispatcher's backend.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::{self, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use tokio_stream::StreamExt;
use tokio_stream::wrappers::ReceiverStream;

use crate::error::{Error, Result};
use crate::store::drv::Derivation;
use crate::store::nar::restore_path;
use crate::store::path::StorePath;

use super::super::copy::topo_sort;
use super::super::event::{Event, Log, Phase};
use super::super::plan::{Object, Plan};
use super::super::worker::build::write_object;
use super::super::worker::wire::write_u64;
use super::drvwire::{
    BUILD_PROTOCOL, BuildResult, PathInfo, read_keyed_build_results, write_basic_derivation,
};
use super::{Client, Headers, c_request, first_line, gone_away, proto};

/// What one `build` call did, in the shape `--timings` prints.
#[derive(Debug, Default, Clone)]
pub struct Timings {
    pub connect: Duration,
    pub query: Duration,
    pub resolve: Duration,
    pub substitute: Duration,
    pub add: Duration,
    pub build: Duration,
    pub outputs: Duration,
    pub fetch: Duration,
    pub farm: bool,
    pub trusted: Option<bool>,
    pub query_paths: usize,
    pub missing: usize,
    /// Missing sources the client located and hashed.
    pub resolve_paths: usize,
    /// Of those, the ones with no eval-time origin, found through the local store.
    pub store_sources: usize,
    /// Missing sources left to the store's substituters.
    pub substitute_paths: usize,
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
            "plan: {} sources, {} drvs, {:.3}s\nconnect: gRPC {mode}, {trusted}, {:.3}s\nquery: {} paths, {} missing, {:.3}s\nresolve: {} sources",
            plan.sources.len(),
            plan.drvs.len(),
            plan_time.as_secs_f64(),
            self.connect.as_secs_f64(),
            self.query_paths,
            self.missing,
            self.query.as_secs_f64(),
            self.resolve_paths,
        );
        if self.store_sources > 0 {
            s.push_str(&format!(
                ", {} not from this eval, read from /nix/store",
                self.store_sources
            ));
        }
        s.push_str(&format!(", {:.3}s", self.resolve.as_secs_f64()));
        if self.substitute_paths > 0 {
            s.push_str(&format!(
                "\nsubstitute: {} paths left to the store, {:.3}s",
                self.substitute_paths,
                self.substitute.as_secs_f64()
            ));
        }
        s.push_str(&format!(
            "\nadd: {} paths, {} bytes, {:.3}s\nbuild: {} drvs, {} already valid, {:.3}s\noutputs: {:.3}s",
            self.add_paths,
            self.add_bytes,
            self.add.as_secs_f64(),
            self.drvs,
            self.drvs_cached,
            self.build.as_secs_f64(),
            self.outputs.as_secs_f64(),
        ));
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

/// The sources the client cannot produce (an input-addressed build output
/// pinned with `builtins.storePath`): the store fetches them instead. Non-farm
/// realises them now, as `worker/build.rs` does with
/// `BuildPathsWithResults`; a farm substitutes an input when the build asks
/// for it (daemon/main.cc:`gatherInputs`), so there we only check that some
/// substituter has them, which is what `QueryValidPaths` with `substitute`
/// answers in farm mode.
pub fn substitute(
    c: &mut Client,
    paths: &[StorePath],
    t: &mut Timings,
    on_event: &mut dyn FnMut(Event),
) -> Result<()> {
    if paths.is_empty() {
        return Ok(());
    }
    if c.info.farm {
        t.calls += 1;
        let req = proto::QueryValidPathsRequest {
            paths: paths.iter().map(|p| p.base_name().to_string()).collect(),
            substitute: true,
        };
        let reply = c.call("QueryValidPaths", |mut stub, h| {
            let req = req.clone();
            async move { stub.query_valid_paths(c_request(req, &h)).await }
        })?;
        let known: HashSet<String> = reply.paths.into_iter().collect();
        for p in paths {
            if !known.contains(p.base_name()) {
                return Err(Error::Store(format!(
                    "'{p}' is not in the local store and no substituter of '{}' has it",
                    c.cfg.authority
                )));
            }
        }
        return Ok(());
    }
    let targets: Vec<String> = paths.iter().map(ToString::to_string).collect();
    for (path, res) in build_paths(c, targets, t, on_event)? {
        if !res.success() {
            return Err(Error::Store(format!(
                "cannot substitute '{path}' on the builder: status {}: {}",
                res.status, res.error_msg
            )));
        }
    }
    Ok(())
}

/// Derivations parsed from their ATerms. A farm build needs every derivation
/// of the batch's closure; a non-farm build needs only the roots, whose
/// outputs it reports.
pub fn drv_table(drvs: &[crate::build::backend::Drv]) -> Result<BTreeMap<StorePath, Derivation>> {
    let mut out = BTreeMap::new();
    for d in drvs {
        let text = std::str::from_utf8(&d.aterm)
            .map_err(|_| Error::Plan(format!("derivation '{}' is not UTF-8", d.path)))?;
        let drv = Derivation::parse(text, Derivation::name_from_path(&d.path)?)?;
        out.insert(d.path.clone(), drv);
    }
    Ok(out)
}

/// The store path of `drv_path!output`, statically.
pub fn output_path(
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
pub fn query_valid_paths(
    c: &mut Client,
    names: &[String],
    t: &mut Timings,
) -> Result<HashSet<String>> {
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
pub fn add_multiple(c: &mut Client, objects: &[&Object], t: &mut Timings) -> Result<u64> {
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
    crate::remote::plan::prehash_drvs(objects);
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

/// One `BuildPaths` call: the targets are `DerivedPath`s in nix's own string
/// notation, and the reply carries the `vector<KeyedBuildResult>` bytes.
/// `log_line` and `phase` become `Log` events while it runs.
pub fn build_paths(
    c: &mut Client,
    targets: Vec<String>,
    t: &mut Timings,
    on_event: &mut dyn FnMut(Event),
) -> Result<Vec<(String, BuildResult)>> {
    let req = proto::BuildPathsRequest {
        targets,
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

    let mut results: Option<Vec<(String, BuildResult)>> = None;
    while let Some(msg) =
        c.rt.block_on(stream.next())
            .transpose()
            .map_err(|s| c.status_error(&s, "BuildPaths"))?
    {
        match msg.event {
            Some(proto::build_paths_chunk::Event::LogLine(line)) => {
                on_event(Event::Log(Log {
                    drv_path: String::new(),
                    line,
                }));
            }
            Some(proto::build_paths_chunk::Event::Phase(phase)) => {
                on_event(Event::Log(Log {
                    drv_path: String::new(),
                    line: format!("phase: {phase}").into_bytes(),
                }));
            }
            Some(proto::build_paths_chunk::Event::Done(done)) => {
                results = Some(read_keyed_build_results(&mut done.results.as_slice())?);
            }
            None => {}
        }
    }
    results.ok_or_else(|| Error::Protocol("gRPC BuildPaths stream ended without a result".into()))
}

/// One derivation the farm must build, in the shape `BuildDerivation` wants.
pub struct Job {
    pub drv_path: StorePath,
    pub drv: Derivation,
    pub input_srcs: BTreeSet<StorePath>,
    pub features: Vec<String>,
}

impl Job {
    /// The features the balancer routes on.
    pub fn new(drv_path: StorePath, drv: Derivation, input_srcs: BTreeSet<StorePath>) -> Job {
        let features = required_system_features(&drv);
        Job {
            drv_path,
            drv,
            input_srcs,
            features,
        }
    }
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
pub fn build_derivation(
    c: &Client,
    job: &Job,
    objects: &Objects,
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
                calls += upload_closure(c, objects, &job.drv_path, &headers)?;
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
            // The call knows its derivation, so the phase is attributed the
            // way the daemon's `setPhase` result is.
            Some(proto::build_derivation_chunk::Event::Phase(phase)) => {
                let _ = tx.send(Event::Phase(Phase {
                    drv_path: drv.clone(),
                    phase,
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

/// What this run can re-send, by store path base name, so a `NOT_FOUND` retry
/// re-sends just the closure of one derivation (store.cc:`uploadDrvClosure`).
/// Everything this run registered is here; a path the store already had is not
/// ours to send again, and neither is a substituted source.
#[derive(Default)]
pub struct Objects {
    by_name: HashMap<String, Object>,
}

impl Objects {
    /// Remembers what an `AddMultipleToStore` carried.
    pub fn extend(&mut self, objects: &[&Object]) {
        for o in objects {
            self.by_name
                .insert(o.store_path().base_name().to_string(), (*o).clone());
        }
    }

    /// `drv_path` and everything it references, references before referrers.
    fn closure(&self, drv_path: &StorePath) -> Vec<&Object> {
        let mut out = Vec::new();
        let mut seen: HashSet<&str> = HashSet::new();
        let mut stack = vec![(drv_path.base_name().to_string(), false)];
        while let Some((name, expanded)) = stack.pop() {
            let Some(o) = self.by_name.get(name.as_str()) else {
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
    objects: &Objects,
    drv_path: &StorePath,
    headers: &Headers,
) -> Result<usize> {
    let closure = objects.closure(drv_path);
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
pub fn fetch_into_dir(
    c: &mut Client,
    paths: &[StorePath],
    dir: &Path,
    t: &mut Timings,
) -> Result<()> {
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
    c: &mut Client,
    paths: &[StorePath],
    conn: &mut super::super::worker::Connection,
    on_event: &mut dyn FnMut(Event),
) -> Result<usize> {
    let mut t = Timings::default();
    let infos = remote_closure(c, paths, &mut t)?;
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
    conn.add_multiple_to_store(false, &mut body, on_event)
        .map_err(|e| {
            Error::Protocol(format!(
                "{e}\nhint: '{}' publishes its outputs to a binary cache. The local daemon \
             checks their signatures, as `nix copy --from` does, so add the cache's public \
             key to `trusted-public-keys` in nix.conf, or use --no-link.",
                c.cfg.url()
            ))
        })?;
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

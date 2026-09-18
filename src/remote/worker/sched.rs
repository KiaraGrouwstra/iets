//! Scheduling of the upload and the builds over a pool of worker-protocol connections.
//!
//! With `Opts::default()` this is the flow of `build::build_on`: one connection, one
//! `QueryValidPaths`, one `AddMultipleToStore` of everything missing, one
//! `BuildPathsWithResults` on the root. The levers, each off by default, change how the
//! plan is cut into ops and over how many connections (docs/scheduling.md):
//!
//! - `connections`: N connections, each served by its own thread and, on the daemon side,
//!   its own process and `Worker` (unix/daemon.cc forks per connection; `--stdio` is one).
//! - `upload_connections`, `upload_batch`: `AddMultipleToStore` ops of at most `upload_batch`
//!   objects, cut in topological order, on up to `upload_connections` of the pool at once.
//!   An object joins a batch only when each reference is valid remotely or earlier in the
//!   same batch: local-store.cc:registerValidPaths rejects a path whose reference is absent.
//! - `per_drv`: `BuildPathsWithResults` ops of at most k ready derivations (0: every ready
//!   one) instead of one op on the root; a derivation is ready when its drv is registered
//!   and every input derivation's outputs are valid. This is what gives per-node completion.
//! - `overlap`: builds are submitted while the upload is still running, on the connections
//!   that are not uploading. Needs `per_drv` and at least two connections to change anything.
//! - `critical_path`: uploads and submissions in order of the longest path to the root (a
//!   derivation weighs 1, a fixed-output derivation and a source 0) instead of plan order.
//! - `skip_substitutable`: paths the daemon can substitute are not uploaded when every path
//!   that references them is skipped too; the daemon fetches that sub-closure itself
//!   (build/derivation-trampoline-goal.cc makes an Opaque goal for an absent drv).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::store::drv::{Derivation, DerivationOutput};
use crate::store::path::StorePath;

use super::super::plan::{Object, Plan, SourceKind};
use super::super::proto::Done;
use super::super::proto::build_event::Event;
use super::build::{Timings, write_object};
use super::wire::write_u64;
use super::{BuildResult, Connection, Stats, Transport, WorkerError, built_event, failed_event};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Opts {
    pub connections: usize,
    pub upload_connections: usize,
    /// Objects per `AddMultipleToStore`; 0 is unbounded.
    pub upload_batch: usize,
    pub overlap: bool,
    pub critical_path: bool,
    /// `Some(k)`: builds in ops of at most k ready derivations, 0 for all ready ones.
    pub per_drv: Option<usize>,
    pub skip_substitutable: bool,
}

impl Default for Opts {
    fn default() -> Self {
        Opts {
            connections: 1,
            upload_connections: 1,
            upload_batch: 0,
            overlap: false,
            critical_path: false,
            per_drv: None,
            skip_substitutable: false,
        }
    }
}

impl Opts {
    /// The levers that need per-derivation submission turn it on.
    pub fn normalized(mut self) -> Self {
        self.connections = self.connections.max(1);
        self.upload_connections = self.upload_connections.clamp(1, self.connections);
        if self.overlap && self.per_drv.is_none() {
            self.per_drv = Some(0);
        }
        self
    }
}

/// The plan as a graph over object indices.
struct Graph {
    is_drv: Vec<bool>,
    refs: Vec<Vec<usize>>,
    /// Input derivations of each drv with the output names it wants of them.
    input_drvs: Vec<Vec<(usize, Vec<String>)>>,
    /// Output name and store path of each drv (empty for sources and unknown outputs).
    outputs: Vec<Vec<(String, String)>>,
    /// Upload and submission order.
    order: Vec<usize>,
    root: usize,
    by_path: HashMap<String, usize>,
}

impl Graph {
    fn new(plan: &Plan, root: &StorePath, critical_path: bool) -> Result<Graph, WorkerError> {
        let n = plan.objects.len();
        let by_path: HashMap<String, usize> = plan
            .objects
            .iter()
            .enumerate()
            .map(|(i, o)| (o.store_path().to_string(), i))
            .collect();
        let root_idx = *by_path
            .get(&root.to_string())
            .ok_or_else(|| WorkerError(format!("root {root} is not in the plan")))?;
        let mut is_drv = vec![false; n];
        let mut refs = vec![Vec::new(); n];
        let mut input_drvs = vec![Vec::new(); n];
        let mut outputs = vec![Vec::new(); n];
        let mut weight = vec![0u32; n];
        for (i, o) in plan.objects.iter().enumerate() {
            match o {
                Object::Source { kind, .. } => {
                    if let SourceKind::Text { references } = kind {
                        refs[i] = references
                            .iter()
                            .filter_map(|r| by_path.get(&r.to_string()).copied())
                            .collect();
                    }
                }
                Object::Drv {
                    path,
                    aterm,
                    references,
                    ..
                } => {
                    is_drv[i] = true;
                    refs[i] = references
                        .iter()
                        .filter_map(|r| by_path.get(&r.to_string()).copied())
                        .collect();
                    let text = std::str::from_utf8(aterm)
                        .map_err(|_| WorkerError(format!("{path}: ATerm is not UTF-8")))?;
                    let name = Derivation::name_from_path(path).map_err(|e| WorkerError(e.0))?;
                    let drv = Derivation::parse(text, name).map_err(|e| WorkerError(e.0))?;
                    input_drvs[i] = drv
                        .input_drvs
                        .iter()
                        .filter_map(|(d, names)| {
                            by_path
                                .get(&d.to_string())
                                .map(|&r| (r, names.iter().cloned().collect()))
                        })
                        .collect();
                    let fixed = drv
                        .outputs
                        .values()
                        .any(|o| matches!(o, DerivationOutput::Fixed { .. }));
                    weight[i] = if fixed { 0 } else { 1 };
                    for (out_name, o) in &drv.outputs {
                        // a deferred (CA) output has no static path: leave the drv's
                        // outputs unknown, so validity never marks it built
                        match o.path(name, out_name).map_err(|e| WorkerError(e.0))? {
                            Some(p) => outputs[i].push((out_name.clone(), p.to_string())),
                            None => {
                                outputs[i].clear();
                                break;
                            }
                        }
                    }
                }
            }
        }
        let mut order: Vec<usize> = (0..n).collect();
        if critical_path {
            // Plan order is topological (a referrer after its references), so referrers are
            // final when walked backwards. prio(x) = weight(x) + max prio over referrers.
            let mut referrers = vec![Vec::new(); n];
            for (i, rs) in refs.iter().enumerate() {
                for &r in rs {
                    referrers[r].push(i);
                }
            }
            let mut prio = vec![0u32; n];
            for i in (0..n).rev() {
                let above = referrers[i].iter().map(|&r| prio[r]).max().unwrap_or(0);
                prio[i] = weight[i] + above;
            }
            order.sort_by_key(|&i| (std::cmp::Reverse(prio[i]), i));
        }
        Ok(Graph {
            is_drv,
            refs,
            input_drvs,
            outputs,
            order,
            root: root_idx,
            by_path,
        })
    }
}

enum Job {
    Query(Vec<String>),
    Substitutable(Vec<String>),
    Upload(Vec<usize>),
    Build(Vec<String>),
    Missing(Vec<String>),
    OutputMap(String),
    Fetch(String, PathBuf),
}

enum Reply {
    Valid(Vec<String>),
    Substitutable(Vec<String>),
    Uploaded { bytes: u64 },
    Built(Vec<BuildResult>),
    Missing(super::Missing),
    OutputMap(BTreeMap<String, Option<String>>),
    Fetched,
}

enum Msg {
    Event(Event),
    Opened {
        conn: usize,
        result: Result<(u64, Option<bool>), WorkerError>,
    },
    Done {
        conn: usize,
        reply: Result<Reply, WorkerError>,
        took: Duration,
    },
    Closed {
        stats: Stats,
    },
}

fn serve(
    idx: usize,
    transport: &Transport,
    jobs: mpsc::Receiver<Job>,
    tx: mpsc::Sender<Msg>,
    plan: &Plan,
) {
    let mut ev = |e: Event| {
        let _ = tx.send(Msg::Event(e));
    };
    let mut conn = match Connection::open(transport, &mut ev) {
        Ok(c) => c,
        Err(e) => {
            let _ = tx.send(Msg::Opened {
                conn: idx,
                result: Err(e),
            });
            return;
        }
    };
    let _ = tx.send(Msg::Opened {
        conn: idx,
        result: Ok((conn.minor, conn.trusted)),
    });
    for job in jobs {
        let t = Instant::now();
        let reply = run_job(&mut conn, job, plan, &mut ev);
        let _ = tx.send(Msg::Done {
            conn: idx,
            reply,
            took: t.elapsed(),
        });
    }
    let _ = tx.send(Msg::Closed { stats: conn.stats });
}

fn run_job(
    conn: &mut Connection,
    job: Job,
    plan: &Plan,
    ev: &mut dyn FnMut(Event),
) -> Result<Reply, WorkerError> {
    match job {
        Job::Query(paths) => Ok(Reply::Valid(conn.query_valid_paths(&paths, ev)?)),
        Job::Substitutable(paths) => Ok(Reply::Substitutable(
            conn.query_substitutable_path_infos(&paths, ev)?,
        )),
        Job::Upload(batch) => {
            let before = conn.bytes_sent();
            let mut body = |w: &mut dyn Write| -> io::Result<()> {
                write_u64(w, batch.len() as u64)?;
                for &i in &batch {
                    write_object(w, &plan.objects[i])?;
                }
                Ok(())
            };
            conn.add_multiple_to_store(&mut body, ev)?;
            Ok(Reply::Uploaded {
                bytes: conn.bytes_sent() - before,
            })
        }
        Job::Build(paths) => Ok(Reply::Built(conn.build_paths_with_results(&paths, ev)?)),
        Job::Missing(paths) => Ok(Reply::Missing(conn.query_missing(&paths, ev)?)),
        Job::OutputMap(drv) => Ok(Reply::OutputMap(
            conn.query_derivation_output_map(&drv, ev)?,
        )),
        Job::Fetch(path, dest) => {
            conn.nar_from_path(&path, ev, &mut |r| {
                crate::store::nar::restore_path(r, &dest)
            })?;
            Ok(Reply::Fetched)
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Upload {
    Missing,
    InFlight,
    Valid,
    /// Substitutable and not referenced by any uploaded path: the daemon fetches it.
    Skipped,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Build {
    Pending,
    Building,
    Built,
}

struct State<'a> {
    plan: &'a Plan,
    g: Graph,
    opts: Opts,
    root: &'a StorePath,
    spec: String,
    upload_only: bool,
    fetch_to: Option<&'a Path>,
    senders: Vec<mpsc::Sender<Job>>,
    idle: Vec<bool>,
    upload: Vec<Upload>,
    build: Vec<Build>,
    /// Output names each drv is wanted for, from the validity walk.
    wanted: Vec<std::collections::BTreeSet<String>>,
    /// Which objects each in-flight upload carries, by connection.
    in_flight: Vec<Vec<usize>>,
    /// Which drvs each in-flight build op carries, by connection.
    building: Vec<Vec<usize>>,
    queried: bool,
    sub_queried: bool,
    missing_queried: bool,
    root_submitted: bool,
    outputs_asked: bool,
    built_outputs: Vec<(String, String)>,
    fetch_queue: Vec<(String, PathBuf)>,
    fetch_pending: usize,
    finished: bool,
    t: Timings,
    add_started: Option<Instant>,
    build_started: Option<Instant>,
    fetch_started: Option<Instant>,
}

impl State<'_> {
    fn send(&mut self, conn: usize, job: Job) {
        self.idle[conn] = false;
        let _ = self.senders[conn].send(job);
    }

    fn uploads_left(&self) -> bool {
        self.upload
            .iter()
            .any(|s| matches!(s, Upload::Missing | Upload::InFlight))
    }

    fn all_valid(&self) -> bool {
        self.upload
            .iter()
            .all(|s| matches!(s, Upload::Valid | Upload::Skipped))
    }

    /// The next `AddMultipleToStore` body: objects in upload order whose references are
    /// valid or earlier in this batch, at most `upload_batch` of them.
    fn next_upload_batch(&self) -> Vec<usize> {
        let limit = if self.opts.upload_batch == 0 {
            usize::MAX
        } else {
            self.opts.upload_batch
        };
        let mut batch = Vec::new();
        let mut in_batch = HashSet::new();
        for &i in &self.g.order {
            if self.upload[i] != Upload::Missing {
                continue;
            }
            let ready = self.g.refs[i].iter().all(|&r| {
                matches!(self.upload[r], Upload::Valid | Upload::Skipped) || in_batch.contains(&r)
            });
            if !ready {
                continue;
            }
            in_batch.insert(i);
            batch.push(i);
            if batch.len() >= limit {
                break;
            }
        }
        batch
    }

    fn input_done(&self, r: usize) -> bool {
        self.build[r] == Build::Built || self.upload[r] == Upload::Skipped
    }

    /// Ready derivations in order: registered, pending, every input derivation done.
    fn next_build_batch(&self, k: usize) -> Vec<usize> {
        let limit = if k == 0 { usize::MAX } else { k };
        let mut batch = Vec::new();
        for &i in &self.g.order {
            if !self.g.is_drv[i]
                || self.build[i] != Build::Pending
                || self.upload[i] != Upload::Valid
            {
                continue;
            }
            if !self.g.input_drvs[i]
                .iter()
                .all(|(r, _)| self.input_done(*r))
            {
                continue;
            }
            batch.push(i);
            if batch.len() >= limit {
                break;
            }
        }
        batch
    }

    /// `drv!outputs`: the root's requested outputs, an input's wanted ones.
    fn derived_path(&self, i: usize) -> String {
        let p = self.plan.objects[i].store_path();
        if i == self.g.root {
            format!("{p}!{}", self.spec)
        } else if self.wanted[i].is_empty() {
            format!("{p}!*")
        } else {
            let names: Vec<&str> = self.wanted[i].iter().map(String::as_str).collect();
            format!("{p}!{}", names.join(","))
        }
    }

    /// Hands work to idle connections. Called after every state change.
    fn dispatch(&mut self) -> Result<(), WorkerError> {
        if self.finished {
            return Ok(());
        }
        let n = self.senders.len();
        if !self.queried {
            if self.idle[0] {
                let mut paths: Vec<String> = self
                    .plan
                    .objects
                    .iter()
                    .map(|o| o.store_path().to_string())
                    .collect();
                if self.opts.per_drv.is_some() {
                    for outs in &self.g.outputs {
                        paths.extend(outs.iter().map(|(_, p)| p.clone()));
                    }
                }
                self.t.query_paths = paths.len();
                self.send(0, Job::Query(paths));
            }
            return Ok(());
        }
        if self.opts.skip_substitutable && !self.sub_queried {
            if self.idle[0] {
                let missing: Vec<String> = (0..self.upload.len())
                    .filter(|&i| self.upload[i] == Upload::Missing)
                    .map(|i| self.plan.objects[i].store_path().to_string())
                    .collect();
                self.send(0, Job::Substitutable(missing));
            }
            return Ok(());
        }
        // uploads on the first `upload_connections`
        let uploaders = self.opts.upload_connections.min(n);
        if self.uploads_left() {
            for c in 0..uploaders {
                if !self.idle[c] {
                    continue;
                }
                let batch = self.next_upload_batch();
                if batch.is_empty() {
                    break;
                }
                self.add_started.get_or_insert_with(Instant::now);
                for &i in &batch {
                    self.upload[i] = Upload::InFlight;
                }
                self.in_flight[c] = batch.clone();
                self.send(c, Job::Upload(batch));
            }
        }
        if self.upload_only {
            if self.all_valid() {
                self.finished = true;
            }
            return Ok(());
        }
        if self.build[self.g.root] == Build::Built {
            return self.finish();
        }
        if self.opts.skip_substitutable && !self.missing_queried {
            // what the daemon will build and substitute, now that it can read the drvs
            if self.all_valid() && self.idle[0] {
                let p = self.derived_path(self.g.root);
                self.send(0, Job::Missing(vec![p]));
            }
            return Ok(());
        }
        match self.opts.per_drv {
            None => {
                if self.all_valid() && self.idle[0] && !self.root_submitted {
                    self.root_submitted = true;
                    self.build[self.g.root] = Build::Building;
                    self.building[0] = vec![self.g.root];
                    self.build_started = Some(Instant::now());
                    let p = self.derived_path(self.g.root);
                    self.send(0, Job::Build(vec![p]));
                }
            }
            Some(k) => {
                let uploading = self.uploads_left();
                if uploading && !self.opts.overlap {
                    return Ok(());
                }
                for c in 0..n {
                    if !self.idle[c] || (uploading && c < uploaders) {
                        continue;
                    }
                    let batch = self.next_build_batch(k);
                    if batch.is_empty() {
                        break;
                    }
                    self.build_started.get_or_insert_with(Instant::now);
                    for &i in &batch {
                        self.build[i] = Build::Building;
                    }
                    self.building[c] = batch.clone();
                    let paths = batch.iter().map(|&i| self.derived_path(i)).collect();
                    self.send(c, Job::Build(paths));
                }
            }
        }
        Ok(())
    }

    /// After the root is built: the output map, then the fetches.
    fn finish(&mut self) -> Result<(), WorkerError> {
        let Some(c) = self.idle.iter().position(|&i| i) else {
            return Ok(());
        };
        if !self.outputs_asked {
            self.outputs_asked = true;
            self.send(c, Job::OutputMap(self.root.to_string()));
            return Ok(());
        }
        let mut c = Some(c);
        while let Some((path, dest)) = self.fetch_queue.pop() {
            let Some(conn) = c.take().or_else(|| self.idle.iter().position(|&i| i)) else {
                self.fetch_queue.push((path, dest));
                return Ok(());
            };
            self.fetch_started.get_or_insert_with(Instant::now);
            self.fetch_pending += 1;
            self.send(conn, Job::Fetch(path, dest));
        }
        Ok(())
    }

    fn on_done(
        &mut self,
        conn: usize,
        reply: Reply,
        took: Duration,
        on_event: &mut dyn FnMut(Event),
    ) -> Result<(), WorkerError> {
        match reply {
            Reply::Valid(valid) => {
                let valid: HashSet<String> = valid.into_iter().collect();
                for (i, o) in self.plan.objects.iter().enumerate() {
                    if valid.contains(&o.store_path().to_string()) {
                        self.upload[i] = Upload::Valid;
                    }
                }
                // What the daemon's goals would visit: from the root, a derivation with
                // its wanted outputs; one whose wanted outputs are valid is built and its
                // inputs are not visited (build/derivation-goal.cc). Everything else
                // counts as built so it is neither submitted nor waited for.
                let n = self.upload.len();
                let root_names: Vec<String> = if self.spec == "*" {
                    Vec::new()
                } else {
                    self.spec.split(',').map(str::to_string).collect()
                };
                let mut needed = vec![false; n];
                let mut stack = vec![(self.g.root, root_names)];
                while let Some((i, names)) = stack.pop() {
                    let outs = &self.g.outputs[i];
                    let names: Vec<String> = if names.is_empty() {
                        outs.iter().map(|(n, _)| n.clone()).collect()
                    } else {
                        names
                    };
                    let new: Vec<String> = names
                        .into_iter()
                        .filter(|n| !self.wanted[i].contains(n))
                        .collect();
                    if new.is_empty() {
                        continue;
                    }
                    self.wanted[i].extend(new);
                    let built = !outs.is_empty()
                        && self.wanted[i]
                            .iter()
                            .all(|n| outs.iter().any(|(name, p)| name == n && valid.contains(p)));
                    if built {
                        continue;
                    }
                    needed[i] = true;
                    self.build[i] = Build::Pending;
                    for (r, want) in &self.g.input_drvs[i] {
                        stack.push((*r, want.clone()));
                    }
                }
                for (i, need) in needed.iter().enumerate() {
                    if self.g.is_drv[i] && !need {
                        self.build[i] = Build::Built;
                    }
                }
                self.t.missing = self
                    .upload
                    .iter()
                    .filter(|s| **s == Upload::Missing)
                    .count();
                self.t.query = took;
                self.queried = true;
            }
            Reply::Substitutable(paths) => {
                let sub: HashSet<usize> = paths
                    .iter()
                    .filter_map(|p| self.g.by_path.get(p).copied())
                    .collect();
                // Root first: a path is skipped when it is substitutable and every path
                // that references it is skipped.
                let mut referrers = vec![Vec::new(); self.upload.len()];
                for (i, rs) in self.g.refs.iter().enumerate() {
                    for &r in rs {
                        referrers[r].push(i);
                    }
                }
                let mut skipped = 0;
                for i in (0..self.upload.len()).rev() {
                    if self.upload[i] != Upload::Missing || !sub.contains(&i) {
                        continue;
                    }
                    if referrers[i]
                        .iter()
                        .all(|&r| matches!(self.upload[r], Upload::Skipped | Upload::Valid))
                    {
                        self.upload[i] = Upload::Skipped;
                        skipped += 1;
                    }
                }
                self.t.skipped = skipped;
                self.t.substitutable = sub.len();
                self.t.query += took;
                self.sub_queried = true;
            }
            Reply::Uploaded { bytes } => {
                let batch = std::mem::take(&mut self.in_flight[conn]);
                self.t.add_paths += batch.len();
                self.t.add_bytes += bytes;
                self.t.add_ops += 1;
                for i in batch {
                    self.upload[i] = Upload::Valid;
                }
                if !self.uploads_left()
                    && let Some(s) = self.add_started
                {
                    self.t.add = s.elapsed();
                }
            }
            Reply::Built(results) => {
                let batch = std::mem::take(&mut self.building[conn]);
                self.t.build_ops += 1;
                let mut failed = false;
                for r in &results {
                    let drv = r.path.split('!').next().unwrap_or("");
                    let Some(&i) = self.g.by_path.get(drv) else {
                        continue;
                    };
                    if r.success() {
                        self.build[i] = Build::Built;
                    } else {
                        failed = true;
                        on_event(failed_event(
                            drv,
                            &format!("status {}: {}", r.status, r.msg),
                        ));
                    }
                }
                for i in batch {
                    if self.build[i] == Build::Building {
                        // entry-points.cc skips goals that never ran (ecBusy): another
                        // goal of the op failed and keep-going is off
                        failed = true;
                        self.build[i] = Build::Pending;
                        on_event(failed_event(
                            &self.plan.objects[i].store_path().to_string(),
                            "no result: the daemon cancelled it after another failure",
                        ));
                    }
                }
                if failed {
                    on_event(Event::Done(Done { success: false }));
                    self.finished = true;
                    return Ok(());
                }
                if self.build[self.g.root] == Build::Built
                    && let Some(s) = self.build_started
                {
                    self.t.build = s.elapsed();
                }
            }
            Reply::OutputMap(map) => {
                self.t.outputs = took;
                for (name, path) in &map {
                    if let Some(p) = path {
                        on_event(built_event(&self.root.to_string(), name, p));
                        self.built_outputs.push((name.clone(), p.clone()));
                    }
                }
                if let Some(dir) = self.fetch_to {
                    std::fs::create_dir_all(dir)?;
                    let wanted = self.spec != "*";
                    for (name, p) in &self.built_outputs {
                        if wanted && !self.spec.split(',').any(|w| w == name) {
                            continue;
                        }
                        let sp = StorePath::parse(p).map_err(|e| WorkerError(e.0))?;
                        let dest = dir.join(sp.base_name());
                        if dest.exists() {
                            return Err(WorkerError(format!(
                                "fetch: '{}' already exists",
                                dest.display()
                            )));
                        }
                        self.fetch_queue.push((p.clone(), dest));
                    }
                    if self.fetch_queue.is_empty() {
                        self.done(on_event);
                    }
                } else {
                    self.done(on_event);
                }
            }
            Reply::Fetched => {
                self.fetch_pending -= 1;
                self.t.fetched += 1;
                if self.fetch_queue.is_empty() && self.fetch_pending == 0 {
                    if let Some(s) = self.fetch_started {
                        self.t.fetch = s.elapsed();
                    }
                    self.done(on_event);
                }
            }
            Reply::Missing(m) => {
                self.t.will_build = m.will_build.len();
                self.t.will_substitute = m.will_substitute.len();
                self.missing_queried = true;
            }
        }
        Ok(())
    }

    fn done(&mut self, on_event: &mut dyn FnMut(Event)) {
        on_event(Event::Done(Done { success: true }));
        self.finished = true;
    }
}

/// `build::build` with levers: uploads what the daemon lacks, builds `root`, reports its
/// outputs, and restores the wanted outputs into `fetch_to` when given. With `upload_only`
/// the call stops after the upload.
#[allow(clippy::too_many_arguments)]
pub fn run(
    transport: &Transport,
    plan: &Plan,
    root: &StorePath,
    outputs: &[String],
    upload_only: bool,
    fetch_to: Option<&Path>,
    opts: Opts,
    on_event: &mut dyn FnMut(Event),
) -> Result<Timings, WorkerError> {
    let opts = opts.normalized();
    let t0 = Instant::now();
    let g = Graph::new(plan, root, opts.critical_path)?;
    let spec = if outputs.is_empty() {
        "*".to_string()
    } else {
        let names: std::collections::BTreeSet<&str> = outputs.iter().map(String::as_str).collect();
        names.into_iter().collect::<Vec<_>>().join(",")
    };
    let n = opts.connections;
    let (tx, rx) = mpsc::channel::<Msg>();
    let mut senders = Vec::with_capacity(n);
    let mut receivers = Vec::with_capacity(n);
    for _ in 0..n {
        let (jt, jr) = mpsc::channel::<Job>();
        senders.push(jt);
        receivers.push(jr);
    }
    let objects = plan.objects.len();
    let mut st = State {
        plan,
        g,
        opts: opts.clone(),
        root,
        spec,
        upload_only,
        fetch_to,
        senders,
        idle: vec![false; n],
        upload: vec![Upload::Missing; objects],
        build: vec![Build::Pending; objects],
        wanted: vec![std::collections::BTreeSet::new(); objects],
        in_flight: vec![Vec::new(); n],
        building: vec![Vec::new(); n],
        queried: false,
        sub_queried: false,
        missing_queried: false,
        root_submitted: false,
        outputs_asked: false,
        built_outputs: Vec::new(),
        fetch_queue: Vec::new(),
        fetch_pending: 0,
        finished: false,
        t: Timings {
            connections: n,
            ..Timings::default()
        },
        add_started: None,
        build_started: None,
        fetch_started: None,
    };
    std::thread::scope(|s| -> Result<Timings, WorkerError> {
        for (i, jr) in receivers.into_iter().enumerate() {
            let tx = tx.clone();
            s.spawn(move || serve(i, transport, jr, tx, plan));
        }
        drop(tx);
        let mut opened = 0;
        let mut result: Result<(), WorkerError> = Ok(());
        while !st.finished {
            let msg = match rx.recv() {
                Ok(m) => m,
                Err(_) => break,
            };
            let r = match msg {
                Msg::Event(e) => {
                    if matches!(e, Event::Started(_)) && st.t.first_build.is_none() {
                        st.t.first_build = Some(t0.elapsed());
                    }
                    on_event(e);
                    Ok(())
                }
                Msg::Opened { conn, result } => match result {
                    Ok((minor, trusted)) => {
                        opened += 1;
                        if conn == 0 {
                            st.t.minor = minor;
                            st.t.trusted = trusted;
                        }
                        if opened == n {
                            st.t.connect = t0.elapsed();
                        }
                        st.idle[conn] = true;
                        st.dispatch()
                    }
                    Err(e) => Err(e),
                },
                Msg::Done { conn, reply, took } => {
                    st.idle[conn] = true;
                    match reply {
                        Ok(reply) => st
                            .on_done(conn, reply, took, on_event)
                            .and_then(|_| st.dispatch()),
                        Err(e) => Err(e),
                    }
                }
                Msg::Closed { .. } => Ok(()),
            };
            if let Err(e) = r {
                result = Err(e);
                break;
            }
            if st.finished {
                break;
            }
            if opened == n && st.idle.iter().all(|&i| i) && !st.finished {
                // nothing in flight and nothing dispatched: the graph cannot progress
                result = Err(WorkerError(
                    "scheduler stalled: no ready derivation and nothing in flight".into(),
                ));
                break;
            }
        }
        st.t.total = t0.elapsed();
        st.senders.clear();
        for msg in rx {
            if let Msg::Closed { stats } = msg {
                st.t.stats.ops += stats.ops;
                st.t.stats.round_trips += stats.round_trips;
                st.t.stats.bytes_sent += stats.bytes_sent;
            }
        }
        result.map(|_| st.t.clone())
    })
}

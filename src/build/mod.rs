//! The one build path. Every ietsp command that realises a derivation goes
//! through `Dispatcher`: one worker-protocol connection carries the uploads
//! and the builds, each batch of ready derivations is one
//! `BuildPathsWithResults` (docs/scheduling.md: one op per ready set beats
//! one op per derivation by 7-10x, and the worker protocol is serial per
//! connection, so one op is in flight at a time), and every build reports
//! through one structured event stream with provenance attached.
//!
//! `ietsp build`, `nix-build` and `nix build` hand over a fixed set of roots
//! (`upload::PlanUpload`). `ietsp fast-build` hands over batches as the
//! eval-jobs walk finishes attributes (`upload::StreamUpload`), which is
//! what overlaps evaluation with building. See docs/fast-build.md.

pub mod event;
pub mod render;
pub mod results;
pub mod upload;

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::error::{Error, Result};
use crate::provenance::Provenance;
use crate::remote::event::Event;
use crate::remote::worker::{Connection, Options, Transport};
use crate::store::path::StorePath;

use event::{FastBuildEvent, Kind};
use render::{Renderer, Style};
use results::Record;
use upload::Uploader;

/// One derivation the caller asked for, under the name it wants to see.
#[derive(Debug, Clone)]
pub struct Target {
    /// Attribute path, or the derivation name when the caller has none.
    pub attr: String,
    pub drv: StorePath,
    /// Wanted output names; empty means every output (`!*`).
    pub outputs: Vec<String>,
    /// From `--check-cache-status`, for the result records.
    pub cache_status: Option<String>,
    /// What the renderer shows, when it differs from `attr`: the attribute
    /// plus the source position, which the records must not carry.
    pub label: Option<String>,
}

impl Target {
    pub fn new(attr: &str, drv: StorePath, outputs: Vec<String>) -> Target {
        Target {
            attr: attr.to_string(),
            drv,
            outputs,
            cache_status: None,
            label: None,
        }
    }

    /// The derived path the worker protocol wants: `<drv>!out1,out2` or `!*`.
    fn derived_path(&self) -> String {
        if self.outputs.is_empty() {
            return format!("{}!*", self.drv);
        }
        let names: BTreeSet<&str> = self.outputs.iter().map(String::as_str).collect();
        format!(
            "{}!{}",
            self.drv,
            names.into_iter().collect::<Vec<_>>().join(",")
        )
    }
}

/// What became of one target.
#[derive(Debug, Clone)]
pub struct Outcome {
    pub attr: String,
    pub drv: StorePath,
    pub success: bool,
    pub skipped: bool,
    pub reason: Option<String>,
    pub log_tail: Vec<String>,
    /// Output name to store path, for the targets that built.
    pub outputs: BTreeMap<String, String>,
    pub elapsed: Duration,
    pub cache_status: Option<String>,
}

impl Outcome {
    /// The build record for `--result-file` and `--stream-json-lines`.
    pub fn record(&self) -> Record {
        Record {
            success: self.success,
            duration: self.elapsed.as_secs_f64(),
            error: self.reason.clone(),
            log_output: (!self.log_tail.is_empty()).then(|| self.log_tail.join("\n")),
            outputs: (!self.outputs.is_empty()).then(|| self.outputs.clone()),
            drv_path: Some(self.drv.to_string()),
            cache_status: self.cache_status.clone(),
            skipped: self.skipped,
            ..Record::build(&self.attr)
        }
    }
}

/// The heartbeat thread: pokes the renderer so a CI log shows liveness and
/// a stalled build gets escalated even while the client sits in a read.
struct Heartbeat {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Heartbeat {
    fn start(render: Arc<Mutex<Renderer>>) -> Heartbeat {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let handle = std::thread::Builder::new()
            .name("build-heartbeat".into())
            .spawn(move || {
                let tick = Duration::from_millis(250);
                let mut waited = Duration::ZERO;
                while !flag.load(Ordering::Relaxed) {
                    std::thread::sleep(tick);
                    waited += tick;
                    if waited >= render::HEARTBEAT {
                        waited = Duration::ZERO;
                        render.lock().unwrap().heartbeat();
                    }
                }
            })
            .ok();
        Heartbeat { stop, handle }
    }
}

impl Drop for Heartbeat {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Where the events go: the renderer, the JSON-lines stream, the records.
pub struct Reporter {
    render: Arc<Mutex<Renderer>>,
    pub records: Vec<Record>,
    jsonl: Option<Box<dyn Write + Send>>,
    /// Per drv: when the build started and how long it ran.
    timing: HashMap<String, (Instant, Option<Duration>)>,
}

impl Reporter {
    pub fn new(style: Style) -> Reporter {
        Reporter {
            render: Arc::new(Mutex::new(Renderer::new(
                style,
                Box::new(std::io::stderr()),
            ))),
            records: Vec::new(),
            jsonl: None,
            timing: HashMap::new(),
        }
    }

    /// `--stream-json-lines`: every record also goes to this writer, one
    /// JSON object per line.
    pub fn stream_to(&mut self, w: Box<dyn Write + Send>) {
        self.jsonl = Some(w);
    }

    pub fn note(&mut self, msg: &str) {
        self.render.lock().unwrap().note(msg);
    }

    /// Records a result and streams it when `--stream-json-lines` is on.
    pub fn record(&mut self, r: Record) {
        if let Some(w) = &mut self.jsonl {
            let _ = results::write_json_line(w.as_mut(), &r);
        }
        self.records.push(r);
    }

    fn emit(&mut self, e: &FastBuildEvent) {
        self.render.lock().unwrap().event(e);
    }

    fn duration(&self, drv: &str) -> Duration {
        match self.timing.get(drv) {
            Some((_, Some(d))) => *d,
            Some((t, None)) => t.elapsed(),
            None => Duration::ZERO,
        }
    }

    fn tail(&self, drv: &str) -> Vec<String> {
        let r = self.render.lock().unwrap();
        let live = r.live_tail(drv);
        if live.is_empty() {
            r.retained_tail(drv)
        } else {
            live
        }
    }
}

/// The dispatcher: one connection, one event stream, batched build ops.
pub struct Dispatcher {
    pub conn: Connection,
    pub reporter: Reporter,
    prov: Provenance,
    seen: HashSet<StorePath>,
    /// `--retries N`: how many more times a failed target is submitted.
    pub retries: usize,
    pub failed: bool,
    pub upload: upload::Stats,
    /// Batches and build ops sent, for `--timings`.
    pub ops: usize,
    /// Time inside `BuildPathsWithResults` and `QueryDerivationOutputMap`.
    pub build_time: Duration,
    pub outputs_time: Duration,
    heartbeat: Option<Heartbeat>,
}

impl Dispatcher {
    pub fn open(
        transport: &Transport,
        options: &Options,
        prov: Provenance,
        style: Style,
    ) -> Result<Dispatcher> {
        let reporter = Reporter::new(style);
        let render = reporter.render.clone();
        let conn = {
            let mut first = |e: Event| {
                if let Event::Log(l) = e {
                    render
                        .lock()
                        .unwrap()
                        .note(&String::from_utf8_lossy(&l.line));
                }
            };
            Connection::open_with(transport, options, &mut first)?
        };
        Ok(Dispatcher::with_reporter(conn, prov, reporter))
    }

    /// Wraps an already-open connection.
    pub fn with_connection(conn: Connection, prov: Provenance, style: Style) -> Dispatcher {
        Dispatcher::with_reporter(conn, prov, Reporter::new(style))
    }

    fn with_reporter(conn: Connection, prov: Provenance, reporter: Reporter) -> Dispatcher {
        let heartbeat = Heartbeat::start(reporter.render.clone());
        Dispatcher {
            conn,
            reporter,
            prov,
            seen: HashSet::new(),
            retries: 0,
            failed: false,
            upload: upload::Stats::default(),
            ops: 0,
            build_time: Duration::ZERO,
            outputs_time: Duration::ZERO,
            heartbeat: Some(heartbeat),
        }
    }

    /// What one derivation is called: the attribute the caller gave, plus
    /// the source position when provenance knows it.
    fn label(&self, by_drv: &HashMap<String, String>, drv: &str) -> String {
        if let Some(attr) = by_drv.get(drv) {
            return match self.prov.site(drv).and_then(|s| s.pos.clone()) {
                Some(pos) => format!("{attr} ({pos})"),
                None => attr.clone(),
            };
        }
        self.prov
            .name_of(drv)
            .unwrap_or_else(|| strip_hash(drv).to_string())
    }

    /// Uploads what the batch needs and builds it in one op. Targets whose
    /// drv was already dispatched are dropped.
    pub fn submit(&mut self, up: &mut dyn Uploader, batch: Vec<Target>) -> Result<Vec<Outcome>> {
        let batch: Vec<Target> = batch
            .into_iter()
            .filter(|t| self.seen.insert(t.drv.clone()))
            .collect();
        if batch.is_empty() {
            return Ok(Vec::new());
        }
        let mut out = self.submit_once(up, &batch)?;
        for _ in 0..self.retries {
            let again: Vec<Target> = batch
                .iter()
                .filter(|t| {
                    out.iter()
                        .any(|o| o.drv == t.drv && !o.success && !o.skipped)
                })
                .cloned()
                .collect();
            if again.is_empty() {
                break;
            }
            self.reporter
                .note(&format!("retrying {} failed builds", again.len()));
            let retried = self.submit_once(up, &again)?;
            for r in retried {
                if let Some(slot) = out.iter_mut().find(|o| o.drv == r.drv) {
                    *slot = r;
                }
            }
        }
        self.failed |= out.iter().any(|o| !o.success && !o.skipped);
        Ok(out)
    }

    fn submit_once(&mut self, up: &mut dyn Uploader, batch: &[Target]) -> Result<Vec<Outcome>> {
        // The renderer's name for each target, and from it the label the
        // event sink hands every build in this batch.
        let by_drv: HashMap<String, String> = batch
            .iter()
            .map(|t| {
                (
                    t.drv.to_string(),
                    t.label.clone().unwrap_or_else(|| t.attr.clone()),
                )
            })
            .collect();
        let labels: HashMap<String, String> = by_drv
            .keys()
            .map(|d| (d.clone(), self.label(&by_drv, d)))
            .collect();
        let roots: Vec<StorePath> = batch.iter().map(|t| t.drv.clone()).collect();

        let stats = {
            let mut on_event = event_sink(&mut self.reporter, &self.prov, &by_drv);
            up.upload(&mut self.conn, &roots, &mut on_event)?
        };
        self.upload.merge(stats);

        let targets: Vec<String> = batch.iter().map(Target::derived_path).collect();
        self.ops += 1;
        let t_build = Instant::now();
        let results = {
            let mut on_event = event_sink(&mut self.reporter, &self.prov, &by_drv);
            self.conn
                .build_paths_with_results(&targets, &mut on_event)?
        };
        self.build_time += t_build.elapsed();

        // `<drv>!<outputs>` back to the drv it names.
        let mut status: HashMap<String, (bool, String)> = HashMap::new();
        for r in &results {
            let drv = r.path.split('!').next().unwrap_or(&r.path).to_string();
            let entry = status.entry(drv).or_insert((true, String::new()));
            if !r.success() {
                entry.0 = false;
                entry.1 = format!("status {}: {}", r.status, r.msg);
            }
        }

        let mut out = Vec::with_capacity(batch.len());
        for t in batch {
            let drv = t.drv.to_string();
            let attr = labels.get(&drv).cloned().unwrap_or_else(|| t.attr.clone());
            let elapsed = self.reporter.duration(&drv);
            match status.get(&drv) {
                Some((true, _)) => {
                    let t_out = Instant::now();
                    let map = {
                        let mut on_event = event_sink(&mut self.reporter, &self.prov, &by_drv);
                        self.conn.query_derivation_output_map(&drv, &mut on_event)?
                    };
                    self.outputs_time += t_out.elapsed();
                    let outputs: BTreeMap<String, String> = map
                        .into_iter()
                        .filter_map(|(n, p)| p.map(|p| (n, p)))
                        .filter(|(n, _)| t.outputs.is_empty() || t.outputs.contains(n))
                        .collect();
                    self.reporter.emit(&FastBuildEvent::new(
                        &attr,
                        &drv,
                        Kind::Built {
                            outputs: outputs.clone(),
                        },
                        elapsed,
                    ));
                    out.push(Outcome {
                        attr: t.attr.clone(),
                        drv: t.drv.clone(),
                        success: true,
                        skipped: false,
                        reason: None,
                        log_tail: Vec::new(),
                        outputs,
                        elapsed,
                        cache_status: t.cache_status.clone(),
                    });
                }
                Some((false, reason)) => {
                    let tail = self.reporter.tail(&drv);
                    // The daemon's message is several coloured lines; the
                    // records want one plain one, the log tail has the rest.
                    let reason = results::strip_ansi(&self.prov.rewrite_str(reason));
                    let reason = reason.lines().next().unwrap_or_default().to_string();
                    self.reporter.emit(&FastBuildEvent::new(
                        &attr,
                        &drv,
                        Kind::Failed {
                            reason: reason.clone(),
                            log_tail: tail.clone(),
                        },
                        elapsed,
                    ));
                    out.push(Outcome {
                        attr: t.attr.clone(),
                        drv: t.drv.clone(),
                        success: false,
                        skipped: false,
                        reason: Some(reason),
                        log_tail: tail,
                        outputs: BTreeMap::new(),
                        elapsed,
                        cache_status: t.cache_status.clone(),
                    });
                }
                // The daemon stopped before it reached this target:
                // `keep-going` is off and something earlier failed.
                None => {
                    let why = "an earlier build in the batch failed".to_string();
                    self.reporter.emit(&FastBuildEvent::new(
                        &attr,
                        &drv,
                        Kind::Skipped { why: why.clone() },
                        elapsed,
                    ));
                    out.push(Outcome {
                        attr: t.attr.clone(),
                        drv: t.drv.clone(),
                        success: false,
                        skipped: true,
                        reason: Some(why),
                        log_tail: Vec::new(),
                        outputs: BTreeMap::new(),
                        elapsed,
                        cache_status: t.cache_status.clone(),
                    });
                }
            }
        }
        Ok(out)
    }

    /// `--no-build`: register the closure and stop.
    pub fn upload_only(&mut self, up: &mut dyn Uploader, roots: &[StorePath]) -> Result<()> {
        let by_drv = HashMap::new();
        let stats = {
            let mut on_event = event_sink(&mut self.reporter, &self.prov, &by_drv);
            up.upload(&mut self.conn, roots, &mut on_event)?
        };
        self.upload.merge(stats);
        Ok(())
    }

    /// An indirect GC root at `link` for `store_path`.
    pub fn out_link(&mut self, store_path: &str, link: &Path) -> Result<()> {
        self.conn.add_perm_root(store_path, link)
    }

    /// Restores one output of a remote store into `dir`.
    pub fn fetch(&mut self, store_path: &str, dir: &Path) -> Result<()> {
        std::fs::create_dir_all(dir)
            .map_err(|e| Error::io(format!("cannot create {}", dir.display()), e))?;
        let sp = StorePath::parse(store_path)?;
        let dest = dir.join(sp.base_name());
        if dest.exists() {
            return Err(Error::Usage(format!(
                "fetch: '{}' already exists",
                dest.display()
            )));
        }
        self.conn.nar_from_path(store_path, &mut |_| {}, &mut |r| {
            crate::store::nar::restore_path(r, &dest)
        })
    }

    /// Records an attribute that never reached a build.
    pub fn record(&mut self, r: Record) {
        self.reporter.record(r);
    }

    pub fn into_connection(mut self) -> Connection {
        drop(self.heartbeat.take());
        self.conn
    }
}

/// The daemon's stream mapped onto `FastBuildEvent`s. A free function so the
/// caller keeps `Dispatcher::conn` borrowed at the same time.
fn event_sink<'a>(
    reporter: &'a mut Reporter,
    prov: &'a Provenance,
    by_drv: &'a HashMap<String, String>,
) -> impl FnMut(Event) + 'a {
    let label = move |drv: &str| -> String {
        if let Some(attr) = by_drv.get(drv) {
            return match prov.site(drv).and_then(|s| s.pos.clone()) {
                Some(pos) => format!("{attr} ({pos})"),
                None => attr.clone(),
            };
        }
        prov.name_of(drv)
            .unwrap_or_else(|| strip_hash(drv).to_string())
    };
    move |e: Event| match e {
        Event::Started(s) => {
            reporter
                .timing
                .insert(s.drv_path.clone(), (Instant::now(), None));
            let l = label(&s.drv_path);
            reporter.emit(&FastBuildEvent::new(
                &l,
                &s.drv_path,
                Kind::Started,
                Duration::ZERO,
            ));
        }
        Event::Phase(p) => {
            let l = label(&p.drv_path);
            let d = reporter.duration(&p.drv_path);
            reporter.emit(&FastBuildEvent::new(
                &l,
                &p.drv_path,
                Kind::Phase(p.phase),
                d,
            ));
        }
        Event::Log(l) => {
            let line = prov.rewrite(&l.line);
            let name = label(&l.drv_path);
            let d = reporter.duration(&l.drv_path);
            reporter.emit(&FastBuildEvent::new(&name, &l.drv_path, Kind::Log(line), d));
        }
        Event::Stopped(s) => {
            if let Some((t, slot)) = reporter.timing.get_mut(&s.drv_path)
                && slot.is_none()
            {
                *slot = Some(t.elapsed());
            }
            reporter.render.lock().unwrap().stopped(&s.drv_path);
        }
        Event::Failed(f) => {
            let name = label(&f.drv_path);
            let d = reporter.duration(&f.drv_path);
            let tail = reporter.tail(&f.drv_path);
            reporter.emit(&FastBuildEvent::new(
                &name,
                &f.drv_path,
                Kind::Failed {
                    reason: prov.rewrite_str(&f.reason),
                    log_tail: tail,
                },
                d,
            ));
        }
        Event::Built(_) | Event::Done(_) => {}
    }
}

/// `<hash>-<name>.drv` as just `<name>`, the way a build log names a
/// derivation that no attribute points at.
fn strip_hash(drv_path: &str) -> &str {
    let base = drv_path.rsplit('/').next().unwrap_or(drv_path);
    let base = base.strip_suffix(".drv").unwrap_or(base);
    match base.split_once('-') {
        Some((hash, rest)) if hash.len() == 32 => rest,
        _ => base,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drv(name: &str) -> StorePath {
        StorePath::parse(&format!(
            "/nix/store/2ijv0g6069dsh55z3bdr5ln2iv69mw7r-{name}.drv"
        ))
        .unwrap()
    }

    #[test]
    fn derived_paths() {
        let t = Target::new("a", drv("x"), vec![]);
        assert!(t.derived_path().ends_with("-x.drv!*"));
        let t = Target::new(
            "a",
            drv("x"),
            vec!["dev".into(), "out".into(), "out".into()],
        );
        assert!(
            t.derived_path().ends_with("-x.drv!dev,out"),
            "{}",
            t.derived_path()
        );
    }

    #[test]
    fn hash_stripping() {
        assert_eq!(strip_hash(&drv("hello").to_string()), "hello");
        assert_eq!(strip_hash("plain"), "plain");
    }

    #[test]
    fn outcome_to_record() {
        let o = Outcome {
            attr: "checks.x.a".into(),
            drv: drv("a"),
            success: false,
            skipped: false,
            reason: Some("status 1: boom".into()),
            log_tail: vec!["l1".into(), "l2".into()],
            outputs: BTreeMap::new(),
            elapsed: Duration::from_millis(1500),
            cache_status: Some("notBuilt".into()),
        };
        let r = o.record();
        assert_eq!(r.kind, results::Kind::Build);
        assert_eq!(r.duration, 1.5);
        assert_eq!(r.log_output.as_deref(), Some("l1\nl2"));
        assert!(r.outputs.is_none());
        assert_eq!(r.cache_status.as_deref(), Some("notBuilt"));
    }
}

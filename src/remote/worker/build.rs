//! The client flow over the worker protocol, mirroring `remote::client::build`:
//! `QueryValidPaths` once, `AddMultipleToStore` once, `BuildPathsWithResults` on the root,
//! `QueryDerivationOutputMap`, then `NarFromPath` per fetched output.

use std::collections::{BTreeSet, HashSet};
use std::io::{self, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use crate::store::hash::{HashAlgo, Hasher, hash_bytes};
use crate::store::nar::{dump_string, flat_hash, restore_path};
use crate::store::path::StorePath;

use super::super::plan::{Object, Plan, SourceData, SourceKind};
use super::super::proto::Done;
use super::super::proto::build_event::Event;
use super::wire::{write_str, write_strs, write_u64};
use super::{Connection, Stats, Transport, WorkerError, built_event, failed_event};

#[derive(Debug, Default, Clone)]
pub struct Timings {
    pub connect: Duration,
    pub query: Duration,
    pub add: Duration,
    pub build: Duration,
    pub outputs: Duration,
    pub fetch: Duration,
    pub minor: u64,
    pub trusted: Option<bool>,
    pub query_paths: usize,
    pub missing: usize,
    pub add_paths: usize,
    pub add_bytes: u64,
    pub fetched: usize,
    pub stats: Stats,
}

impl Timings {
    pub fn report(&self, plan: &Plan, plan_time: Duration) -> String {
        let trusted = match self.trusted {
            Some(true) => "trusted",
            Some(false) => "untrusted",
            None => "trust unknown",
        };
        let mut s = format!(
            "plan: {} sources, {} drvs, {:.3}s\nconnect: protocol 1.{}, {trusted}, {:.3}s\nquery: {} paths, {} missing, {:.3}s\nadd: {} paths, {} bytes, {:.3}s\nbuild: {:.3}s\noutputs: {:.3}s",
            plan.sources(),
            plan.drvs(),
            plan_time.as_secs_f64(),
            self.minor,
            self.connect.as_secs_f64(),
            self.query_paths,
            self.missing,
            self.query.as_secs_f64(),
            self.add_paths,
            self.add_bytes,
            self.add.as_secs_f64(),
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
        s.push_str(&format!(
            "\nworker: {} ops, {} round trips, {} bytes sent",
            self.stats.ops, self.stats.round_trips, self.stats.bytes_sent
        ));
        s
    }
}

/// Uploads what the daemon lacks, builds `root`, reports its outputs, and restores the wanted
/// outputs into `fetch_to` when given. With `upload_only` the call stops after the upload.
pub fn build(
    transport: &Transport,
    plan: &Plan,
    root: &StorePath,
    outputs: &[String],
    upload_only: bool,
    fetch_to: Option<&Path>,
    on_event: &mut dyn FnMut(Event),
) -> Result<Timings, WorkerError> {
    let t0 = Instant::now();
    let mut conn = Connection::open(transport, on_event)?;
    let connect = t0.elapsed();
    let mut t = build_on(
        &mut conn,
        plan,
        root,
        outputs,
        upload_only,
        fetch_to,
        on_event,
    )?;
    t.connect = connect;
    Ok(t)
}

/// `build` on an open connection; `Timings::connect` stays zero.
pub fn build_on(
    conn: &mut Connection,
    plan: &Plan,
    root: &StorePath,
    outputs: &[String],
    upload_only: bool,
    fetch_to: Option<&Path>,
    on_event: &mut dyn FnMut(Event),
) -> Result<Timings, WorkerError> {
    let mut t = Timings {
        minor: conn.minor,
        trusted: conn.trusted,
        ..Timings::default()
    };

    let t1 = Instant::now();
    let paths: Vec<String> = plan
        .objects
        .iter()
        .map(|o| o.store_path().to_string())
        .collect();
    t.query_paths = paths.len();
    let valid: HashSet<String> = conn
        .query_valid_paths(&paths, on_event)?
        .into_iter()
        .collect();
    let missing: Vec<&Object> = plan
        .objects
        .iter()
        .filter(|o| !valid.contains(&o.store_path().to_string()))
        .collect();
    t.missing = missing.len();
    t.query = t1.elapsed();

    let t2 = Instant::now();
    if !missing.is_empty() {
        let before = conn.bytes_sent();
        let mut body = |w: &mut dyn Write| -> io::Result<()> {
            write_u64(w, missing.len() as u64)?;
            for o in &missing {
                write_object(w, o)?;
            }
            Ok(())
        };
        conn.add_multiple_to_store(&mut body, on_event)?;
        t.add_paths = missing.len();
        t.add_bytes = conn.bytes_sent() - before;
    }
    t.add = t2.elapsed();
    if upload_only {
        t.stats = conn.stats;
        return Ok(t);
    }

    let t3 = Instant::now();
    let spec = if outputs.is_empty() {
        "*".to_string()
    } else {
        let names: BTreeSet<&str> = outputs.iter().map(String::as_str).collect();
        names.into_iter().collect::<Vec<_>>().join(",")
    };
    let results = conn.build_paths_with_results(&[format!("{root}!{spec}")], on_event)?;
    t.build = t3.elapsed();
    let mut success = true;
    for r in &results {
        if !r.success() {
            success = false;
            on_event(failed_event(
                &root.to_string(),
                &format!("status {}: {}", r.status, r.msg),
            ));
        }
    }
    if !success {
        on_event(Event::Done(Done { success: false }));
        t.stats = conn.stats;
        return Ok(t);
    }

    let t4 = Instant::now();
    let map = conn.query_derivation_output_map(&root.to_string(), on_event)?;
    let mut built = Vec::new();
    for (name, path) in &map {
        if let Some(p) = path {
            on_event(built_event(&root.to_string(), name, p));
            built.push((name.clone(), p.clone()));
        }
    }
    t.outputs = t4.elapsed();

    if let Some(dir) = fetch_to {
        let t5 = Instant::now();
        std::fs::create_dir_all(dir)?;
        for (name, p) in &built {
            if !outputs.is_empty() && !outputs.contains(name) {
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
            conn.nar_from_path(p, on_event, &mut |r| restore_path(r, &dest))?;
            t.fetched += 1;
        }
        t.fetch = t5.elapsed();
    }
    on_event(Event::Done(Done { success: true }));
    if conn.has_pending() {
        return Err(WorkerError(
            "daemon sent more data than the protocol expects".into(),
        ));
    }
    t.stats = conn.stats;
    Ok(t)
}

/// One `ValidPathInfo` (worker-protocol.cc, the 1.16 layout `addMultipleToStore` fixes) and
/// the NAR after it. Registration time 0 and no signatures: the daemon recomputes the content
/// address and accepts the path on that alone.
fn write_object(w: &mut dyn Write, o: &Object) -> io::Result<()> {
    match o {
        Object::Source {
            path,
            data,
            kind,
            nar_hash,
            nar_size,
            ..
        } => {
            let (ca, references): (String, Vec<String>) = match kind {
                SourceKind::Source => (format!("fixed:r:sha256:{}", nar_hash.to_nix32()), vec![]),
                SourceKind::Text { references } => {
                    let flat = match data {
                        SourceData::Memory(contents) => hash_bytes(HashAlgo::Sha256, contents),
                        SourceData::Local { path, .. } => flat_hash(path, HashAlgo::Sha256)?,
                    };
                    (
                        format!("text:sha256:{}", flat.to_nix32()),
                        references.iter().map(|r| r.to_string()).collect(),
                    )
                }
            };
            write_info(
                w,
                &path.to_string(),
                &nar_hash.to_base16(),
                &references,
                *nar_size,
                &ca,
            )?;
            data.dump(w)
        }
        Object::Drv {
            path,
            aterm,
            digest,
            references,
        } => {
            let mut h = Hasher::new(HashAlgo::Sha256);
            dump_string(aterm, &mut h)?;
            let nar_size = h.bytes_written();
            let nar_hash = h.finish();
            let references: Vec<String> = references.iter().map(|r| r.to_string()).collect();
            write_info(
                w,
                &path.to_string(),
                &nar_hash.to_base16(),
                &references,
                nar_size,
                &format!("text:sha256:{}", digest.to_nix32()),
            )?;
            dump_string(aterm, w)
        }
    }
}

fn write_info(
    w: &mut dyn Write,
    path: &str,
    nar_hash_base16: &str,
    references: &[String],
    nar_size: u64,
    ca: &str,
) -> io::Result<()> {
    write_str(w, path.as_bytes())?;
    write_str(w, b"")?;
    write_str(w, nar_hash_base16.as_bytes())?;
    write_strs(w, references)?;
    write_u64(w, 0)?;
    write_u64(w, nar_size)?;
    write_u64(w, 0)?;
    write_strs::<&str>(w, &[])?;
    write_str(w, ca.as_bytes())
}

//! The client flow over the worker protocol:
//! `QueryValidPaths` once, the missing sources located and hashed, `BuildPathsWithResults`
//! on the sources only the daemon can substitute, `AddMultipleToStore` once,
//! `BuildPathsWithResults` on the root, `QueryDerivationOutputMap`, then `NarFromPath` per
//! fetched output.

use std::collections::{BTreeSet, HashSet};
use std::io::{self, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use crate::store::hash::{HashAlgo, hash_bytes};
use crate::store::nar::{dump_string, flat_hash, restore_path};
use crate::store::path::StorePath;

use super::super::event::{Done, Event};
use super::super::plan::{DrvHashes, Locator, Object, Plan, SourceData, SourceKind, prehash_drvs};
use super::wire::{write_str, write_strs, write_u64};
use crate::error::{Error, Result};

use super::{Connection, Stats, Transport, built_event, failed_event};

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
    pub minor: u64,
    pub trusted: Option<bool>,
    pub query_paths: usize,
    pub missing: usize,
    /// Missing sources the client located and hashed.
    pub resolve_paths: usize,
    /// Of those, the ones with no eval-time origin, found through the local store.
    pub store_sources: usize,
    /// Missing sources left to the daemon's substituters.
    pub substitute_paths: usize,
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
            "plan: {} sources, {} drvs, {:.3}s\nconnect: protocol 1.{}, {trusted}, {:.3}s\nquery: {} paths, {} missing, {:.3}s\nresolve: {} sources",
            plan.sources.len(),
            plan.drvs.len(),
            plan_time.as_secs_f64(),
            self.minor,
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
                "\nsubstitute: {} paths left to the daemon, {:.3}s",
                self.substitute_paths,
                self.substitute.as_secs_f64()
            ));
        }
        s.push_str(&format!(
            "\nadd: {} paths, {} bytes, {:.3}s\nbuild: {:.3}s\noutputs: {:.3}s",
            self.add_paths,
            self.add_bytes,
            self.add.as_secs_f64(),
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
        s.push_str(&format!(
            "\nworker: {} ops, {} round trips, {} bytes sent",
            self.stats.ops, self.stats.round_trips, self.stats.bytes_sent
        ));
        s
    }
}

/// Uploads what the daemon lacks, builds `root`, reports its outputs, and restores the wanted
/// outputs into `fetch_to` when given. With `upload_only` the call stops after the upload.
/// `locate` answers for the missing sources that have no eval-time origin.
#[allow(clippy::too_many_arguments)]
pub fn build(
    transport: &Transport,
    plan: &Plan,
    locate: &Locator<'_>,
    root: &StorePath,
    outputs: &[String],
    upload_only: bool,
    fetch_to: Option<&Path>,
    on_event: &mut dyn FnMut(Event),
) -> Result<Timings> {
    let t0 = Instant::now();
    let mut conn = Connection::open(transport, on_event)?;
    let connect = t0.elapsed();
    let mut t = build_on(
        &mut conn,
        plan,
        locate,
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
#[allow(clippy::too_many_arguments)]
pub fn build_on(
    conn: &mut Connection,
    plan: &Plan,
    locate: &Locator<'_>,
    root: &StorePath,
    outputs: &[String],
    upload_only: bool,
    fetch_to: Option<&Path>,
    on_event: &mut dyn FnMut(Event),
) -> Result<Timings> {
    let mut t = Timings {
        minor: conn.minor,
        trusted: conn.trusted,
        ..Timings::default()
    };

    let t1 = Instant::now();
    let paths = plan.paths();
    t.query_paths = paths.len();
    let valid: HashSet<String> = conn
        .query_valid_paths(&paths, on_event)?
        .into_iter()
        .collect();
    let missing_sources: Vec<StorePath> = plan
        .sources
        .iter()
        .filter(|p| !valid.contains(&p.to_string()))
        .cloned()
        .collect();
    let missing_drvs: Vec<&Object> = plan
        .drvs
        .iter()
        .filter(|o| !valid.contains(&o.store_path().to_string()))
        .collect();
    t.missing = missing_sources.len() + missing_drvs.len();
    t.query = t1.elapsed();

    let t2 = Instant::now();
    let resolved = plan.resolve(&missing_sources, locate)?;
    t.resolve_paths = resolved.objects.len();
    t.store_sources = resolved.store_sources;
    t.substitute_paths = resolved.substitute.len();
    t.resolve = t2.elapsed();

    // A path registers only once its references are valid (local-store.cc,
    // registerValidPaths), so the daemon must have these before the drvs that name them.
    let t3 = Instant::now();
    if !resolved.substitute.is_empty() {
        let targets: Vec<String> = resolved.substitute.iter().map(|p| p.to_string()).collect();
        for r in conn.build_paths_with_results(&targets, on_event)? {
            if !r.success() {
                return Err(Error::Store(format!(
                    "cannot substitute '{}' on the builder: status {}: {}",
                    r.path, r.status, r.msg
                )));
            }
        }
    }
    t.substitute = t3.elapsed();

    let t4 = Instant::now();
    let missing: Vec<&Object> = resolved.objects.iter().chain(missing_drvs).collect();
    if !missing.is_empty() {
        prehash_drvs(&missing);
        let before = conn.bytes_sent();
        let mut body = |w: &mut dyn Write| -> io::Result<()> {
            write_u64(w, missing.len() as u64)?;
            for o in &missing {
                write_object(w, o)?;
            }
            Ok(())
        };
        conn.add_multiple_to_store(false, &mut body, on_event)?;
        t.add_paths = missing.len();
        t.add_bytes = conn.bytes_sent() - before;
    }
    t.add = t4.elapsed();
    if upload_only {
        t.stats = conn.stats;
        return Ok(t);
    }

    let t5 = Instant::now();
    let spec = if outputs.is_empty() {
        "*".to_string()
    } else {
        let names: BTreeSet<&str> = outputs.iter().map(String::as_str).collect();
        names.into_iter().collect::<Vec<_>>().join(",")
    };
    let results = conn.build_paths_with_results(&[format!("{root}!{spec}")], on_event)?;
    t.build = t5.elapsed();
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

    let t6 = Instant::now();
    let map = conn.query_derivation_output_map(&root.to_string(), on_event)?;
    let mut built = Vec::new();
    for (name, path) in &map {
        if let Some(p) = path {
            on_event(built_event(&root.to_string(), name, p));
            built.push((name.clone(), p.clone()));
        }
    }
    t.outputs = t6.elapsed();

    if let Some(dir) = fetch_to {
        let t7 = Instant::now();
        std::fs::create_dir_all(dir)
            .map_err(|e| Error::io(format!("cannot create {}", dir.display()), e))?;
        for (name, p) in &built {
            if !outputs.is_empty() && !outputs.contains(name) {
                continue;
            }
            let sp = StorePath::parse(p)?;
            let dest = dir.join(sp.base_name());
            if dest.exists() {
                return Err(Error::Usage(format!(
                    "fetch: '{}' already exists",
                    dest.display()
                )));
            }
            conn.nar_from_path(p, on_event, &mut |r| restore_path(r, &dest))?;
            t.fetched += 1;
        }
        t.fetch = t7.elapsed();
    }
    on_event(Event::Done(Done { success: true }));
    if conn.has_pending() {
        return Err(Error::Protocol(
            "daemon sent more data than the protocol expects".into(),
        ));
    }
    t.stats = conn.stats;
    Ok(t)
}

/// One `ValidPathInfo` (worker-protocol.cc, the 1.16 layout `addMultipleToStore` fixes) and
/// the NAR after it. Registration time 0 and no signatures: the daemon recomputes the content
/// address and accepts the path on that alone.
pub fn write_object(w: &mut dyn Write, o: &Object) -> io::Result<()> {
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
                        SourceData::Nar(_) => {
                            return Err(io::Error::other("a text path cannot come from a NAR"));
                        }
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
            let mut counted = Counting { inner: w, n: 0 };
            data.dump(&mut counted)?;
            if counted.n != *nar_size {
                return Err(io::Error::other(format!(
                    "source '{path}' changed since evaluation: its NAR is {} bytes, not {nar_size}",
                    counted.n
                )));
            }
            Ok(())
        }
        Object::Drv {
            path,
            aterm,
            references,
            hashes,
        } => {
            let h = hashes.get_or_init(|| DrvHashes::of(aterm));
            let references: Vec<String> = references.iter().map(|r| r.to_string()).collect();
            write_info(
                w,
                &path.to_string(),
                &h.nar_hash.to_base16(),
                &references,
                h.nar_size,
                &format!("text:sha256:{}", h.text.to_nix32()),
            )?;
            dump_string(aterm, w)
        }
    }
}

/// Counts what a source's NAR actually was. The framed body declares `nar_size` before the
/// bytes, so a tree that changed after evaluation must fail here rather than desynchronise
/// the stream.
struct Counting<'a> {
    inner: &'a mut dyn Write,
    n: u64,
}

impl Write for Counting<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.n += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
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

//! The instantiation nix-eval-jobs does inside `derivationStrict`: every
//! job's `.drv` and its input closure registered through the daemon. Here
//! it happens in batches on a thread beside the walk: one `QueryValidPaths`
//! and one `AddMultipleToStore` per batch, then `--gc-roots-dir` roots
//! (worker.cc:registerGCRoot) and the batch's lines go on to the sink.

use std::collections::{BTreeSet, HashSet};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Sender, channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::line::{Job, Line};
use crate::builtins::derivation::DrvInfo;
use crate::error::{Error, Result};
use crate::parallel::shared::SharedMap;
use crate::remote::plan::Object;
use crate::remote::worker::build::write_object;
use crate::remote::worker::{Connection, Transport};
use crate::store::drv::Derivation;
use crate::store::path::{STORE_DIR, StorePath};
use crate::store::source::SourceOrigin;

/// Jobs per daemon round trip.
pub const BATCH: usize = 256;

#[derive(Debug, Default, Clone, Copy)]
pub struct Stats {
    pub batches: usize,
    pub queried: usize,
    pub added: usize,
    pub bytes: u64,
    pub roots: usize,
    pub time: Duration,
}

/// The daemon side: one connection, the paths known valid so far, and the
/// evaluation's tables to read ATerms and source trees from.
pub struct Store {
    conn: Connection,
    gc_roots_dir: Option<PathBuf>,
    valid: HashSet<StorePath>,
    drvs: SharedMap<StorePath, DrvInfo>,
    sources: SharedMap<StorePath, SourceOrigin>,
    pub stats: Stats,
}

impl Store {
    pub fn open(
        transport: &Transport,
        gc_roots_dir: Option<PathBuf>,
        drvs: SharedMap<StorePath, DrvInfo>,
        sources: SharedMap<StorePath, SourceOrigin>,
    ) -> Result<Store> {
        let conn = Connection::open(transport, &mut |_| {})?;
        if let Some(dir) = &gc_roots_dir {
            std::fs::create_dir_all(dir)
                .map_err(|e| Error::io(format!("cannot create '{}'", dir.display()), e))?;
        }
        Ok(Store {
            conn,
            gc_roots_dir,
            valid: HashSet::new(),
            drvs,
            sources,
            stats: Stats::default(),
        })
    }

    /// ATerm and references of `p`: this evaluation's, else `/nix/store`'s.
    fn load_drv(&self, p: &StorePath) -> Result<(Arc<Vec<u8>>, BTreeSet<StorePath>)> {
        if let Some(info) = self.drvs.get(p)
            && let Some(aterm) = info.aterm
        {
            return Ok((aterm, info.references));
        }
        let full = format!("{STORE_DIR}/{}", p.base_name());
        let text = std::fs::read_to_string(&full)
            .map_err(|e| Error::io(format!("cannot read derivation '{full}'"), e))?;
        let name = Derivation::name_from_path(p)?;
        let drv = Derivation::parse(&text, name)?;
        let mut references = drv.input_srcs;
        references.extend(drv.input_drvs.into_keys());
        Ok((Arc::new(text.into_bytes()), references))
    }

    /// `p` after the sources a `toFile` text references: the daemon
    /// registers a path only when its references are valid.
    fn collect_source(
        &self,
        p: StorePath,
        seen: &mut HashSet<StorePath>,
        srcs: &mut Vec<StorePath>,
    ) {
        if self.valid.contains(&p) || !seen.insert(p.clone()) {
            return;
        }
        if let Some(SourceOrigin::Text { references, .. }) = self.sources.get(&p) {
            for r in references {
                if !r.is_derivation() {
                    self.collect_source(r, seen, srcs);
                }
            }
        }
        srcs.push(p);
    }

    /// Registers the closures of `jobs`' drvs that the daemon lacks, then
    /// roots each job's `.drv`.
    pub fn instantiate(&mut self, jobs: &[&Job]) -> Result<()> {
        let t0 = Instant::now();
        let mut seen: HashSet<StorePath> = HashSet::new();
        let mut drvs: Vec<(StorePath, Arc<Vec<u8>>, BTreeSet<StorePath>)> = Vec::new();
        let mut srcs: Vec<StorePath> = Vec::new();
        type Pending = Option<(Arc<Vec<u8>>, BTreeSet<StorePath>)>;
        let mut stack: Vec<(StorePath, Pending)> = jobs
            .iter()
            .rev()
            .map(|j| (j.drv_path.clone(), None))
            .collect();
        // Iterative post-order: a drv after every drv it references.
        while let Some((p, done)) = stack.pop() {
            if let Some((aterm, refs)) = done {
                drvs.push((p, aterm, refs));
                continue;
            }
            if self.valid.contains(&p) || !seen.insert(p.clone()) {
                continue;
            }
            let (aterm, refs) = self.load_drv(&p)?;
            stack.push((p, Some((aterm, refs.clone()))));
            for r in refs {
                if self.valid.contains(&r) {
                    continue;
                }
                if r.is_derivation() {
                    if !seen.contains(&r) {
                        stack.push((r, None));
                    }
                } else {
                    self.collect_source(r, &mut seen, &mut srcs);
                }
            }
        }
        let paths: Vec<String> = srcs
            .iter()
            .chain(drvs.iter().map(|d| &d.0))
            .map(|p| p.to_string())
            .collect();
        self.stats.queried += paths.len();
        let valid: HashSet<String> = if paths.is_empty() {
            HashSet::new()
        } else {
            self.conn
                .query_valid_paths(&paths, &mut |_| {})?
                .into_iter()
                .collect()
        };
        let mut objects: Vec<Object> = Vec::new();
        for p in &srcs {
            if valid.contains(&p.to_string()) {
                continue;
            }
            let Some(origin) = self.sources.get(p) else {
                return Err(Error::Store(format!(
                    "source '{p}' is not valid in the store and this evaluation did not create it"
                )));
            };
            let info = crate::remote::plan::source_info(&origin);
            objects.push(
                Object::source(p.clone(), info)
                    .map_err(|e| Error::io(format!("cannot dump '{p}'"), e))?,
            );
        }
        for (path, aterm, references) in drvs.iter() {
            if valid.contains(&path.to_string()) {
                continue;
            }
            objects.push(Object::drv(path.clone(), aterm.clone(), references.clone()));
        }
        if !objects.is_empty() {
            let refs: Vec<&Object> = objects.iter().collect();
            crate::remote::plan::prehash_drvs(&refs);
            let before = self.conn.bytes_sent();
            let mut body = |w: &mut dyn Write| -> std::io::Result<()> {
                crate::remote::worker::wire::write_u64(w, objects.len() as u64)?;
                for o in &objects {
                    write_object(w, o)?;
                }
                Ok(())
            };
            self.conn
                .add_multiple_to_store(false, &mut body, &mut |_| {})?;
            self.stats.added += objects.len();
            self.stats.bytes += self.conn.bytes_sent() - before;
        }
        self.valid.extend(srcs);
        self.valid.extend(drvs.into_iter().map(|d| d.0));
        if let Some(dir) = &self.gc_roots_dir {
            for j in jobs {
                let root = dir.join(j.drv_path.base_name());
                if std::fs::symlink_metadata(&root).is_ok() {
                    continue;
                }
                self.conn.add_perm_root(&j.drv_path.to_string(), &root)?;

                self.stats.roots += 1;
            }
        }
        self.stats.batches += 1;
        self.stats.time += t0.elapsed();
        Ok(())
    }

    pub fn into_connection(self) -> Connection {
        self.conn
    }
}

/// The thread: lines in, instantiated batches out to `sink`, in order.
pub struct Instantiator {
    tx: Option<Sender<Line>>,
    thread: Option<JoinHandle<Result<(Store, Stats)>>>,
}

impl Instantiator {
    pub fn start(
        mut store: Store,
        mut sink: Box<dyn FnMut(Vec<Line>) -> Result<()> + Send>,
    ) -> Instantiator {
        let (tx, rx) = channel::<Line>();
        let thread = std::thread::Builder::new()
            .name("instantiate".into())
            .stack_size(64 << 20)
            .spawn(move || {
                let mut pending: Vec<Line> = Vec::new();
                let mut njobs = 0usize;
                let mut flush = |store: &mut Store, pending: &mut Vec<Line>| -> Result<()> {
                    let jobs: Vec<&Job> = pending
                        .iter()
                        .filter_map(|l| match l {
                            Line::Job(j) => Some(j),
                            Line::Error(_) => None,
                        })
                        .collect();
                    if !jobs.is_empty() {
                        store.instantiate(&jobs)?;
                    }
                    sink(std::mem::take(pending))
                };
                for line in rx {
                    if matches!(line, Line::Job(_)) {
                        njobs += 1;
                    }
                    pending.push(line);
                    if njobs >= BATCH {
                        flush(&mut store, &mut pending)?;
                        njobs = 0;
                    }
                }
                flush(&mut store, &mut pending)?;
                let stats = store.stats;
                Ok((store, stats))
            })
            .expect("spawn instantiate thread");
        Instantiator {
            tx: Some(tx),
            thread: Some(thread),
        }
    }

    pub fn push(&self, line: Line) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(line);
        }
    }

    /// Flushes the last batch and returns the store for later use.
    pub fn finish(mut self) -> Result<(Store, Stats)> {
        drop(self.tx.take());
        self.thread
            .take()
            .expect("instantiate thread")
            .join()
            .map_err(|_| Error::Protocol("instantiate thread panicked".into()))?
    }
}

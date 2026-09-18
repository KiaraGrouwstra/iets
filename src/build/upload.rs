//! Getting what a batch of derivations needs onto the builder, over the
//! connection the builds themselves use.
//!
//! Two shapes. `PlanUpload` ships a `remote::plan::Plan` computed once, for
//! the commands that evaluate to a fixed set of roots before they build
//! anything (`ietsp build`, `nix-build`, `nix build`). `StreamUpload` is
//! incremental, for the eval-jobs walk that hands over jobs while it runs:
//! per batch it walks only the part of the closure it has not registered
//! yet, one `QueryValidPaths` and one `AddMultipleToStore`. The walk
//! duplicates `remote::plan::plan` on purpose: `plan` has no way to prune
//! at a drv already known valid, and re-planning a whole closure per batch
//! is quadratic on a big `checks` set.

use std::collections::{BTreeSet, HashSet};
use std::io::Write;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::builtins::derivation::DrvInfo;
use crate::error::{Error, Result};
use crate::parallel::shared::SharedMap;
use crate::remote::event::Event;
use crate::remote::plan::{Locator, Object, Plan, SourceData, SourceInfo, SourceKind};
use crate::remote::worker::build::write_object;
use crate::remote::worker::{Connection, wire};
use crate::store::drv::Derivation;
use crate::store::path::{STORE_DIR, StorePath};
use crate::store::source::SourceOrigin;

#[derive(Debug, Default, Clone, Copy)]
pub struct Stats {
    pub batches: usize,
    pub queried: usize,
    pub added: usize,
    pub bytes: u64,
    pub substituted: usize,
    pub time: Duration,
}

impl Stats {
    pub fn merge(&mut self, o: Stats) {
        self.batches += o.batches;
        self.queried += o.queried;
        self.added += o.added;
        self.bytes += o.bytes;
        self.substituted += o.substituted;
        self.time += o.time;
    }
}

/// Registers everything `roots` need that the builder lacks.
pub trait Uploader {
    fn upload(
        &mut self,
        conn: &mut Connection,
        roots: &[StorePath],
        on_event: &mut dyn FnMut(Event),
    ) -> Result<Stats>;
}

/// A closure planned once, shipped on the first call.
pub struct PlanUpload<'a> {
    pub plan: &'a Plan,
    pub locate: &'a Locator<'a>,
    /// What the upload half of the five-op flow measured, for `--timings`.
    pub timings: Option<crate::remote::worker::build::Timings>,
    done: bool,
}

impl<'a> PlanUpload<'a> {
    pub fn new(plan: &'a Plan, locate: &'a Locator<'a>) -> PlanUpload<'a> {
        PlanUpload {
            plan,
            locate,
            timings: None,
            done: false,
        }
    }
}

impl Uploader for PlanUpload<'_> {
    fn upload(
        &mut self,
        conn: &mut Connection,
        roots: &[StorePath],
        on_event: &mut dyn FnMut(Event),
    ) -> Result<Stats> {
        if self.done || roots.is_empty() {
            return Ok(Stats::default());
        }
        self.done = true;
        let t0 = Instant::now();
        let t = crate::remote::worker::build::build_on(
            conn,
            self.plan,
            self.locate,
            &roots[0],
            &[],
            true,
            None,
            on_event,
        )?;
        let st = Stats {
            batches: 1,
            queried: t.query_paths,
            added: t.add_paths,
            bytes: t.add_bytes,
            substituted: t.substitute_paths,
            time: t0.elapsed(),
        };
        self.timings = Some(t);
        Ok(st)
    }
}

/// The incremental one: the evaluation's tables plus what the builder is
/// already known to hold.
pub struct StreamUpload {
    drvs: SharedMap<StorePath, DrvInfo>,
    sources: SharedMap<StorePath, SourceOrigin>,
    valid: HashSet<StorePath>,
}

impl StreamUpload {
    pub fn new(
        drvs: SharedMap<StorePath, DrvInfo>,
        sources: SharedMap<StorePath, SourceOrigin>,
    ) -> StreamUpload {
        StreamUpload {
            drvs,
            sources,
            valid: HashSet::new(),
        }
    }

    /// ATerm and references of `p`: this evaluation's, else `/nix/store`'s.
    fn load_drv(&self, p: &StorePath) -> Result<(Arc<Vec<u8>>, BTreeSet<StorePath>)> {
        if let Some(info) = self.drvs.get(p)
            && let Some(aterm) = info.aterm
        {
            return Ok((Arc::new(aterm), info.references));
        }
        let full = format!("{STORE_DIR}/{}", p.base_name());
        let text = std::fs::read_to_string(&full)
            .map_err(|e| Error::io(format!("cannot read derivation '{full}'"), e))?;
        let drv = Derivation::parse(&text, Derivation::name_from_path(p)?)?;
        let mut references = drv.input_srcs;
        references.extend(drv.input_drvs.into_keys());
        Ok((Arc::new(text.into_bytes()), references))
    }

    /// `p` after the sources a `toFile` text references: the daemon
    /// registers a path only once its references are valid.
    fn collect_source(
        &self,
        p: StorePath,
        seen: &mut HashSet<StorePath>,
        out: &mut Vec<StorePath>,
    ) {
        if self.valid.contains(&p) || !seen.insert(p.clone()) {
            return;
        }
        if let Some(SourceOrigin::Text { references, .. }) = self.sources.get(&p) {
            for r in references {
                if !r.is_derivation() {
                    self.collect_source(r, seen, out);
                }
            }
        }
        out.push(p);
    }

    /// Where to read a source and how it was addressed: from the evaluation,
    /// else from the local store, else `None` and the builder substitutes it.
    fn source_info(&self, p: &StorePath) -> Result<Option<SourceInfo>> {
        if let Some(origin) = self.sources.get(p) {
            return Ok(Some(match origin {
                SourceOrigin::Tree { local, accepted } => SourceInfo {
                    data: SourceData::Local {
                        path: local,
                        accepted,
                    },
                    kind: SourceKind::Source,
                },
                SourceOrigin::Text {
                    contents,
                    references,
                } => SourceInfo {
                    data: SourceData::Memory(Arc::new(contents)),
                    kind: SourceKind::Text { references },
                },
            }));
        }
        let found = crate::store::locate::locate_in_store(
            std::slice::from_ref(p),
            &std::collections::HashMap::new(),
        )?;
        Ok(found.into_iter().next().flatten())
    }
}

impl Uploader for StreamUpload {
    fn upload(
        &mut self,
        conn: &mut Connection,
        roots: &[StorePath],
        on_event: &mut dyn FnMut(Event),
    ) -> Result<Stats> {
        let t0 = Instant::now();
        let mut st = Stats {
            batches: 1,
            ..Stats::default()
        };
        let mut seen: HashSet<StorePath> = HashSet::new();
        let mut drvs: Vec<(StorePath, Arc<Vec<u8>>, BTreeSet<StorePath>)> = Vec::new();
        let mut srcs: Vec<StorePath> = Vec::new();
        type Pending = Option<(Arc<Vec<u8>>, BTreeSet<StorePath>)>;
        let mut stack: Vec<(StorePath, Pending)> =
            roots.iter().rev().map(|p| (p.clone(), None)).collect();
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
        if paths.is_empty() {
            st.time = t0.elapsed();
            return Ok(st);
        }
        st.queried = paths.len();
        let valid: HashSet<String> = conn
            .query_valid_paths(&paths, on_event)?
            .into_iter()
            .collect();

        let mut objects: Vec<Object> = Vec::new();
        let mut substitute: Vec<String> = Vec::new();
        for p in &srcs {
            if valid.contains(&p.to_string()) {
                continue;
            }
            match self.source_info(p)? {
                Some(info) => objects.push(
                    Object::source(p.clone(), info)
                        .map_err(|e| Error::io(format!("cannot dump '{p}'"), e))?,
                ),
                None => substitute.push(p.to_string()),
            }
        }
        // A path registers only once its references are valid, so the
        // substituted sources must land before the drvs that name them.
        if !substitute.is_empty() {
            st.substituted = substitute.len();
            for r in conn.build_paths_with_results(&substitute, on_event)? {
                if !r.success() {
                    return Err(Error::Store(format!(
                        "cannot substitute '{}' on the builder: status {}: {}",
                        r.path, r.status, r.msg
                    )));
                }
            }
        }
        for (path, aterm, references) in &drvs {
            if valid.contains(&path.to_string()) {
                continue;
            }
            objects.push(Object::drv(path.clone(), aterm.clone(), references.clone()));
        }
        if !objects.is_empty() {
            let before = conn.bytes_sent();
            let mut body = |w: &mut dyn Write| -> std::io::Result<()> {
                wire::write_u64(w, objects.len() as u64)?;
                for o in &objects {
                    write_object(w, o)?;
                }
                Ok(())
            };
            conn.add_multiple_to_store(false, &mut body, on_event)?;
            st.added = objects.len();
            st.bytes = conn.bytes_sent() - before;
        }
        self.valid.extend(srcs);
        self.valid.extend(drvs.into_iter().map(|d| d.0));
        st.time = t0.elapsed();
        Ok(st)
    }
}

//! Getting what a batch of derivations needs onto the builder, over the
//! backend the builds themselves use.
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
//!
//! Neither shape knows the transport: both speak `build::backend::Backend`,
//! so the same upload runs over the worker protocol and over `grpc://`.

use std::collections::{BTreeSet, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::builtins::derivation::DrvInfo;
use crate::error::{Error, Result};
use crate::parallel::shared::SharedMap;
use crate::remote::event::Event;
use crate::remote::plan::{Locator, Object, Plan, SourceData, SourceInfo, SourceKind};
use crate::store::drv::Derivation;
use crate::store::path::{STORE_DIR, StorePath};
use crate::store::source::SourceOrigin;

use super::backend::{Backend, Drv, Phase};

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
        b: &mut dyn Backend,
        roots: &[StorePath],
        on_event: &mut dyn FnMut(Event),
    ) -> Result<Stats>;

    /// The derivations `roots` name, and with `deep` everything they need as
    /// well, references before referrers: what a backend that reports outputs
    /// itself, or schedules the builds itself, drives. The worker backend never
    /// asks, so neither walk costs anything there.
    fn drvs(&self, roots: &[StorePath], deep: bool) -> Result<Vec<Drv>>;
}

/// A closure planned once, shipped on the first call.
pub struct PlanUpload<'a> {
    pub plan: &'a Plan,
    pub locate: &'a Locator<'a>,
    done: bool,
}

impl<'a> PlanUpload<'a> {
    pub fn new(plan: &'a Plan, locate: &'a Locator<'a>) -> PlanUpload<'a> {
        PlanUpload {
            plan,
            locate,
            done: false,
        }
    }
}

impl Uploader for PlanUpload<'_> {
    fn upload(
        &mut self,
        b: &mut dyn Backend,
        roots: &[StorePath],
        on_event: &mut dyn FnMut(Event),
    ) -> Result<Stats> {
        if self.done || roots.is_empty() {
            return Ok(Stats::default());
        }
        self.done = true;
        let t0 = Instant::now();
        let plan = self.plan;
        let paths: Vec<&StorePath> = plan
            .sources
            .iter()
            .chain(plan.drvs.iter().map(Object::store_path))
            .collect();
        let queried = paths.len();
        let valid = b.query_valid(&paths, on_event)?;
        let missing_sources: Vec<StorePath> = plan
            .sources
            .iter()
            .filter(|p| !valid.contains(p.base_name()))
            .cloned()
            .collect();
        let missing_drvs: Vec<&Object> = plan
            .drvs
            .iter()
            .filter(|o| !valid.contains(o.store_path().base_name()))
            .collect();

        let t1 = Instant::now();
        let resolved = plan.resolve(&missing_sources, self.locate)?;
        b.note_upload(Phase {
            missing: missing_sources.len() + missing_drvs.len(),
            resolve: t1.elapsed(),
            resolve_paths: resolved.objects.len(),
            store_sources: resolved.store_sources,
        });

        b.substitute(&resolved.substitute, on_event)?;

        let missing: Vec<&Object> = resolved.objects.iter().chain(missing_drvs).collect();
        let bytes = b.add_objects(&missing, on_event)?;
        Ok(Stats {
            batches: 1,
            queried,
            added: missing.len(),
            bytes,
            substituted: resolved.substitute.len(),
            time: t0.elapsed(),
        })
    }

    /// The plan is the closure of every root, in post-order already.
    fn drvs(&self, roots: &[StorePath], deep: bool) -> Result<Vec<Drv>> {
        let wanted: HashSet<&StorePath> = roots.iter().collect();
        Ok(self
            .plan
            .drvs
            .iter()
            .filter_map(|o| match o {
                Object::Drv { path, aterm, .. } if deep || wanted.contains(path) => Some(Drv {
                    path: path.clone(),
                    aterm: aterm.clone(),
                }),
                _ => None,
            })
            .collect())
    }
}

/// One derivation of a walk: its ATerm and what it references.
type Node = (StorePath, Arc<Vec<u8>>, BTreeSet<StorePath>);

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

    /// The drvs `roots` need in post-order and the sources they name, pruned
    /// at everything in `known`.
    fn walk(
        &self,
        roots: &[StorePath],
        known: &HashSet<StorePath>,
    ) -> Result<(Vec<Node>, Vec<StorePath>)> {
        let mut seen: HashSet<StorePath> = HashSet::new();
        let mut drvs: Vec<Node> = Vec::new();
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
            if known.contains(&p) || !seen.insert(p.clone()) {
                continue;
            }
            let (aterm, refs) = self.load_drv(&p)?;
            stack.push((p, Some((aterm, refs.clone()))));
            for r in refs {
                if known.contains(&r) {
                    continue;
                }
                if r.is_derivation() {
                    if !seen.contains(&r) {
                        stack.push((r, None));
                    }
                } else {
                    self.collect_source(r, known, &mut seen, &mut srcs);
                }
            }
        }
        Ok((drvs, srcs))
    }

    /// `p` after the sources a `toFile` text references: the daemon
    /// registers a path only once its references are valid.
    fn collect_source(
        &self,
        p: StorePath,
        known: &HashSet<StorePath>,
        seen: &mut HashSet<StorePath>,
        out: &mut Vec<StorePath>,
    ) {
        if known.contains(&p) || !seen.insert(p.clone()) {
            return;
        }
        if let Some(SourceOrigin::Text { references, .. }) = self.sources.get(&p) {
            for r in references {
                if !r.is_derivation() {
                    self.collect_source(r, known, seen, out);
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
        b: &mut dyn Backend,
        roots: &[StorePath],
        on_event: &mut dyn FnMut(Event),
    ) -> Result<Stats> {
        let t0 = Instant::now();
        let mut st = Stats {
            batches: 1,
            ..Stats::default()
        };
        let (drvs, srcs) = self.walk(roots, &self.valid)?;
        let paths: Vec<&StorePath> = srcs.iter().chain(drvs.iter().map(|d| &d.0)).collect();
        if paths.is_empty() {
            st.time = t0.elapsed();
            return Ok(st);
        }
        st.queried = paths.len();
        let valid = b.query_valid(&paths, on_event)?;

        let t1 = Instant::now();
        let mut objects: Vec<Object> = Vec::new();
        let mut substitute: Vec<StorePath> = Vec::new();
        for p in &srcs {
            if valid.contains(p.base_name()) {
                continue;
            }
            match self.source_info(p)? {
                Some(info) => objects.push(
                    Object::source(p.clone(), info)
                        .map_err(|e| Error::io(format!("cannot dump '{p}'"), e))?,
                ),
                None => substitute.push(p.clone()),
            }
        }
        let missing_drvs = drvs
            .iter()
            .filter(|(p, _, _)| !valid.contains(p.base_name()))
            .count();
        b.note_upload(Phase {
            missing: objects.len() + substitute.len() + missing_drvs,
            resolve: t1.elapsed(),
            resolve_paths: objects.len(),
            // A streamed batch never prints `--timings`.
            store_sources: 0,
        });

        // A path registers only once its references are valid, so the
        // substituted sources must land before the drvs that name them.
        st.substituted = substitute.len();
        b.substitute(&substitute, on_event)?;

        for (path, aterm, references) in &drvs {
            if valid.contains(path.base_name()) {
                continue;
            }
            objects.push(Object::drv(path.clone(), aterm.clone(), references.clone()));
        }
        let refs: Vec<&Object> = objects.iter().collect();
        st.bytes = b.add_objects(&refs, on_event)?;
        st.added = objects.len();
        self.valid.extend(srcs);
        self.valid.extend(drvs.into_iter().map(|d| d.0));
        st.time = t0.elapsed();
        Ok(st)
    }

    fn drvs(&self, roots: &[StorePath], deep: bool) -> Result<Vec<Drv>> {
        if !deep {
            return roots
                .iter()
                .map(|p| {
                    self.load_drv(p).map(|(aterm, _)| Drv {
                        path: p.clone(),
                        aterm,
                    })
                })
                .collect();
        }
        // No pruning: a derivation whose `.drv` the store already has still
        // needs building when its outputs are missing.
        let (drvs, _) = self.walk(roots, &HashSet::new())?;
        Ok(drvs
            .into_iter()
            .map(|(path, aterm, _)| Drv { path, aterm })
            .collect())
    }
}

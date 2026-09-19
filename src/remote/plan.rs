//! The upload plan: the closure of the root derivations, and the sources the daemon lacks
//! hashed the way Nix addresses them.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use crate::builtins::derivation::DrvInfo;
use crate::error::{Error, Result};
use crate::store::drv::Derivation;
use crate::store::hash::{Hash, HashAlgo, Hasher};
use crate::store::nar::{dump_path, dump_string};
use crate::store::path::{STORE_DIR, StorePath};
use crate::store::source::SourceOrigin;

/// How an input source was content-addressed (store-api.hh:ContentAddress, the two kinds
/// eval produces).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceKind {
    /// Recursive sha256 of the NAR, no references: `./path`, `fetchTarball`.
    Source,
    /// Flat sha256 of one regular file plus references: `builtins.toFile`.
    Text { references: BTreeSet<StorePath> },
}

/// Where the bytes of a source come from when it is hashed and uploaded.
#[derive(Clone, Debug)]
pub enum SourceData {
    /// A local tree; `accepted` is the filter result recorded at eval time.
    Local {
        path: PathBuf,
        accepted: Option<Arc<HashSet<PathBuf>>>,
    },
    /// One regular file whose contents are in memory (`builtins.toFile`).
    Memory(Arc<Vec<u8>>),
    /// A NAR nothing holds: written on demand, once to hash it and once to send
    /// it (`flake::spool`).
    Nar(Arc<dyn NarSource>),
}

/// A NAR its owner can write as many times as asked, without a tree on disk.
pub trait NarSource: std::fmt::Debug + Send + Sync {
    fn dump(&self, sink: &mut dyn io::Write) -> io::Result<()>;
}

impl SourceData {
    /// Writes the NAR of this source into `sink`.
    pub fn dump(&self, sink: &mut dyn io::Write) -> io::Result<()> {
        match self {
            SourceData::Local { path, accepted } => match accepted {
                Some(a) => dump_path(path, sink, &mut |p, _| a.contains(p)),
                None => dump_path(path, sink, &mut |_, _| true),
            },
            SourceData::Memory(contents) => dump_string(contents, sink),
            SourceData::Nar(nar) => nar.dump(sink),
        }
    }
}

/// Where to read an input source and how it was addressed.
#[derive(Clone, Debug)]
pub struct SourceInfo {
    pub data: SourceData,
    pub kind: SourceKind,
    /// The NAR hash and size evaluation already computed under this source's exact filter.
    /// `None` for a source the evaluation did not record, which the plan must hash itself.
    pub hashed: Option<(Hash, u64)>,
}

impl SourceInfo {
    /// A plain, unfiltered local tree, not hashed yet.
    pub fn tree(local: impl Into<PathBuf>) -> SourceInfo {
        SourceInfo {
            data: SourceData::Local {
                path: local.into(),
                accepted: None,
            },
            kind: SourceKind::Source,
            hashed: None,
        }
    }
}

/// One object the daemon must hold before it can build the root.
#[derive(Clone, Debug)]
pub enum Object {
    /// A source tree plus the NAR hash Nix addresses it by.
    Source {
        path: StorePath,
        data: SourceData,
        kind: SourceKind,
        nar_hash: Hash,
        nar_size: u64,
    },
    /// A derivation: its ATerm. `references` is inputSrcs plus inputDrvs, what the worker
    /// protocol needs to register the `.drv` as a text path. `hashes` is filled by
    /// `prehash_drvs` before the upload starts, so the connection only writes bytes.
    Drv {
        path: StorePath,
        aterm: Arc<Vec<u8>>,
        references: BTreeSet<StorePath>,
        hashes: OnceLock<DrvHashes>,
    },
}

/// What registering a `.drv` needs beyond its bytes: the flat sha256 of the ATerm, which is
/// its `text:sha256:` content address, and the hash and size of its NAR.
#[derive(Clone, Debug)]
pub struct DrvHashes {
    pub text: Hash,
    pub nar_hash: Hash,
    pub nar_size: u64,
}

impl DrvHashes {
    pub fn of(aterm: &[u8]) -> DrvHashes {
        let mut nar = Hasher::new(HashAlgo::Sha256);
        dump_string(aterm, &mut nar).expect("a hasher never fails");
        DrvHashes {
            text: crate::store::hash::hash_bytes(HashAlgo::Sha256, aterm),
            nar_size: nar.bytes_written(),
            nar_hash: nar.finish(),
        }
    }
}

/// Hashes the ATerm of every `.drv` in `objects` on a thread pool. Without this each ATerm is
/// hashed twice inside the framed upload, one drv at a time, with the daemon waiting.
pub fn prehash_drvs(objects: &[&Object]) {
    let todo: Vec<(&OnceLock<DrvHashes>, &[u8])> = objects
        .iter()
        .filter_map(|o| match o {
            Object::Drv { aterm, hashes, .. } if hashes.get().is_none() => {
                Some((hashes, aterm.as_slice()))
            }
            _ => None,
        })
        .collect();
    if todo.len() < 2 {
        for (cell, aterm) in todo {
            let _ = cell.set(DrvHashes::of(aterm));
        }
        return;
    }
    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .min(todo.len());
    let next = AtomicUsize::new(0);
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= todo.len() {
                        break;
                    }
                    let (cell, aterm) = todo[i];
                    let _ = cell.set(DrvHashes::of(aterm));
                }
            });
        }
    });
}

impl Object {
    /// A source object, hashing the tree only when evaluation did not already report the
    /// hash under the same filter.
    pub fn source(path: StorePath, info: SourceInfo) -> io::Result<Object> {
        let (nar_hash, nar_size) = match info.hashed {
            Some(h) => h,
            None => hash_source(&info.data)?,
        };
        Ok(Object::Source {
            path,
            data: info.data,
            kind: info.kind,
            nar_hash,
            nar_size,
        })
    }

    pub fn drv(path: StorePath, aterm: Arc<Vec<u8>>, references: BTreeSet<StorePath>) -> Object {
        Object::Drv {
            path,
            aterm,
            references,
            hashes: OnceLock::new(),
        }
    }

    pub fn store_path(&self) -> &StorePath {
        match self {
            Object::Source { path, .. } | Object::Drv { path, .. } => path,
        }
    }
}

/// The closure of the roots: what the daemon must hold before it can build them. Sources are
/// only named here; `resolve` locates and hashes the ones the daemon lacks.
#[derive(Debug, Default)]
pub struct Plan {
    /// Input sources in registration order: a `toFile` text after the sources it references.
    pub sources: Vec<StorePath>,
    /// The eval-time origin of every source in `sources` that this evaluation added.
    pub origins: HashMap<StorePath, SourceOrigin>,
    /// Derivations in post-order: each after its input drvs.
    pub drvs: Vec<Object>,
    /// Input drvs that were not created by this evaluation and were read from `/nix/store`.
    pub store_drvs: usize,
}

/// The missing sources after `Plan::resolve`: what the client uploads and what it leaves to
/// the daemon.
#[derive(Debug, Default)]
pub struct Resolved {
    /// Hashed sources, in the order they were asked for.
    pub objects: Vec<Object>,
    /// Sources the client cannot produce: not in the local store, or not content-addressed
    /// the way v1 sends (an input-addressed build output pinned with `builtins.storePath`).
    /// The daemon substitutes them.
    pub substitute: Vec<StorePath>,
    /// Sources with no eval-time origin, read from the local store and hashed.
    pub store_sources: usize,
}

impl Plan {
    /// Every path in the closure, sources then drvs, for `QueryValidPaths`.
    pub fn paths(&self) -> Vec<String> {
        self.sources
            .iter()
            .map(ToString::to_string)
            .chain(self.drvs.iter().map(|d| d.store_path().to_string()))
            .collect()
    }

    /// Locates and hashes `missing`, a subset of `sources` in plan order: eval-time origins
    /// first, the rest through `locate` in one batch, hashed on a thread pool.
    pub fn resolve(&self, missing: &[StorePath], locate: &Locator<'_>) -> Result<Resolved> {
        let mut infos: Vec<Option<SourceInfo>> = Vec::with_capacity(missing.len());
        let mut unknown: Vec<StorePath> = Vec::new();
        for p in missing {
            let info = self.origins.get(p).map(source_info);
            if info.is_none() {
                unknown.push(p.clone());
            }
            infos.push(info);
        }
        let mut substitute = Vec::new();
        if !unknown.is_empty() {
            let found = locate(&unknown)?;
            if found.len() != unknown.len() {
                return Err(Error::Plan(
                    "locator returned the wrong number of sources".into(),
                ));
            }
            let mut found = found.into_iter();
            for (p, slot) in missing.iter().zip(infos.iter_mut()) {
                if slot.is_some() {
                    continue;
                }
                match found.next().expect("one answer per unknown source") {
                    Some(info) => *slot = Some(info),
                    None => substitute.push(p.clone()),
                }
            }
        }
        let (paths, infos): (Vec<&StorePath>, Vec<SourceInfo>) = missing
            .iter()
            .zip(infos)
            .filter_map(|(p, i)| i.map(|i| (p, i)))
            .unzip();
        let store_sources = unknown.len() - substitute.len();
        Ok(Resolved {
            objects: hash_sources(&paths, &infos)?,
            substitute,
            store_sources,
        })
    }
}

/// What to upload for one recorded source. A tree carries the NAR hash and size the
/// evaluation computed under the very filter it recorded, so nothing walks the tree again.
pub fn source_info(o: &SourceOrigin) -> SourceInfo {
    match o {
        SourceOrigin::Tree {
            local,
            accepted,
            nar_hash,
            nar_size,
        } => SourceInfo {
            data: SourceData::Local {
                path: local.clone(),
                accepted: accepted.clone(),
            },
            kind: SourceKind::Source,
            hashed: Some((*nar_hash, *nar_size)),
        },
        SourceOrigin::Text {
            contents,
            references,
        } => SourceInfo {
            data: SourceData::Memory(Arc::new(contents.clone())),
            kind: SourceKind::Text {
                references: references.clone(),
            },
            hashed: None,
        },
        SourceOrigin::Input {
            entry,
            rel,
            nar_hash,
            nar_size,
        } => SourceInfo {
            data: SourceData::Nar(crate::inputs::nar_source(entry, rel)),
            kind: SourceKind::Source,
            hashed: Some((*nar_hash, *nar_size)),
        },
    }
}

/// Checks that a planned source still hashes to its store path: the NAR of the original tree
/// must yield the path eval computed, or the daemon rejects it. Only sources the plan hashed
/// itself need this. A tree whose hash comes from evaluation cannot fail it by construction;
/// if such a tree changed on disk after evaluation, the daemon catches it, because
/// `AddMultipleToStore` recomputes the NAR hash of a `fixed:r` path and rejects a mismatch,
/// and `write_object` refuses to send a NAR whose length is not the recorded `nar_size`.
fn check_source_path(path: &StorePath, kind: &SourceKind, nar_hash: &Hash) -> Result<()> {
    use crate::store::path::{IngestionMethod, make_fixed_output_path};
    if let SourceKind::Source = kind {
        let expected = make_fixed_output_path(
            IngestionMethod::Recursive,
            nar_hash,
            path.name(),
            &BTreeSet::new(),
        )?;
        if &expected != path {
            return Err(Error::Plan(format!(
                "source '{path}' changed since evaluation (now {expected})"
            )));
        }
    }
    Ok(())
}

/// Maps input source store paths that eval did not record, in one batch, to where to read
/// them and how they were addressed; `None` when the local side cannot produce the path and
/// the daemon must substitute it. `ietsp build` asks the local store.
pub type Locator<'a> = dyn Fn(&[StorePath]) -> Result<Vec<Option<SourceInfo>>> + 'a;

/// What the evaluation knows: the drv table and the origin of every added source. Both may
/// be empty, in which case everything is read from `/nix/store`.
#[derive(Clone, Copy)]
pub struct Inputs<'a> {
    pub drvs: &'a HashMap<StorePath, DrvInfo>,
    pub sources: &'a HashMap<StorePath, SourceOrigin>,
}

/// The NAR hash and size of a source, one pass over the bytes.
pub fn hash_source(data: &SourceData) -> io::Result<(Hash, u64)> {
    let mut nar = Hasher::new(HashAlgo::Sha256);
    data.dump(&mut nar)?;
    let size = nar.bytes_written();
    Ok((nar.finish(), size))
}

/// A drv's ATerm and references, from the eval table or from `/nix/store`.
struct DrvNode {
    references: BTreeSet<StorePath>,
    aterm: Arc<Vec<u8>>,
    from_store: bool,
}

fn load_drv(path: &StorePath, inputs: &Inputs<'_>) -> Result<DrvNode> {
    if let Some(info) = inputs.drvs.get(path)
        && let Some(aterm) = &info.aterm
    {
        return Ok(DrvNode {
            references: info.references.clone(),
            aterm: aterm.clone(),
            from_store: false,
        });
    }
    let full = format!("{STORE_DIR}/{}", path.base_name());
    let text = std::fs::read_to_string(&full)
        .map_err(|e| Error::io(format!("cannot read derivation '{full}'"), e))?;
    let drv = Derivation::parse(&text, Derivation::name_from_path(path)?)?;
    let mut references = drv.input_srcs;
    references.extend(drv.input_drvs.into_keys());
    Ok(DrvNode {
        references,
        aterm: Arc::new(text.into_bytes()),
        from_store: true,
    })
}

/// Computes the closure of `roots`: input drvs recursively and every input source, without
/// reading or hashing a source. Every drv comes from `inputs`; what is not there is read from
/// `/nix/store` and counted in the plan. Fixed-output drvs are ordinary drvs here; the builder
/// fetches them.
pub fn plan(roots: &[StorePath], inputs: Inputs<'_>) -> Result<Plan> {
    let mut sources: Vec<StorePath> = Vec::new();
    let mut drvs: Vec<Object> = Vec::new();
    let mut seen_src: HashSet<StorePath> = HashSet::new();
    let mut seen_drv: HashSet<StorePath> = HashSet::new();
    let mut store_drvs = 0;

    // Iterative post-order: a drv is emitted after every drv it references.
    type Pending = Option<(Arc<Vec<u8>>, BTreeSet<StorePath>)>;
    let mut stack: Vec<(StorePath, Pending)> =
        roots.iter().rev().map(|r| (r.clone(), None)).collect();
    while let Some((path, done)) = stack.pop() {
        if let Some((aterm, references)) = done {
            drvs.push(Object::drv(path, aterm, references));
            continue;
        }
        if !seen_drv.insert(path.clone()) {
            continue;
        }
        let node = load_drv(&path, &inputs)?;
        store_drvs += usize::from(node.from_store);
        stack.push((path, Some((node.aterm, node.references.clone()))));
        for r in node.references {
            if r.is_derivation() {
                if !seen_drv.contains(&r) {
                    stack.push((r, None));
                }
            } else {
                collect_source(r, inputs.sources, &mut seen_src, &mut sources);
            }
        }
    }

    let origins = sources
        .iter()
        .filter_map(|p| inputs.sources.get(p).map(|o| (p.clone(), o.clone())))
        .collect();
    Ok(Plan {
        sources,
        origins,
        drvs,
        store_drvs,
    })
}

/// Adds `p` to `sources` after the sources it references: a `toFile`
/// text can point at a copied path that no derivation lists directly,
/// and the daemon registers a path only once its references are valid.
fn collect_source(
    p: StorePath,
    origins: &HashMap<StorePath, SourceOrigin>,
    seen: &mut HashSet<StorePath>,
    sources: &mut Vec<StorePath>,
) {
    if !seen.insert(p.clone()) {
        return;
    }
    if let Some(SourceOrigin::Text { references, .. }) = origins.get(&p) {
        for r in references {
            if !r.is_derivation() {
                collect_source(r.clone(), origins, seen, sources);
            }
        }
    }
    sources.push(p);
}

/// Dumps and hashes every source on a thread pool; results keep the input order.
fn hash_sources(sources: &[&StorePath], infos: &[SourceInfo]) -> Result<Vec<Object>> {
    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .min(sources.len().max(1));
    let next = Mutex::new(0usize);
    let results: Vec<Mutex<Option<Result<Object>>>> =
        sources.iter().map(|_| Mutex::new(None)).collect();
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                loop {
                    let i = {
                        let mut n = next.lock().unwrap();
                        let i = *n;
                        *n += 1;
                        i
                    };
                    if i >= sources.len() {
                        break;
                    }
                    let path = sources[i];
                    let info = &infos[i];
                    let from_eval = info.hashed.is_some();
                    let r = Object::source(path.clone(), info.clone())
                        .map_err(|e| Error::io(format!("cannot dump '{path}'"), e))
                        .and_then(|o| {
                            if let Object::Source { nar_hash, kind, .. } = &o
                                && !from_eval
                            {
                                check_source_path(path, kind, nar_hash)?;
                            }
                            Ok(o)
                        });
                    *results[i].lock().unwrap() = Some(r);
                }
            });
        }
    });
    results
        .into_iter()
        .map(|m| m.into_inner().unwrap().expect("every source hashed"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::nar::{nar_hash, nar_size};

    #[test]
    fn hash_source_matches_nar_hash() {
        let tree = crate::store::nar::tests::fixture_tree("plan");
        let data = SourceData::Local {
            path: tree.clone(),
            accepted: None,
        };
        let (h, size) = hash_source(&data).unwrap();
        assert_eq!(h, nar_hash(&tree).unwrap());
        assert_eq!(size, nar_size(&tree).unwrap());
    }

    #[test]
    fn memory_source_matches_dump_string() {
        let data = SourceData::Memory(Arc::new(b"hello\n".to_vec()));
        let (h, size) = hash_source(&data).unwrap();
        assert_eq!(h, crate::store::nar::nar_hash_of_string(b"hello\n"));
        assert!(size > 0);
    }
}

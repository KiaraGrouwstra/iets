//! The upload plan: the closure of the root derivations as content-addressed objects.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::builtins::derivation::DrvInfo;
use crate::store::drv::{Derivation, DrvError};
use crate::store::hash::{Hash, HashAlgo, Hasher};
use crate::store::nar::{dump_path, dump_string};
use crate::store::path::{STORE_DIR, StorePath};
use crate::store::source::SourceOrigin;

use super::chunk::{Chunker, Tee, sha256};

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
        }
    }

    pub fn describe(&self) -> String {
        match self {
            SourceData::Local { path, .. } => path.display().to_string(),
            SourceData::Memory(_) => "<in-memory text>".into(),
        }
    }
}

/// Where to read an input source and how it was addressed.
#[derive(Clone, Debug)]
pub struct SourceInfo {
    pub data: SourceData,
    pub kind: SourceKind,
}

impl SourceInfo {
    /// A plain, unfiltered local tree.
    pub fn tree(local: impl Into<PathBuf>) -> SourceInfo {
        SourceInfo {
            data: SourceData::Local {
                path: local.into(),
                accepted: None,
            },
            kind: SourceKind::Source,
        }
    }
}

/// One object the builder must hold before it can build the root.
#[derive(Clone, Debug)]
pub enum Object {
    /// A source tree, sent as chunks plus a manifest.
    Source {
        path: StorePath,
        data: SourceData,
        kind: SourceKind,
        nar_hash: Hash,
        nar_size: u64,
        chunks: Vec<Hash>,
    },
    /// A derivation, sent as its ATerm bytes.
    Drv {
        path: StorePath,
        aterm: Arc<Vec<u8>>,
        digest: Hash,
    },
}

impl Object {
    /// The digest the builder indexes this object by.
    pub fn digest(&self) -> &Hash {
        match self {
            Object::Source { nar_hash, .. } => nar_hash,
            Object::Drv { digest, .. } => digest,
        }
    }

    pub fn store_path(&self) -> &StorePath {
        match self {
            Object::Source { path, .. } | Object::Drv { path, .. } => path,
        }
    }
}

/// Objects in topological order: every source first, then each drv after its input drvs.
#[derive(Debug, Default)]
pub struct Plan {
    pub objects: Vec<Object>,
    /// Input drvs that were not created by this evaluation and were read from `/nix/store`.
    pub store_drvs: usize,
    /// Input sources with no eval-time origin, located through the local store.
    pub store_sources: usize,
}

impl Plan {
    pub fn sources(&self) -> usize {
        self.objects
            .iter()
            .filter(|o| matches!(o, Object::Source { .. }))
            .count()
    }

    pub fn drvs(&self) -> usize {
        self.objects.len() - self.sources()
    }
}

#[derive(Debug)]
pub struct PlanError(pub String);

impl std::fmt::Display for PlanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for PlanError {}

impl From<DrvError> for PlanError {
    fn from(e: DrvError) -> Self {
        PlanError(e.0)
    }
}

impl From<io::Error> for PlanError {
    fn from(e: io::Error) -> Self {
        PlanError(e.to_string())
    }
}

impl From<crate::store::path::StorePathError> for PlanError {
    fn from(e: crate::store::path::StorePathError) -> Self {
        PlanError(e.0)
    }
}

/// Checks that a planned source still hashes to its store path: the NAR of the original tree
/// must yield the path eval computed, or the builder rejects the manifest.
fn check_source_path(
    path: &StorePath,
    kind: &SourceKind,
    nar_hash: &Hash,
) -> Result<(), PlanError> {
    use crate::store::path::{IngestionMethod, make_fixed_output_path};
    if let SourceKind::Source = kind {
        let expected = make_fixed_output_path(
            IngestionMethod::Recursive,
            nar_hash,
            path.name(),
            &BTreeSet::new(),
        )?;
        if &expected != path {
            return Err(PlanError(format!(
                "source '{path}' changed since evaluation (now {expected})"
            )));
        }
    }
    Ok(())
}

/// Maps input source store paths that eval did not record, in one batch, to where to read
/// them and how they were addressed. `ietsp build` asks the local store.
pub type Locator<'a> = dyn Fn(&[StorePath]) -> Result<Vec<SourceInfo>, String> + 'a;

/// What the evaluation knows: the drv table and the origin of every added source. Both may
/// be empty, in which case everything is read from `/nix/store`.
#[derive(Clone, Copy)]
pub struct Inputs<'a> {
    pub drvs: &'a HashMap<StorePath, DrvInfo>,
    pub sources: &'a HashMap<StorePath, SourceOrigin>,
}

/// Hashes a source: NAR hash and size, and the sha256 of each chunk.
pub fn hash_source(data: &SourceData) -> io::Result<(Hash, u64, Vec<Hash>)> {
    let mut chunks = Vec::new();
    let mut chunker = Chunker::new(|c: Vec<u8>| {
        chunks.push(sha256(&c));
        Ok(())
    });
    let mut nar = Hasher::new(HashAlgo::Sha256);
    data.dump(&mut Tee(&mut nar, &mut chunker))?;
    chunker.finish()?;
    let size = nar.bytes_written();
    Ok((nar.finish(), size, chunks))
}

/// A drv's ATerm and references, from the eval table or from `/nix/store`.
struct DrvNode {
    references: BTreeSet<StorePath>,
    aterm: Arc<Vec<u8>>,
    from_store: bool,
}

fn load_drv(path: &StorePath, inputs: &Inputs<'_>) -> Result<DrvNode, PlanError> {
    if let Some(info) = inputs.drvs.get(path)
        && let Some(aterm) = &info.aterm
    {
        return Ok(DrvNode {
            references: info.references.clone(),
            aterm: Arc::new(aterm.clone()),
            from_store: false,
        });
    }
    let full = format!("{STORE_DIR}/{}", path.base_name());
    let text = std::fs::read_to_string(&full)
        .map_err(|e| PlanError(format!("cannot read derivation '{full}': {e}")))?;
    let drv = Derivation::parse(&text, Derivation::name_from_path(path)?)?;
    let mut references = drv.input_srcs;
    references.extend(drv.input_drvs.into_keys());
    Ok(DrvNode {
        references,
        aterm: Arc::new(text.into_bytes()),
        from_store: true,
    })
}

/// Computes the closure of `roots`: input drvs recursively and every input source, hashed in
/// parallel. Every drv and source comes from `inputs`; what is not there is read from
/// `/nix/store` (drvs) or asked from `locate` (sources) and counted in the plan. Fixed-output
/// drvs are ordinary drvs here; the builder fetches them.
pub fn plan(
    roots: &[StorePath],
    inputs: Inputs<'_>,
    locate: &Locator<'_>,
) -> Result<Plan, PlanError> {
    let mut sources: Vec<StorePath> = Vec::new();
    let mut drvs: Vec<(StorePath, Arc<Vec<u8>>)> = Vec::new();
    let mut seen_src: HashSet<StorePath> = HashSet::new();
    let mut seen_drv: HashSet<StorePath> = HashSet::new();
    let mut store_drvs = 0;

    // Iterative post-order: a drv is emitted after every drv it references.
    let mut stack: Vec<(StorePath, Option<Arc<Vec<u8>>>)> =
        roots.iter().rev().map(|r| (r.clone(), None)).collect();
    while let Some((path, done)) = stack.pop() {
        if let Some(aterm) = done {
            drvs.push((path, aterm));
            continue;
        }
        if !seen_drv.insert(path.clone()) {
            continue;
        }
        let node = load_drv(&path, &inputs)?;
        store_drvs += usize::from(node.from_store);
        stack.push((path, Some(node.aterm)));
        for r in node.references {
            if r.is_derivation() {
                if !seen_drv.contains(&r) {
                    stack.push((r, None));
                }
            } else if seen_src.insert(r.clone()) {
                sources.push(r);
            }
        }
    }

    let (infos, store_sources) = locate_sources(&sources, inputs.sources, locate)?;
    let hashed = hash_sources(&sources, &infos)?;
    let mut objects = Vec::with_capacity(hashed.len() + drvs.len());
    objects.extend(hashed);
    objects.extend(drvs.into_iter().map(|(path, aterm)| {
        let digest = sha256(&aterm);
        Object::Drv {
            path,
            aterm,
            digest,
        }
    }));
    Ok(Plan {
        objects,
        store_drvs,
        store_sources,
    })
}

/// Resolves each source from its eval-time origin; the rest go to `locate` in one batch.
fn locate_sources(
    sources: &[StorePath],
    origins: &HashMap<StorePath, SourceOrigin>,
    locate: &Locator<'_>,
) -> Result<(Vec<SourceInfo>, usize), PlanError> {
    let mut infos: Vec<Option<SourceInfo>> = Vec::with_capacity(sources.len());
    let mut unknown: Vec<StorePath> = Vec::new();
    for p in sources {
        let info = origins.get(p).map(|o| match o {
            SourceOrigin::Tree { local, accepted } => SourceInfo {
                data: SourceData::Local {
                    path: local.clone(),
                    accepted: accepted.as_ref().map(|a| Arc::new(a.clone())),
                },
                kind: SourceKind::Source,
            },
            SourceOrigin::Text {
                contents,
                references,
            } => SourceInfo {
                data: SourceData::Memory(Arc::new(contents.clone())),
                kind: SourceKind::Text {
                    references: references.clone(),
                },
            },
        });
        if info.is_none() {
            unknown.push(p.clone());
        }
        infos.push(info);
    }
    if !unknown.is_empty() {
        let found = locate(&unknown).map_err(PlanError)?;
        if found.len() != unknown.len() {
            return Err(PlanError(
                "locator returned the wrong number of sources".into(),
            ));
        }
        let mut found = found.into_iter();
        for slot in infos.iter_mut().filter(|s| s.is_none()) {
            *slot = found.next();
        }
    }
    let n = unknown.len();
    Ok((infos.into_iter().map(|i| i.expect("resolved")).collect(), n))
}

/// Dumps and hashes every source on a thread pool; results keep the input order.
fn hash_sources(sources: &[StorePath], infos: &[SourceInfo]) -> Result<Vec<Object>, PlanError> {
    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .min(sources.len().max(1));
    let next = Mutex::new(0usize);
    let results: Vec<Mutex<Option<Result<Object, PlanError>>>> =
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
                    let path = &sources[i];
                    let info = &infos[i];
                    let r = hash_source(&info.data)
                        .map_err(|e| PlanError(format!("cannot dump '{path}': {e}")))
                        .and_then(|(nar_hash, nar_size, chunks)| {
                            check_source_path(path, &info.kind, &nar_hash)?;
                            Ok(Object::Source {
                                path: path.clone(),
                                data: info.data.clone(),
                                kind: info.kind.clone(),
                                nar_hash,
                                nar_size,
                                chunks,
                            })
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
        let (h, size, chunks) = hash_source(&data).unwrap();
        assert_eq!(h, nar_hash(&tree).unwrap());
        assert_eq!(size, nar_size(&tree).unwrap());
        assert_eq!(chunks.len(), 1);
    }

    #[test]
    fn memory_source_matches_dump_string() {
        let data = SourceData::Memory(Arc::new(b"hello\n".to_vec()));
        let (h, size, _) = hash_source(&data).unwrap();
        assert_eq!(h, crate::store::nar::nar_hash_of_string(b"hello\n"));
        assert!(size > 0);
    }
}

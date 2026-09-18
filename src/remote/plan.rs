//! The upload plan: the closure of the root derivations, hashed the way Nix addresses them.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

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
    /// protocol needs to register the `.drv` as a text path.
    Drv {
        path: StorePath,
        aterm: Arc<Vec<u8>>,
        references: BTreeSet<StorePath>,
    },
}

impl Object {
    /// Hashes a source.
    pub fn source(path: StorePath, info: SourceInfo) -> io::Result<Object> {
        let (nar_hash, nar_size) = hash_source(&info.data)?;
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

/// Checks that a planned source still hashes to its store path: the NAR of the original tree
/// must yield the path eval computed, or the daemon rejects it.
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
/// them and how they were addressed. `ietsp build` asks the local store.
pub type Locator<'a> = dyn Fn(&[StorePath]) -> Result<Vec<SourceInfo>> + 'a;

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
            aterm: Arc::new(aterm.clone()),
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

/// Computes the closure of `roots`: input drvs recursively and every input source, hashed in
/// parallel. Every drv and source comes from `inputs`; what is not there is read from
/// `/nix/store` (drvs) or asked from `locate` (sources) and counted in the plan. Fixed-output
/// drvs are ordinary drvs here; the builder fetches them.
pub fn plan(roots: &[StorePath], inputs: Inputs<'_>, locate: &Locator<'_>) -> Result<Plan> {
    let mut sources: Vec<StorePath> = Vec::new();
    let mut drvs: Vec<(StorePath, Arc<Vec<u8>>, BTreeSet<StorePath>)> = Vec::new();
    let mut seen_src: HashSet<StorePath> = HashSet::new();
    let mut seen_drv: HashSet<StorePath> = HashSet::new();
    let mut store_drvs = 0;

    // Iterative post-order: a drv is emitted after every drv it references.
    type Pending = Option<(Arc<Vec<u8>>, BTreeSet<StorePath>)>;
    let mut stack: Vec<(StorePath, Pending)> =
        roots.iter().rev().map(|r| (r.clone(), None)).collect();
    while let Some((path, done)) = stack.pop() {
        if let Some((aterm, references)) = done {
            drvs.push((path, aterm, references));
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

    let (infos, store_sources) = locate_sources(&sources, inputs.sources, locate)?;
    let hashed = hash_sources(&sources, &infos)?;
    let mut objects = Vec::with_capacity(hashed.len() + drvs.len());
    objects.extend(hashed);
    objects.extend(
        drvs.into_iter()
            .map(|(path, aterm, references)| Object::drv(path, aterm, references)),
    );
    Ok(Plan {
        objects,
        store_drvs,
        store_sources,
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

/// Resolves each source from its eval-time origin; the rest go to `locate` in one batch.
fn locate_sources(
    sources: &[StorePath],
    origins: &HashMap<StorePath, SourceOrigin>,
    locate: &Locator<'_>,
) -> Result<(Vec<SourceInfo>, usize)> {
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
        let found = locate(&unknown)?;
        if found.len() != unknown.len() {
            return Err(Error::Plan(
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
fn hash_sources(sources: &[StorePath], infos: &[SourceInfo]) -> Result<Vec<Object>> {
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
                    let path = &sources[i];
                    let info = &infos[i];
                    let r = Object::source(path.clone(), info.clone())
                        .map_err(|e| Error::io(format!("cannot dump '{path}'"), e))
                        .and_then(|o| {
                            if let Object::Source { nar_hash, kind, .. } = &o {
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

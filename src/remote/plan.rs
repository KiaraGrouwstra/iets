//! The upload plan: the closure of a root derivation as content-addressed objects.

use std::collections::{BTreeSet, HashSet};
use std::io;
use std::path::PathBuf;
use std::sync::Mutex;

use crate::store::drv::{Derivation, DrvError};
use crate::store::hash::{Hash, HashAlgo, Hasher};
use crate::store::nar::dump_path;
use crate::store::path::StorePath;

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

/// Where to read an input source and how it was addressed.
#[derive(Clone, Debug)]
pub struct SourceInfo {
    pub local: PathBuf,
    pub kind: SourceKind,
}

/// One object the builder must hold before it can build the root.
#[derive(Clone, Debug)]
pub enum Object {
    /// A source tree, sent as chunks plus a manifest. `local` is where to read it.
    Source {
        path: StorePath,
        local: PathBuf,
        kind: SourceKind,
        nar_hash: Hash,
        nar_size: u64,
        chunks: Vec<Hash>,
    },
    /// A derivation, sent as its ATerm bytes.
    Drv {
        path: StorePath,
        aterm: Vec<u8>,
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

/// Loads a derivation by its drv path.
pub type Loader<'a> = dyn FnMut(&StorePath) -> Result<Derivation, DrvError> + 'a;
/// Maps input source store paths, in one batch, to where to read them and how they were
/// addressed. Eval will answer from memory; `ietsp build` asks the local store.
pub type Locator<'a> = dyn Fn(&[StorePath]) -> Result<Vec<SourceInfo>, String> + 'a;

/// Hashes a source tree: NAR hash and size, and the sha256 of each chunk.
pub fn hash_source(local: &std::path::Path) -> io::Result<(Hash, u64, Vec<Hash>)> {
    let mut chunks = Vec::new();
    let mut chunker = Chunker::new(|c: Vec<u8>| {
        chunks.push(sha256(&c));
        Ok(())
    });
    let mut nar = Hasher::new(HashAlgo::Sha256);
    dump_path(local, &mut Tee(&mut nar, &mut chunker), &mut |_, _| true)?;
    chunker.finish()?;
    let size = nar.bytes_written();
    Ok((nar.finish(), size, chunks))
}

/// Computes the closure of `root`: input drvs recursively and every input source, hashed in
/// parallel. Fixed-output drvs are ordinary drvs here; the builder fetches them.
pub fn plan(
    root_path: &StorePath,
    root: &Derivation,
    loader: &mut Loader<'_>,
    locate: &Locator<'_>,
) -> Result<Plan, PlanError> {
    let mut sources: Vec<StorePath> = Vec::new();
    let mut drvs: Vec<(StorePath, Vec<u8>)> = Vec::new();
    let mut seen_src: HashSet<StorePath> = HashSet::new();
    let mut seen_drv: HashSet<StorePath> = HashSet::new();

    fn visit(
        path: &StorePath,
        drv: &Derivation,
        loader: &mut Loader<'_>,
        seen_src: &mut HashSet<StorePath>,
        seen_drv: &mut HashSet<StorePath>,
        sources: &mut Vec<StorePath>,
        drvs: &mut Vec<(StorePath, Vec<u8>)>,
    ) -> Result<(), PlanError> {
        for src in &drv.input_srcs {
            if seen_src.insert(src.clone()) {
                sources.push(src.clone());
            }
        }
        for input in drv.input_drvs.keys() {
            if seen_drv.insert(input.clone()) {
                let d = loader(input)?;
                visit(input, &d, loader, seen_src, seen_drv, sources, drvs)?;
            }
        }
        drvs.push((path.clone(), drv.unparse()?.into_bytes()));
        Ok(())
    }

    seen_drv.insert(root_path.clone());
    visit(
        root_path,
        root,
        loader,
        &mut seen_src,
        &mut seen_drv,
        &mut sources,
        &mut drvs,
    )?;

    let hashed = hash_sources(&sources, locate)?;
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
    Ok(Plan { objects })
}

/// Dumps and hashes every source on a thread pool; results keep the input order.
fn hash_sources(sources: &[StorePath], locate: &Locator<'_>) -> Result<Vec<Object>, PlanError> {
    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .min(sources.len().max(1));
    let infos = locate(sources).map_err(PlanError)?;
    if infos.len() != sources.len() {
        return Err(PlanError(
            "locator returned the wrong number of sources".into(),
        ));
    }
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
                    let r = hash_source(&info.local)
                        .map(|(nar_hash, nar_size, chunks)| Object::Source {
                            path: path.clone(),
                            local: info.local.clone(),
                            kind: info.kind.clone(),
                            nar_hash,
                            nar_size,
                            chunks,
                        })
                        .map_err(|e| PlanError(format!("cannot dump '{path}': {e}")));
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
        let (h, size, chunks) = hash_source(&tree).unwrap();
        assert_eq!(h, nar_hash(&tree).unwrap());
        assert_eq!(size, nar_size(&tree).unwrap());
        assert_eq!(chunks.len(), 1);
    }
}

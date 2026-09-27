//! The local content-addressed tree store: where a fetched tree lives instead
//! of `/nix/store`.
//!
//! Two different things are called a store in ietsp. `crate::remote` is a Nix
//! store reached over a protocol, with its trust model. This is the other one:
//! content we own on local disk, under `crate::cachefs`'s rules — a version
//! file, one writer at a time, an atomic rename into place, a TTL sweep
//! (docs/explanation/caching.md) — like the IR cache and the tracing store. `cachefs` does
//! not know about this module; this module is one of its users.
//!
//! Put a tree in, get a store path. Read bytes back for evaluation. Stream a
//! NAR out for an upload. Nothing else.
//!
//! **The store is fixed-output only, and that is what makes it safe in user
//! space.** [`put`] takes the hash the caller already knows — a lock entry's
//! `narHash` — and has no form without it, so the content verifies itself:
//! no signature, no trusted builder, no daemon. An entry's name *is* its
//! content address, which is why a reader never re-hashes and why two
//! processes can share one entry.
//!
//! Today the only user is `crate::source`, which fetches a
//! `github`/`gitlab`/`sourcehut`/`tarball` input as one decompressed tar and
//! indexes it (`crate::fetch::spool`). The spool is kept under
//! `$XDG_CACHE_HOME/ietsp/inputs/<store path base name>/` with its index
//! beside it, and evaluation reads the input straight out of it: `import`,
//! `readFile`, `readDir`, `pathExists`, `readFileType`, `hashFile`,
//! `builtins.path`, `filterSource` and string coercion all answer from the
//! spool by offset. No daemon is needed to evaluate a flake with fetched
//! inputs, and none is written during evaluation; a build uploads the tree
//! from the same spool when its target store lacks it (`remote::plan`).

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use crate::cachefs;
use crate::error::Result;
use crate::eval::Evaluator;
use crate::fetch::spool::{self, Node, Tree};
use crate::store::hash::Hash;
use crate::store::nar::{FileType, PathFilter};
use crate::store::path::{IngestionMethod, StorePath, make_fixed_output_path};

/// The store path a tree with this NAR hash has. The one naming site for a
/// tree the store holds: `fetchTree`, `fetchTarball` and a copied source all
/// ask here, so a stored entry's name and an evaluated store path cannot
/// disagree.
pub fn store_path(name: &str, nar_hash: &Hash) -> Result<StorePath> {
    make_fixed_output_path(
        IngestionMethod::Recursive,
        nar_hash,
        name,
        &std::collections::BTreeSet::new(),
    )
    .map_err(Into::into)
}

/// One tree the store holds: its content, and the NAR hash and size that were
/// verified against the caller's expectation before it was named.
#[derive(Debug)]
pub struct StoredTree {
    tree: Tree,
    nar_hash: Hash,
    nar_size: u64,
    /// Whether this run's read set already names the entry. One observation
    /// covers every read of it, so the flag turns a nixpkgs-sized input's
    /// thousands of reads into a single record and one atomic per read.
    recorded: std::sync::atomic::AtomicBool,
}

impl StoredTree {
    fn new(tree: Tree, nar_hash: Hash, nar_size: u64) -> StoredTree {
        StoredTree {
            tree,
            nar_hash,
            nar_size,
            recorded: std::sync::atomic::AtomicBool::new(false),
        }
    }

    pub fn nar_hash(&self) -> Hash {
        self.nar_hash
    }

    pub fn nar_size(&self) -> u64 {
        self.nar_size
    }

    /// The type of the member at `rel` (`b""` is the tree itself), with
    /// symlinks followed as the kernel follows them under `/nix/store`;
    /// `follow` is false for an `lstat`.
    pub fn stat(&self, rel: &[u8], follow: bool) -> Option<FileType> {
        self.tree.resolve(rel, follow).map(Node::file_type)
    }

    /// A directory's entries, in NAR order. `ENOTDIR` for anything else.
    pub fn list(&self, rel: &[u8]) -> io::Result<Vec<(Vec<u8>, FileType)>> {
        match self.tree.resolve(rel, true) {
            Some(Node::Dir(d)) => Ok(d
                .iter()
                .map(|(n, child)| (n.clone(), child.file_type()))
                .collect()),
            Some(_) => Err(io::Error::from_raw_os_error(libc::ENOTDIR)),
            None => Err(io::Error::from_raw_os_error(libc::ENOENT)),
        }
    }

    /// A regular member's whole contents, in one `pread` into one buffer: no
    /// `open`, no copy through a staging buffer, so a read of a member costs
    /// less than a read of the same file under `/nix/store`. `EISDIR` for a
    /// member that is not a regular file.
    pub fn read(&self, rel: &[u8]) -> io::Result<Vec<u8>> {
        match self.tree.resolve(rel, true) {
            Some(Node::File { at, .. }) => self.tree.read(at),
            Some(_) => Err(io::Error::from_raw_os_error(libc::EISDIR)),
            None => Err(io::Error::from_raw_os_error(libc::ENOENT)),
        }
    }

    /// The NAR of the member at `rel`, with `filter` asked about every entry
    /// below it under the path Nix would show (`shown`), as
    /// `nar::dump_path` asks. This is what an upload streams.
    pub fn dump(
        &self,
        rel: &[u8],
        shown: &Path,
        sink: &mut dyn io::Write,
        filter: &mut PathFilter<'_>,
    ) -> io::Result<()> {
        let node = (self.tree.resolve(rel, true))
            .ok_or_else(|| io::Error::from_raw_os_error(libc::ENOENT))?;
        self.tree.dump_filtered(node, shown, sink, filter)
    }
}

/// `--cache-dir DIR`, else `IETSP_CACHE_DIR`, else `$XDG_CACHE_HOME/ietsp`,
/// else `~/.cache/ietsp`; the entries live in its `inputs/` subdirectory.
/// `None` when no home is known.
pub fn locate(override_dir: Option<&str>) -> Option<PathBuf> {
    Some(crate::cachefs::root(override_dir)?.join("inputs"))
}

/// The store of one run: where it lives, the trees it opened, and what
/// opening them cost. The session owns it (`crate::session`).
#[derive(Default)]
pub struct Store {
    root: OnceLock<Option<PathBuf>>,
    /// Trees opened, by store path base name. An entry is
    /// content-addressed, so one open serves every read of it; a stored
    /// `None` records a name the store does not hold, so a miss is asked
    /// once.
    opened: Mutex<HashMap<String, Option<Arc<StoredTree>>>>,
    entries: std::sync::atomic::AtomicUsize,
    nanos: std::sync::atomic::AtomicU64,
}

fn session() -> Arc<crate::session::Session> {
    crate::session::Session::current()
}

/// The store this run uses, prepared on first use.
pub fn root() -> Option<PathBuf> {
    session()
        .trees
        .root
        .get_or_init(|| {
            let root = locate(None)?;
            cachefs::prepare(&root, cachefs::Shape::StoreEntries, false).then_some(root)
        })
        .clone()
}

fn dir_of(name: &str) -> Option<PathBuf> {
    Some(root()?.join(name))
}

/// What opening the trees of this run cost, for `--timings`: reading each
/// index and decoding it is the one fixed cost the tree store adds to an
/// evaluation that `/nix/store` does not pay.
pub fn report_timings() {
    use std::sync::atomic::Ordering::Relaxed;
    let s = session();
    let n = s.trees.entries.load(Relaxed);
    if n > 0 {
        crate::errln!(
            "inputs: {n} entries opened, {:.3}s",
            s.trees.nanos.load(Relaxed) as f64 / 1e9
        );
    }
}

/// The tree named `name`, opened once per run. An entry whose index does not
/// decode, or whose spool is not the size the index recorded, is wiped: the
/// next evaluation fetches it again.
pub fn get(name: &str) -> Option<Arc<StoredTree>> {
    use std::sync::atomic::Ordering::Relaxed;
    let s = session();
    if let Some(hit) = s.trees.opened.lock().unwrap().get(name) {
        return hit.clone();
    }
    let t0 = std::time::Instant::now();
    let found = dir_of(name).and_then(|dir| match Tree::reopen(&dir) {
        Some((tree, nar_hash, nar_size)) => {
            cachefs::touch(&dir);
            Some(Arc::new(StoredTree::new(tree, nar_hash, nar_size)))
        }
        None => {
            if dir.exists() {
                let _ = std::fs::remove_dir_all(&dir);
            }
            None
        }
    });
    if found.is_some() {
        s.trees.entries.fetch_add(1, Relaxed);
        s.trees
            .nanos
            .fetch_add(t0.elapsed().as_nanos() as u64, Relaxed);
    }
    s.trees
        .opened
        .lock()
        .unwrap()
        .entry(name.to_string())
        .or_insert(found)
        .clone()
}

/// Whether the store holds `name`, for the tracing cache: an entry's presence
/// is the whole observation, because its name is its content address.
pub fn has(name: &[u8], version: u8) -> bool {
    version == spool::INDEX_VERSION
        && std::str::from_utf8(name)
            .ok()
            .and_then(dir_of)
            .is_some_and(|dir| dir.join(spool::SPOOL).exists() && dir.join(spool::INDEX).exists())
}

/// A place to download into, under the store root. The staged directory is
/// named like a `cachefs` temporary, so a killed writer's leftovers are swept.
pub struct Staged {
    dir: PathBuf,
}

impl Staged {
    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

impl Drop for Staged {
    fn drop(&mut self) {
        if self.dir.exists() {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

pub fn stage(name: &str) -> Option<Staged> {
    let dir = root()?.join(format!(
        ".{name}.{}.{}.tmp",
        std::process::id(),
        cachefs::tag()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).ok()?;
    Some(Staged { dir })
}

/// One download per tree: every process that is about to fetch `name` takes
/// this lock first, and one that had to wait finds the tree its sibling
/// stored (`reopen`). The lock file is named like a temporary, so the sweep
/// removes it once it is a day old and unused.
pub fn fetch_lock(name: &str, on_wait: impl FnOnce()) -> Option<cachefs::WriteLock> {
    cachefs::wait_lock(&root()?.join(format!(".{name}.fetching.tmp")), on_wait)
}

/// [`get`], past a miss this process recorded earlier.
pub fn reopen(name: &str) -> Option<Arc<StoredTree>> {
    session().trees.opened.lock().unwrap().remove(name);
    get(name)
}

/// Why a tree was not stored. A mismatch carries the hash the content really
/// has, so the caller can say what it expected instead.
#[derive(Debug)]
pub enum Rejected {
    Mismatch(Hash),
    Io(io::Error),
    /// No cache directory, or the rename lost to something that is not a
    /// sibling's entry: the tree stays unstored, which costs a fetch on the
    /// next run and nothing else.
    Unstorable,
}

/// Puts a tree in, under the store path `name` and `expect` name together.
///
/// `expect` is the hash the caller already knows — a lock entry's `narHash`,
/// a derivation's `outputHash` — and there is no form of this call without
/// it. The tree is hashed here and refused unless it is the one that was
/// asked for, so an entry's name is always its content address and nothing
/// that lacks a prior hash can be admitted. That is the whole of the store's
/// trust model: no signature, no trusted builder, no daemon.
///
/// The rename is atomic, so a reader sees either no entry or a whole one; a
/// sibling that got there first wins and its files stay.
pub fn put(
    name: &str,
    expect: &Hash,
    staged: Staged,
    tree: Tree,
) -> std::result::Result<Arc<StoredTree>, Rejected> {
    let mut hasher = crate::store::hash::Hasher::new(expect.algo);
    tree.dump(&mut hasher).map_err(Rejected::Io)?;
    let nar_size = hasher.bytes_written();
    let nar_hash = hasher.finish();
    if nar_hash != *expect {
        return Err(Rejected::Mismatch(nar_hash));
    }
    let sp = store_path(name, &nar_hash).map_err(|_| Rejected::Unstorable)?;
    let name = sp.base_name();
    let dir = dir_of(name).ok_or(Rejected::Unstorable)?;
    tree.persist(staged.dir(), (&nar_hash, nar_size))
        .map_err(Rejected::Io)?;
    let root = root().ok_or(Rejected::Unstorable)?;
    {
        let _lock = cachefs::write_lock(&root).ok_or(Rejected::Unstorable)?;
        if !dir.exists() && std::fs::rename(staged.dir(), &dir).is_err() {
            return Err(Rejected::Unstorable);
        }
    }
    cachefs::maybe_sweep(&root, cachefs::Shape::StoreEntries, false);
    let stored = Arc::new(StoredTree::new(tree, nar_hash, nar_size));
    session()
        .trees
        .opened
        .lock()
        .unwrap()
        .insert(name.to_string(), Some(stored.clone()));
    Ok(stored)
}

/// The one observation a read of the tree store leaves in the read set. The
/// entry's name is the NAR hash of its tree, so its presence, plus the index
/// version that says the spool can still be read, is the whole check: there is
/// nothing per file to stat and nothing to rehash (docs/explanation/caching.md).
pub fn record(ev: &Evaluator, tree: &StoredTree, name: &str) {
    if ev.trace().is_on()
        && !tree
            .recorded
            .swap(true, std::sync::atomic::Ordering::Relaxed)
    {
        ev.trace().record(crate::trace::Obs::Input {
            entry: name.as_bytes().to_vec(),
            version: spool::INDEX_VERSION,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_staged_directory_goes_when_it_is_dropped() {
        let d = std::env::temp_dir().join(format!("ietsp-inputs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("temp dir");
        let staged = Staged { dir: d.join("x") };
        std::fs::create_dir_all(staged.dir()).expect("staged");
        let at = staged.dir().to_path_buf();
        drop(staged);
        assert!(!at.exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The name an entry gets is the store path the lock's `narHash` implies.
    #[test]
    fn a_tree_is_named_by_its_nar_hash() {
        let h =
            Hash::parse_any("sha256-9i/VTdusq/+NM/tz+J1Re+ojkMB8MBf0QshnYfzHz30=", None).unwrap();
        assert_eq!(
            store_path("source", &h).unwrap().to_full_string(),
            "/nix/store/z4wkdlk69gck09s64a8ghpn6h7bxhkyy-source"
        );
    }
}

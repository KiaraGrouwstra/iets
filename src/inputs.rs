//! The input store: where a fetched flake input lives instead of `/nix/store`.
//!
//! `flake::fetch` downloads a `github`/`gitlab`/`sourcehut`/`tarball` input as
//! one decompressed tar and indexes it (`flake::spool`). That spool used to be
//! an unlinked temporary file, thrown away once the tree had been registered on
//! the local daemon. It is kept here instead, under
//! `$XDG_CACHE_HOME/ietsp/inputs/<store path base name>/`, with its index
//! beside it, and evaluation reads the input straight out of it: `import`,
//! `readFile`, `readDir`, `pathExists`, `readFileType`, `hashFile`,
//! `builtins.path`, `filterSource` and string coercion all answer from the
//! spool by offset. No daemon is needed to evaluate a flake with fetched
//! inputs, and none is written during evaluation; a build uploads the input
//! from the same spool when its target store lacks it (`remote::plan`).
//!
//! The key is the store path the lock implies, which is the NAR hash of the
//! tree, so an entry is safe to share between processes: the hash was verified
//! against the lock before the entry was named. A reader therefore never
//! re-hashes. It checks the index version and the spool's size on open and, on
//! any doubt, wipes the entry instead of repairing it. The store-level rules
//! are `crate::cachefs`'s, the ones the IR and tracing caches follow: a version
//! file, one writer at a time, an atomic rename into place, a TTL sweep
//! (docs/caches.md).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use crate::cachefs;
use crate::eval::Evaluator;
use crate::flake::spool::{self, Tree};
use crate::store::hash::Hash;

/// One opened entry: the spooled archive with its index, and the NAR hash and
/// size the fetch verified against the lock.
#[derive(Debug)]
pub struct Entry {
    pub tree: Tree,
    pub nar_hash: Hash,
    pub nar_size: u64,
    /// Whether this run's read set already names the entry. One observation
    /// covers every read of it, so the flag turns a nixpkgs-sized input's
    /// thousands of reads into a single record and one atomic per read.
    recorded: std::sync::atomic::AtomicBool,
}

impl Entry {
    pub fn new(tree: Tree, nar_hash: Hash, nar_size: u64) -> Entry {
        Entry {
            tree,
            nar_hash,
            nar_size,
            recorded: std::sync::atomic::AtomicBool::new(false),
        }
    }
}

/// `--cache-dir DIR`, else `IETSP_CACHE_DIR`, else `$XDG_CACHE_HOME/ietsp`,
/// else `~/.cache/ietsp`; the entries live in its `inputs/` subdirectory.
/// `None` when no home is known.
pub fn locate(override_dir: Option<&str>) -> Option<PathBuf> {
    let base = match override_dir {
        Some(d) => PathBuf::from(d),
        None => match &crate::settings::get().cache_dir {
            Some(d) => d.clone(),
            None => {
                let base = match std::env::var_os("XDG_CACHE_HOME") {
                    Some(x) if !x.is_empty() => PathBuf::from(x),
                    _ => PathBuf::from(std::env::var_os("HOME")?).join(".cache"),
                };
                base.join("ietsp")
            }
        },
    };
    Some(base.join("inputs"))
}

/// The store this process uses, prepared on first use.
pub fn root() -> Option<&'static Path> {
    static ROOT: OnceLock<Option<PathBuf>> = OnceLock::new();
    ROOT.get_or_init(|| {
        let root = locate(None)?;
        cachefs::prepare(&root, false);
        Some(root)
    })
    .as_deref()
}

fn dir_of(name: &str) -> Option<PathBuf> {
    Some(root()?.join(name))
}

/// Entries this process opened, by store path base name. A spool is
/// content-addressed, so one open serves every read of that input; a stored
/// `None` records a name the store does not hold, so a miss is asked once.
fn opened() -> &'static Mutex<HashMap<String, Option<Arc<Entry>>>> {
    static OPEN: OnceLock<Mutex<HashMap<String, Option<Arc<Entry>>>>> = OnceLock::new();
    OPEN.get_or_init(|| Mutex::new(HashMap::new()))
}

/// What opening the entries of this run cost, for `--timings`: reading each
/// index and decoding it is the one fixed cost the input store adds to an
/// evaluation that `/nix/store` does not pay.
static OPENED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static OPEN_NANOS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub fn report_timings() {
    let n = OPENED.load(std::sync::atomic::Ordering::Relaxed);
    if n > 0 {
        eprintln!(
            "inputs: {n} entries opened, {:.3}s",
            OPEN_NANOS.load(std::sync::atomic::Ordering::Relaxed) as f64 / 1e9
        );
    }
}

/// The entry named `name`, opened once per process. An entry whose index does
/// not decode, or whose spool is not the size the index recorded, is wiped: the
/// next evaluation fetches it again.
pub fn open(name: &str) -> Option<Arc<Entry>> {
    if let Some(hit) = opened().lock().unwrap().get(name) {
        return hit.clone();
    }
    let t0 = std::time::Instant::now();
    let found = dir_of(name).and_then(|dir| match Tree::reopen(&dir) {
        Some((tree, nar_hash, nar_size)) => {
            cachefs::touch(&dir);
            Some(Arc::new(Entry::new(tree, nar_hash, nar_size)))
        }
        None => {
            if dir.exists() {
                let _ = std::fs::remove_dir_all(&dir);
            }
            None
        }
    });
    if found.is_some() {
        OPENED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        OPEN_NANOS.fetch_add(
            t0.elapsed().as_nanos() as u64,
            std::sync::atomic::Ordering::Relaxed,
        );
    }
    opened()
        .lock()
        .unwrap()
        .entry(name.to_string())
        .or_insert(found)
        .clone()
}

/// Whether the store holds `name`, for the tracing cache: an entry's presence
/// is the whole observation, because its name is its content address.
pub fn holds(name: &[u8], version: u8) -> bool {
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

/// Moves a staged entry into place and keeps it open. The rename is atomic, so
/// a reader sees either no entry or a whole one; a sibling that got there first
/// wins and its files stay. `None` leaves the tree unstored, which costs a
/// fetch on the next run and nothing else.
pub fn store(name: &str, staged: Staged, entry: Entry) -> Option<Arc<Entry>> {
    let dir = dir_of(name)?;
    entry
        .tree
        .persist(staged.dir(), (&entry.nar_hash, entry.nar_size))
        .ok()?;
    let root = root()?;
    {
        let _lock = cachefs::write_lock(root)?;
        if !dir.exists() && std::fs::rename(staged.dir(), &dir).is_err() {
            return None;
        }
    }
    cachefs::maybe_sweep(root, cachefs::Shape::StoreEntries, false);
    let entry = Arc::new(entry);
    opened()
        .lock()
        .unwrap()
        .insert(name.to_string(), Some(entry.clone()));
    Some(entry)
}

/// The one observation an input-store read leaves in the read set. The entry's
/// name is the NAR hash of its tree, so its presence, plus the index version
/// that says the spool can still be read, is the whole check: there is nothing
/// per file to stat and nothing to rehash (docs/tracing-cache.md).
pub fn record(ev: &Evaluator, entry: &Entry, name: &str) {
    if ev.trace.is_on()
        && !entry
            .recorded
            .swap(true, std::sync::atomic::Ordering::Relaxed)
    {
        ev.trace.record(crate::trace::Obs::Input {
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
}

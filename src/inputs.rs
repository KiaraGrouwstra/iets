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
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use crate::builtins::store_hooks::{Access, Whence, whence};
use crate::cachefs;
use crate::eval::{Error, ErrorKind, EvalResult, Evaluator, Pos};
use crate::flake::spool::{self, Node, Tree};
use crate::runtime::Word;
use crate::store::hash::{Hash, HashAlgo, Hasher};
use crate::store::nar::{FileType, PathFilter};
use crate::store::path::StorePath;
use crate::store::source::SourceOrigin;
use crate::trace::Kind;

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

// ---- the resolver ------------------------------------------------------------

/// The origin record a store path served from this store gets. `rel` is the
/// path inside the entry, empty for the input itself.
pub fn origin(name: &str, rel: &[u8], nar_hash: Hash, nar_size: u64) -> SourceOrigin {
    SourceOrigin::Input {
        entry: name.to_string(),
        rel: rel.to_vec(),
        nar_hash,
        nar_size,
    }
}

/// `store_hooks::locate` for a store path this store holds: `rel` is the
/// origin's path inside the entry and `sub` what the read asked for below the
/// store path. A vanished entry falls back to the store, which a build has
/// usually filled by then.
pub fn access(name: &str, rel: &[u8], sub: &[u8]) -> Access {
    let Some(entry) = open(name) else {
        return Access::Store;
    };
    let Ok(base) = StorePath::from_base_name(name) else {
        return Access::Store;
    };
    Access::Input {
        entry,
        base,
        rel: join(rel, sub),
    }
}

fn join(rel: &[u8], sub: &[u8]) -> Vec<u8> {
    if sub.is_empty() {
        return rel.to_vec();
    }
    if rel.is_empty() {
        return sub.to_vec();
    }
    let mut v = Vec::with_capacity(rel.len() + 1 + sub.len());
    v.extend_from_slice(rel);
    v.push(b'/');
    v.extend_from_slice(sub);
    v
}

/// The NAR of an input source, for `remote::plan`. The entry is opened when the
/// NAR is asked for, so a `build` that hits the tracing cache needs no
/// evaluation to have run.
pub fn nar_source(name: &str, rel: &[u8]) -> Arc<dyn crate::remote::plan::NarSource> {
    Arc::new(Spooled {
        name: name.to_string(),
        rel: rel.to_vec(),
    })
}

#[derive(Debug)]
struct Spooled {
    name: String,
    rel: Vec<u8>,
}

impl crate::remote::plan::NarSource for Spooled {
    fn dump(&self, sink: &mut dyn io::Write) -> io::Result<()> {
        let entry = open(&self.name).ok_or_else(|| {
            io::Error::other(format!(
                "the input store no longer holds '{}'; evaluate again to fetch it",
                self.name
            ))
        })?;
        let node = entry.tree.resolve(&self.rel, true).ok_or_else(enoent)?;
        entry
            .tree
            .dump_filtered(node, Path::new("/"), sink, &mut |_, _| true)
    }

    fn describe(&self) -> String {
        match self.rel.is_empty() {
            true => format!("<the input store entry {}>", self.name),
            false => format!(
                "<the input store entry {}, {}>",
                self.name,
                String::from_utf8_lossy(&self.rel)
            ),
        }
    }
}

// ---- what evaluation reads ---------------------------------------------------

fn enoent() -> io::Error {
    io::Error::from_raw_os_error(libc::ENOENT)
}

/// `strerror(ENOENT)` alone, the text `import`'s message has always carried
/// (no Rust " (os error 2)" suffix).
fn missing() -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, "No such file or directory")
}

fn os(p: &[u8]) -> &Path {
    Path::new(<std::ffi::OsStr as std::os::unix::ffi::OsStrExt>::from_bytes(p))
}

fn shown_str(p: &[u8]) -> std::borrow::Cow<'_, str> {
    String::from_utf8_lossy(p)
}

fn io_err(e: &io::Error, shown: &[u8], pos: Pos) -> Box<Error> {
    let msg = if e.kind() == io::ErrorKind::NotFound {
        format!("path '{}' does not exist", shown_str(shown))
    } else {
        format!("getting status of '{}': {e}", shown_str(shown))
    };
    Error::boxed(ErrorKind::Io, msg, pos)
}

/// The one observation an input-store read leaves in the read set. The entry's
/// name is the NAR hash of its tree, so its presence, plus the index version
/// that says the spool can still be read, is the whole check: there is nothing
/// per file to stat and nothing to rehash (docs/tracing-cache.md).
pub fn record(ev: &Evaluator, entry: &Entry, base: &StorePath) {
    if ev.trace.is_on()
        && !entry
            .recorded
            .swap(true, std::sync::atomic::Ordering::Relaxed)
    {
        ev.trace.record(crate::trace::Obs::Input {
            entry: base.base_name().as_bytes().to_vec(),
            version: spool::INDEX_VERSION,
        });
    }
}

/// A regular member's bytes.
fn read_at(ev: &Evaluator, entry: &Entry, base: &StorePath, rel: &[u8]) -> io::Result<Vec<u8>> {
    let node = entry.tree.resolve(rel, true).ok_or_else(enoent)?;
    let Node::File { at, .. } = node else {
        return Err(io::Error::from_raw_os_error(libc::EISDIR));
    };
    record(ev, entry, base);
    entry.tree.read(at)
}

/// `import`'s stat: `true` when the path is a directory, so `/default.nix` is
/// appended. The one hook `eval::resolve_import_path` needs.
pub fn import_is_dir(ev: &Evaluator, path: &[u8]) -> io::Result<bool> {
    match whence(ev, path).ok_or_else(missing)? {
        Whence::In(entry, base, rel) => {
            let node = entry.tree.resolve(&rel, true).ok_or_else(missing)?;
            record(ev, &entry, &base);
            Ok(matches!(node, Node::Dir(_)))
        }
        Whence::Fs(phys) => {
            let meta = std::fs::metadata(os(&phys))?;
            if meta.is_dir() {
                // a regular file needs no Stat: its File observation checks the type
                ev.trace.stat(&phys, true, Kind::Directory);
                return Ok(true);
            }
            Ok(false)
        }
    }
}

/// The bytes `import` compiles, recorded in the read set. The one hook
/// `eval::read_source` needs.
pub fn read_source(ev: &Evaluator, path: &[u8]) -> io::Result<Vec<u8>> {
    match whence(ev, path).ok_or_else(enoent)? {
        Whence::In(entry, base, rel) => read_at(ev, &entry, &base, &rel),
        Whence::Fs(phys) => {
            let mut f = std::fs::File::open(os(&phys))?;
            let meta = f.metadata()?;
            let mut s = Vec::with_capacity(meta.len() as usize);
            std::io::Read::read_to_end(&mut f, &mut s)?;
            ev.trace.file(&phys, &meta, &s);
            Ok(s)
        }
    }
}

/// `builtins.readFile`.
pub fn read_file(
    ev: &mut Evaluator,
    entry: &Entry,
    base: &StorePath,
    rel: &[u8],
    shown: &[u8],
    pos: Pos,
) -> EvalResult<Word> {
    let bytes = read_at(ev, entry, base, rel).map_err(|e| {
        if e.raw_os_error() == Some(libc::EISDIR) {
            Error::boxed(
                ErrorKind::Io,
                format!("file '{}' is not a regular file", shown_str(shown)),
                pos,
            )
        } else {
            io_err(&e, shown, pos)
        }
    })?;
    if bytes.contains(&0) {
        return Err(Error::boxed(
            ErrorKind::Eval,
            format!(
                "the contents of the file '{}' cannot be represented as a Nix string",
                shown_str(shown)
            ),
            pos,
        ));
    }
    Ok(ev.mk_string(&bytes))
}

/// `builtins.readDir`. Nothing filters an input: the tree is what the lock's
/// `narHash` covers, entry for entry.
pub fn read_dir(
    ev: &mut Evaluator,
    entry: &Entry,
    base: &StorePath,
    rel: &[u8],
    shown: &[u8],
    pos: Pos,
) -> EvalResult<Word> {
    let node = entry
        .tree
        .resolve(rel, true)
        .ok_or_else(|| io_err(&enoent(), shown, pos))?;
    let Node::Dir(d) = node else {
        return Err(Error::boxed(
            ErrorKind::Io,
            format!("'{}' is not a directory", shown_str(shown)),
            pos,
        ));
    };
    record(ev, entry, base);
    let names = [
        ev.mk_string(FileType::Regular.nix_name().as_bytes()),
        ev.mk_string(FileType::Directory.nix_name().as_bytes()),
        ev.mk_string(FileType::Symlink.nix_name().as_bytes()),
    ];
    let mut entries = Vec::with_capacity(d.len());
    for (name, child) in d {
        let idx = match child.file_type() {
            FileType::Directory => 1,
            FileType::Symlink => 2,
            _ => 0,
        };
        let sym = ev.sym(name);
        entries.push((sym, Pos::NONE, names[idx]));
    }
    ev.mk_attrs(&mut entries, pos)
}

/// `builtins.readFileType`: an `lstat`, so a symlink stays one.
pub fn read_file_type(
    ev: &mut Evaluator,
    entry: &Entry,
    base: &StorePath,
    rel: &[u8],
    shown: &[u8],
    pos: Pos,
) -> EvalResult<Word> {
    let node = entry
        .tree
        .resolve(rel, false)
        .ok_or_else(|| io_err(&enoent(), shown, pos))?;
    record(ev, entry, base);
    Ok(ev.mk_string(node.file_type().nix_name().as_bytes()))
}

/// `builtins.pathExists`. A trailing `/` or `/.` asks for a directory, as it
/// does for a real path.
pub fn path_exists(
    ev: &Evaluator,
    entry: &Entry,
    base: &StorePath,
    rel: &[u8],
    must_be_dir: bool,
) -> Word {
    record(ev, entry, base);
    match entry.tree.resolve(rel, must_be_dir) {
        Some(Node::Dir(_)) => Word::TRUE,
        Some(_) => Word::from_bool(!must_be_dir),
        None => Word::FALSE,
    }
}

/// `builtins.hashFile`.
pub fn hash_file(
    ev: &Evaluator,
    entry: &Entry,
    base: &StorePath,
    rel: &[u8],
    algo: HashAlgo,
) -> io::Result<Hash> {
    let bytes = read_at(ev, entry, base, rel)?;
    Ok(crate::store::hash::hash_bytes(algo, &bytes))
}

/// The NAR hash and size of a member, for `builtins.path`, `filterSource` and
/// string coercion. `filter` is asked about every entry below `rel` with the
/// path Nix shows it: the store path plus the name inside it.
pub fn nar_hash(
    entry: &Entry,
    rel: &[u8],
    shown: &Path,
    filter: &mut PathFilter<'_>,
) -> io::Result<(Hash, u64)> {
    let node = entry.tree.resolve(rel, true).ok_or_else(enoent)?;
    let mut h = Hasher::new(HashAlgo::Sha256);
    entry.tree.dump_filtered(node, shown, &mut h, filter)?;
    let size = h.bytes_written();
    Ok((h.finish(), size))
}

/// `"${<input>/x}"`: the store path the member would be copied to. The input
/// itself needs no hashing, because its store path is the one the lock named
/// and its NAR hash is in the index.
pub fn copy_to_store(
    ev: &mut Evaluator,
    entry: &Entry,
    base: &StorePath,
    rel: &[u8],
    path: &'static [u8],
    pos: Pos,
) -> EvalResult<&'static [u8]> {
    record(ev, entry, base);
    let (sp, nar) = if rel.is_empty() {
        (base.clone(), (entry.nar_hash, entry.nar_size))
    } else {
        let nar = nar_hash(entry, rel, os(path), &mut |_, _| true).map_err(|e| {
            Error::boxed(
                ErrorKind::Io,
                format!("copying '{}' to the store: {e}", shown_str(path)),
                pos,
            )
        })?;
        let name = os(path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| "source".into());
        let sp = crate::store::path::make_fixed_output_path(
            crate::store::path::IngestionMethod::Recursive,
            &nar.0,
            &name,
            &std::collections::BTreeSet::new(),
        )
        .map_err(|e| Error::boxed(ErrorKind::Eval, e.0, pos))?;
        (sp, nar)
    };
    let full = ev.arena.alloc_bytes(sp.to_full_string().as_bytes());
    ev.src_to_store.insert(path.to_vec(), full);
    ev.sources
        .insert(sp, origin(base.base_name(), rel, nar.0, nar.1));
    Ok(full)
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

    #[test]
    fn a_relative_path_inside_an_entry_joins_once() {
        assert_eq!(join(b"", b""), b"");
        assert_eq!(join(b"lib", b""), b"lib");
        assert_eq!(join(b"", b"lib"), b"lib");
        assert_eq!(join(b"lib", b"x.nix"), b"lib/x.nix");
    }
}

//! On-disk hygiene shared by the two caches, the compiled-IR cache
//! (`src/compile/cache.rs`) and the tracing run cache (`src/trace/`): a format
//! version file, a single-writer lock, mtime touches, and TTL sweeps. The
//! roots store (`roots/`, `docs/explanation/caching.md`) takes the TTL sweep
//! and nothing else: a symlink has no format to version and no torn state to
//! wipe.
//!
//! Many `ietsp` processes share one cache directory (eval-jobs workers,
//! direnv, the classic command names). The invariant they hold together:
//! **an entry is one file, written to a temporary name and renamed into
//! place, so one rename publishes the whole entry, a reader sees either the
//! whole previous entry or the whole new one and never locks; a writer holds
//! the store's `flock` for exactly one such file, and a writer that cannot
//! take it within `LOCK_TIMEOUT` drops the write.** A
//! dropped write costs one cache miss later, which is why no path here waits
//! on a sibling worker or fails a run. The one exception is `wait_lock`, for a
//! download a sibling is already doing.

use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::io::{AsRawFd, RawFd};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The store layout. A mismatch wipes the store; bump it when an entry
/// encoding changes in a way the keys do not already cover.
pub const FORMAT: &[u8] = b"1";

const DAY: u64 = 24 * 60 * 60;
/// Entries unread for this long are swept.
pub const TTL_DAYS: u64 = 30;
/// A hit inside this window writes nothing.
const TOUCH_INTERVAL: Duration = Duration::from_secs(DAY);
/// A sweep runs at most this often per store.
const GC_INTERVAL: Duration = Duration::from_secs(DAY);
/// Leftover temporary files from a killed writer go after this long.
const TMP_TTL: Duration = Duration::from_secs(DAY);
const LOCK_TIMEOUT: Duration = Duration::from_millis(50);
const LOCK_POLL: Duration = Duration::from_millis(2);

const VERSION: &str = "version";
const LOCK: &str = "lock";
const STAMP: &str = "gc-stamp";

/// The directory every cache store lives under: `--cache-dir DIR`, else
/// `IETSP_CACHE_DIR`, else `$XDG_CACHE_HOME/ietsp`, else `~/.cache/ietsp`.
/// Each store takes a subdirectory of it — `trace/`, `ir/`, `inputs/`,
/// `roots/` — so the directory the user names is never a removal target.
/// `None` when no home is known.
pub fn root(override_dir: Option<&str>) -> Option<PathBuf> {
    if let Some(d) = override_dir {
        return Some(PathBuf::from(d));
    }
    if let Some(d) = &crate::session::Session::current().settings().cache_dir {
        return Some(d.clone());
    }
    let base = match std::env::var_os("XDG_CACHE_HOME") {
        Some(x) if !x.is_empty() => PathBuf::from(x),
        _ => PathBuf::from(std::env::var_os("HOME")?).join(".cache"),
    };
    Some(base.join("ietsp"))
}

/// The GC roots ietsp owns, under `<cache root>/roots`. A root is not a cache
/// entry: it holds nothing that can be recomputed, it is a claim on the
/// daemon's store. The TTL sweep still fits — a root nobody has used in
/// `TTL_DAYS` days is one nobody wants kept — so `ietsp cache gc` walks it,
/// but the version file, the writer lock and poison-and-wipe do not, which is
/// why it is not `prepare`d.
pub fn roots(override_dir: Option<&str>) -> Option<PathBuf> {
    Some(root(override_dir)?.join("roots"))
}

/// The link in the roots store that `parts` name — what the root is for, then
/// whatever tells two of them apart. They are length-prefixed into one 64-hex
/// key, so no part can spell another one's boundary.
pub fn root_link(override_dir: Option<&str>, parts: &[&[u8]]) -> Option<PathBuf> {
    let mut bytes = Vec::new();
    for p in parts {
        bytes.extend_from_slice(&(p.len() as u64).to_le_bytes());
        bytes.extend_from_slice(p);
    }
    let name: String = crate::trace::sha256(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    Some(roots(override_dir)?.join(name))
}

/// Whether a cache entry may send a run to `program`: it has to be a file
/// inside `out_path`, and `out_path` a path of the store directory. A cache
/// directory is writable by whoever runs ietsp, so an entry that named a
/// program freely would be a way to start one; what the store holds under
/// that name is the daemon's to say. `nix fmt` is the one command that runs
/// what an entry remembered.
pub fn program_of_output(out_path: &str, program: &str) -> bool {
    let Ok(out) = crate::store::path::StorePath::parse(out_path) else {
        return false;
    };
    let program = Path::new(program);
    program.starts_with(out.to_full_string())
        && !program
            .components()
            .any(|c| c == std::path::Component::ParentDir)
}

/// How a store names its entries under its root.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    /// One directory per entry, named by the 64-hex run key (tracing cache).
    Dirs,
    /// One file per entry, named by its key past the two-character shard
    /// directory the key's first two characters make (IR cache).
    ShardedFiles,
    /// One directory per entry, named by a store path's base name (the input
    /// store, `crate::treestore`).
    StoreEntries,
    /// One symlink per entry, named by a 64-hex key (the roots store). The
    /// link points into `/nix/store` and is what the daemon's
    /// `gcroots/auto/<hash>` entry names, so removing it releases the root.
    Links,
}

/// A sweep only ever removes something named like a key, so the store-level
/// files, the `ir/` store under a shared `--cache-dir`, and anything else a
/// user put in the directory all survive.
fn is_key(name: &std::ffi::OsStr, shape: Shape) -> bool {
    let b = name.as_bytes();
    match shape {
        Shape::StoreEntries => std::str::from_utf8(b)
            .is_ok_and(|n| crate::store::path::StorePath::from_base_name(n).is_ok()),
        Shape::Dirs | Shape::Links => b.len() == 64 && b.iter().all(u8::is_ascii_hexdigit),
        Shape::ShardedFiles => b.len() == 62 && b.iter().all(u8::is_ascii_hexdigit),
    }
}

fn is_shard(name: &std::ffi::OsStr) -> bool {
    let b = name.as_bytes();
    b.len() == 2 && b.iter().all(u8::is_ascii_hexdigit)
}

fn is_tmp(name: &std::ffi::OsStr) -> bool {
    let b = name.as_bytes();
    b.starts_with(b".") && b.ends_with(b".tmp")
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn age(path: &Path) -> Option<Duration> {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()?
        .elapsed()
        .ok()
}

/// The age of an entry of `shape`: a symlink entry is aged by the link, not by
/// what it points at, whose mtime is the store's epoch.
fn entry_age(path: &Path, shape: Shape) -> Option<Duration> {
    match shape {
        Shape::Links => fs::symlink_metadata(path)
            .and_then(|m| m.modified())
            .ok()?
            .elapsed()
            .ok(),
        _ => age(path),
    }
}

/// Marks a link in the roots store as used now, so the TTL sweep keeps the
/// roots of the flakes somebody still formats. `utimensat` with a null time
/// sets both stamps to the current time, and `AT_SYMLINK_NOFOLLOW` sets them
/// on the link itself.
pub fn touch_link(path: &Path) {
    let Ok(c) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return;
    };
    unsafe {
        libc::utimensat(
            libc::AT_FDCWD,
            c.as_ptr(),
            std::ptr::null(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
}

fn stamp_age(bytes: &[u8]) -> Option<Duration> {
    let secs: u64 = std::str::from_utf8(bytes.trim_ascii()).ok()?.parse().ok()?;
    Some(Duration::from_secs(now_secs().saturating_sub(secs)))
}

/// Writes `chunks`, one after the other, to `dir/name` through a temporary
/// file, so a reader sees either the old contents or the new ones. Several
/// chunks make one file, which is how an entry of many parts is published in
/// one rename without first joining it into one buffer. `tag` separates
/// concurrent writers of one name inside one process.
pub fn write_atomic(dir: &Path, name: &str, tag: u64, chunks: &[&[u8]]) -> io::Result<()> {
    use io::Write;
    let tmp = dir.join(format!(".{name}.{}.{tag}.tmp", std::process::id()));
    let write = || -> io::Result<()> {
        let mut f = fs::File::create(&tmp)?;
        for c in chunks {
            f.write_all(c)?;
        }
        Ok(())
    };
    match write() {
        Ok(()) => fs::rename(&tmp, dir.join(name)).inspect_err(|_| {
            let _ = fs::remove_file(&tmp);
        }),
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// A counter that keeps two writers of one name inside one process apart.
/// Process-wide by nature: the names it disambiguates are process-wide.
pub fn tag() -> u64 {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

// ---- store-level structure --------------------------------------------------

/// Whose directory a store root is, before ietsp writes to it.
enum Owner {
    /// ietsp's, and usable as it stands.
    Ready,
    /// ietsp's, but of another format or with a broken store-level file.
    Stale(&'static str),
    /// Not ietsp's: it holds a name that no store of this shape can have.
    Foreign,
}

/// A name a store of `shape` puts in its own root: a store-level file, a
/// temporary file, an entry, or an IR shard.
fn belongs(name: &std::ffi::OsStr, shape: Shape) -> bool {
    matches!(name.as_bytes(), b"version" | b"lock" | b"gc-stamp")
        || is_tmp(name)
        || is_key(name, shape)
        || (matches!(shape, Shape::ShardedFiles) && is_shard(name))
}

/// True when every name in `root` is one this store's own layout makes. An
/// unreadable directory or entry counts as foreign: ietsp removes only what
/// it can prove it put there.
fn all_ours(root: &Path, shape: Shape) -> bool {
    let Ok(rd) = fs::read_dir(root) else {
        return false;
    };
    for e in rd {
        match e {
            Ok(e) if belongs(&e.file_name(), shape) => {}
            _ => return false,
        }
    }
    true
}

fn owner(root: &Path, shape: Shape) -> Owner {
    if !root.exists() {
        return Owner::Ready;
    }
    match fs::read(root.join(VERSION)) {
        Ok(v) if v.trim_ascii() == FORMAT => {}
        Ok(_) => return Owner::Stale("format version mismatch"),
        Err(_) if empty(root) => {}
        Err(_) if all_ours(root, shape) => return Owner::Stale("no format version"),
        Err(_) => return Owner::Foreign,
    }
    if root.join(LOCK).is_dir() {
        return Owner::Stale("lock is not a file");
    }
    match fs::read(root.join(STAMP)) {
        Ok(s) if stamp_age(&s).is_none() => Owner::Stale("unreadable gc stamp"),
        _ => Owner::Ready,
    }
}

/// Empties a store ietsp owns, touching only the names its own layout makes:
/// the store-level files, the temporary files, the entries and the IR
/// shards. Anything else in the directory stays.
fn wipe(root: &Path, shape: Shape) {
    for name in [VERSION, LOCK, STAMP] {
        let p = root.join(name);
        let _ = fs::remove_file(&p).or_else(|_| fs::remove_dir_all(&p));
    }
    let Ok(rd) = fs::read_dir(root) else {
        return;
    };
    for e in rd.flatten() {
        let name = e.file_name();
        let path = e.path();
        if is_tmp(&name) {
            let _ = fs::remove_file(&path).or_else(|_| fs::remove_dir_all(&path));
        } else if is_key(&name, shape) {
            drop_entry(&path, shape);
        } else if matches!(shape, Shape::ShardedFiles) && is_shard(&name) {
            wipe_shard(&path);
        }
    }
}

fn wipe_shard(dir: &Path) {
    let Ok(rd) = fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let name = e.file_name();
        if is_tmp(&name) || is_key(&name, Shape::ShardedFiles) {
            let _ = fs::remove_file(e.path());
        }
    }
    let _ = fs::remove_dir(dir);
}

/// Makes `root` usable, and says whether it is. A version mismatch or an
/// unreadable store-level file wipes the store and starts it empty: a cache
/// is never repaired in place. A directory that is not ietsp's — it exists,
/// holds something, and carries no store of this shape — is refused instead,
/// because `--cache-dir` points this at a directory the user names.
#[must_use]
pub fn prepare(root: &Path, shape: Shape, timings: bool) -> bool {
    match owner(root, shape) {
        Owner::Ready => {}
        Owner::Stale(why) => {
            if timings {
                crate::errln!("cache: wiping {} ({why})", root.display());
            }
            wipe(root, shape);
        }
        Owner::Foreign => {
            crate::errln!(
                "error: {} is not an ietsp cache and is not empty, so ietsp will not \
                 use it; point --cache-dir (or IETSP_CACHE_DIR) at a new or empty \
                 directory, or empty this one",
                root.display()
            );
            return false;
        }
    }
    if fs::create_dir_all(root).is_ok() && fs::read(root.join(VERSION)).is_err() {
        let _ = write_atomic(root, VERSION, tag(), &[FORMAT]);
    }
    true
}

fn empty(root: &Path) -> bool {
    fs::read_dir(root).is_ok_and(|mut d| d.next().is_none())
}

/// The store's single-writer lock, released when this value drops.
pub struct WriteLock(fs::File);

impl Drop for WriteLock {
    fn drop(&mut self) {
        unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}

/// Takes the store's writer lock for one file write. `None` when a sibling
/// holds it longer than `LOCK_TIMEOUT`: the caller then skips the write,
/// which is a later miss and never a stall.
pub fn write_lock(root: &Path) -> Option<WriteLock> {
    let f = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(root.join(LOCK))
        .ok()?;
    let mut waited = Duration::ZERO;
    loop {
        if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Some(WriteLock(f));
        }
        if waited >= LOCK_TIMEOUT {
            return None;
        }
        std::thread::sleep(LOCK_POLL);
        waited += LOCK_POLL;
    }
}

/// An exclusive `flock` on the file `path`, taken without a timeout: the one
/// wait in the caches, for work that costs far more than waiting (a
/// download). `on_wait` runs once when a sibling holds it. The file is
/// touched, so a sweep never takes a lock file that is in use.
pub fn wait_lock(path: &Path, on_wait: impl FnOnce()) -> Option<WriteLock> {
    let f = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(path)
        .ok()?;
    let fd = f.as_raw_fd();
    if unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        on_wait();
        while unsafe { libc::flock(fd, libc::LOCK_EX) } != 0 {
            if io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
                return None;
            }
        }
    }
    set_now(fd, None);
    Some(WriteLock(f))
}

// ---- touching --------------------------------------------------------------

fn set_now(fd: RawFd, path: Option<&Path>) {
    let times = [
        libc::timespec {
            tv_sec: 0,
            tv_nsec: libc::UTIME_NOW,
        },
        libc::timespec {
            tv_sec: 0,
            tv_nsec: libc::UTIME_NOW,
        },
    ];
    match path {
        None => unsafe {
            libc::futimens(fd, times.as_ptr());
        },
        Some(p) => {
            let Ok(c) = std::ffi::CString::new(p.as_os_str().as_bytes()) else {
                return;
            };
            unsafe {
                libc::utimensat(libc::AT_FDCWD, c.as_ptr(), times.as_ptr(), 0);
            }
        }
    }
}

/// Marks `path` as used, at most once per `TOUCH_INTERVAL`: a hot cache on a
/// busy box does not churn the disk.
pub fn touch(path: &Path) {
    if age(path).is_some_and(|a| a < TOUCH_INTERVAL) {
        return;
    }
    set_now(-1, Some(path));
}

/// `touch` for a file already open, with `modified` from its own `fstat`.
pub fn touch_open(f: &fs::File, modified: Option<SystemTime>) {
    if modified
        .and_then(|m| m.elapsed().ok())
        .is_some_and(|a| a < TOUCH_INTERVAL)
    {
        return;
    }
    set_now(f.as_raw_fd(), None);
}

// ---- sweeping --------------------------------------------------------------

#[derive(Default, Debug, PartialEq, Eq)]
pub struct Swept {
    pub scanned: usize,
    pub removed: usize,
    pub bytes: u64,
}

fn entry_size(path: &Path, shape: Shape) -> u64 {
    match shape {
        Shape::ShardedFiles => fs::metadata(path).map_or(0, |m| m.len()),
        // a root holds no bytes of its own; what it frees is the daemon's to say
        Shape::Links => 0,
        Shape::Dirs | Shape::StoreEntries => fs::read_dir(path).map_or(0, |d| {
            d.flatten()
                .filter_map(|e| e.metadata().ok())
                .map(|m| m.len())
                .sum()
        }),
    }
}

fn drop_entry(path: &Path, shape: Shape) -> bool {
    match shape {
        Shape::ShardedFiles | Shape::Links => fs::remove_file(path).is_ok(),
        Shape::Dirs | Shape::StoreEntries => fs::remove_dir_all(path).is_ok(),
    }
}

fn sweep_dir(dir: &Path, shape: Shape, ttl: Duration, dry_run: bool, out: &mut Swept) {
    let Ok(rd) = fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let name = e.file_name();
        let path = e.path();
        if is_tmp(&name) {
            if !dry_run && age(&path).is_some_and(|a| a >= TMP_TTL) {
                // A killed writer leaves a file in the two hashed stores and a
                // whole staged directory in the input store.
                let _ = fs::remove_file(&path).or_else(|_| fs::remove_dir_all(&path));
            }
            continue;
        }
        if !is_key(&name, shape) {
            continue;
        }
        out.scanned += 1;
        let Some(a) = entry_age(&path, shape) else {
            continue;
        };
        if a < ttl {
            continue;
        }
        let bytes = entry_size(&path, shape);
        if dry_run || drop_entry(&path, shape) {
            out.removed += 1;
            out.bytes += bytes;
        }
    }
}

/// Removes every entry unread for longer than `ttl`, plus the temporary files
/// a killed writer left behind. `dry_run` counts and changes nothing.
pub fn sweep(root: &Path, shape: Shape, ttl: Duration, dry_run: bool) -> Swept {
    let mut out = Swept::default();
    match shape {
        Shape::Dirs | Shape::StoreEntries | Shape::Links => {
            sweep_dir(root, shape, ttl, dry_run, &mut out);
        }
        Shape::ShardedFiles => {
            let Ok(rd) = fs::read_dir(root) else {
                return out;
            };
            for shard in rd.flatten() {
                if !is_shard(&shard.file_name()) {
                    continue;
                }
                let dir = shard.path();
                if dir.is_dir() {
                    sweep_dir(&dir, shape, ttl, dry_run, &mut out);
                    if !dry_run {
                        let _ = fs::remove_dir(&dir);
                    }
                }
            }
        }
    }
    out
}

/// A sweep on a write path, at most once a day per store. The stamp is
/// claimed under the writer lock and the walk runs unlocked, so a sweep of a
/// large store never holds a sibling's write back.
pub fn maybe_sweep(root: &Path, shape: Shape, timings: bool) {
    let due = |root: &Path| {
        fs::read(root.join(STAMP))
            .ok()
            .and_then(|s| stamp_age(&s))
            .is_none_or(|a| a >= GC_INTERVAL)
    };
    if !due(root) {
        return;
    }
    {
        let Some(_lock) = write_lock(root) else {
            return;
        };
        if !due(root) {
            return;
        }
        if write_atomic(root, STAMP, tag(), &[now_secs().to_string().as_bytes()]).is_err() {
            return;
        }
    }
    let s = sweep(root, shape, Duration::from_secs(TTL_DAYS * DAY), false);
    if timings && s.scanned > 0 {
        crate::errln!(
            "cache: swept {} of {} entries ({} KiB) from {}",
            s.removed,
            s.scanned,
            s.bytes / 1024,
            root.display()
        );
    }
}

/// The four stores a `cache gc` run walks: the tracing cache, the IR cache,
/// the input store and the roots store.
pub fn stores(cache_dir: Option<&str>) -> Vec<(PathBuf, Shape)> {
    let mut v = Vec::new();
    if let Some(s) = crate::trace::store::Store::locate(cache_dir) {
        v.push((s.root().to_path_buf(), Shape::Dirs));
    }
    if let Some(c) = crate::compile::cache::IrCache::locate(cache_dir, false) {
        v.push((c.root().to_path_buf(), Shape::ShardedFiles));
    }
    if let Some(r) = crate::treestore::locate(cache_dir) {
        v.push((r, Shape::StoreEntries));
    }
    if let Some(r) = roots(cache_dir) {
        v.push((r, Shape::Links));
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ietsp-cachefs-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).expect("temp dir");
        d
    }

    fn back_date(path: &Path, secs: u64) {
        let t = (now_secs() - secs) as i64;
        let times = [
            libc::timespec {
                tv_sec: t,
                tv_nsec: 0,
            },
            libc::timespec {
                tv_sec: t,
                tv_nsec: 0,
            },
        ];
        let c = std::ffi::CString::new(path.as_os_str().as_bytes()).expect("path");
        assert_eq!(
            unsafe { libc::utimensat(libc::AT_FDCWD, c.as_ptr(), times.as_ptr(), 0) },
            0
        );
    }

    fn back_date_link(path: &Path, secs: u64) {
        let t = (now_secs() - secs) as i64;
        let ts = libc::timespec {
            tv_sec: t,
            tv_nsec: 0,
        };
        let times = [ts, ts];
        let c = std::ffi::CString::new(path.as_os_str().as_bytes()).expect("path");
        assert_eq!(
            unsafe {
                libc::utimensat(
                    libc::AT_FDCWD,
                    c.as_ptr(),
                    times.as_ptr(),
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            },
            0
        );
    }

    fn ready(d: &Path) {
        assert!(prepare(d, Shape::Dirs, false), "the store is ietsp's");
    }

    #[test]
    fn version_mismatch_wipes_the_store() {
        let d = tmp("version");
        ready(&d);
        fs::create_dir_all(d.join(OLD)).expect("entry");
        fs::write(d.join(VERSION), b"99").expect("version");
        ready(&d);
        assert!(!d.join(OLD).exists());
        assert_eq!(fs::read(d.join(VERSION)).expect("version"), FORMAT);
    }

    #[test]
    fn garbled_stamp_wipes_the_store() {
        let d = tmp("stamp");
        ready(&d);
        fs::create_dir_all(d.join(OLD)).expect("entry");
        fs::write(d.join(STAMP), b"\x00not a time").expect("stamp");
        ready(&d);
        assert!(!d.join(OLD).exists());
    }

    #[test]
    fn a_store_without_a_version_file_is_wiped_once() {
        let d = tmp("legacy");
        fs::create_dir_all(d.join(OLD)).expect("entry");
        ready(&d);
        assert!(!d.join(OLD).exists());
        ready(&d);
        assert_eq!(fs::read(d.join(VERSION)).expect("version"), FORMAT);
    }

    #[test]
    fn a_directory_that_is_not_ietsps_is_refused_untouched() {
        let d = tmp("foreign");
        fs::write(d.join("unrelated.txt"), b"PRECIOUS").expect("file");
        fs::create_dir_all(d.join("notes")).expect("dir");
        assert!(!prepare(&d, Shape::Dirs, false), "a foreign directory");
        assert_eq!(
            fs::read(d.join("unrelated.txt")).expect("file"),
            b"PRECIOUS"
        );
        assert!(d.join("notes").exists());
        assert!(!d.join(VERSION).exists(), "and nothing is written into it");
    }

    #[test]
    fn a_wipe_keeps_what_ietsp_did_not_create() {
        let d = tmp("scoped");
        ready(&d);
        fs::create_dir_all(d.join(OLD)).expect("entry");
        fs::write(d.join("unrelated.txt"), b"PRECIOUS").expect("file");
        fs::write(d.join(VERSION), b"99").expect("version");
        ready(&d);
        assert!(!d.join(OLD).exists(), "the store starts empty");
        assert_eq!(
            fs::read(d.join("unrelated.txt")).expect("file"),
            b"PRECIOUS"
        );
    }

    #[test]
    fn a_wipe_of_the_ir_cache_walks_its_shards() {
        let d = tmp("wipeshards");
        assert!(prepare(&d, Shape::ShardedFiles, false));
        fs::create_dir_all(d.join("ab")).expect("shard");
        fs::write(d.join("ab").join(&OLD[2..]), b"ir").expect("entry");
        fs::write(d.join("ab").join("keep.txt"), b"PRECIOUS").expect("file");
        fs::write(d.join(VERSION), b"99").expect("version");
        assert!(prepare(&d, Shape::ShardedFiles, false));
        assert!(!d.join("ab").join(&OLD[2..]).exists(), "the entry goes");
        assert_eq!(
            fs::read(d.join("ab").join("keep.txt")).expect("file"),
            b"PRECIOUS",
            "and a foreign file in the shard stays"
        );
    }

    #[test]
    fn an_empty_directory_is_ietsps_to_take() {
        let d = tmp("empty");
        assert!(prepare(&d, Shape::Dirs, false));
        assert_eq!(fs::read(d.join(VERSION)).expect("version"), FORMAT);
    }

    #[test]
    fn the_writer_lock_excludes_one_holder_at_a_time() {
        let d = tmp("lock");
        ready(&d);
        let held = write_lock(&d).expect("first lock");
        assert!(write_lock(&d).is_none());
        drop(held);
        assert!(write_lock(&d).is_some());
    }

    #[test]
    fn a_wait_lock_waits_for_its_holder() {
        let d = tmp("waitlock");
        ready(&d);
        let f = d.join(".x.fetching.tmp");
        let held = wait_lock(&f, || panic!("nobody holds it")).expect("first lock");
        let waited = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (w, f2) = (waited.clone(), f.clone());
        let t = std::thread::spawn(move || {
            wait_lock(&f2, || w.store(true, std::sync::atomic::Ordering::SeqCst)).is_some()
        });
        while !waited.load(std::sync::atomic::Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(1));
        }
        drop(held);
        assert!(t.join().expect("waiter"));
    }

    #[test]
    fn a_touch_inside_the_interval_writes_nothing() {
        let d = tmp("touch");
        ready(&d);
        let f = d.join("entry");
        fs::write(&f, b"x").expect("entry");
        back_date(&f, 60);
        let before = fs::metadata(&f).expect("stat").modified().expect("mtime");
        touch(&f);
        assert_eq!(
            fs::metadata(&f).expect("stat").modified().expect("mtime"),
            before
        );
        back_date(&f, 3 * DAY);
        touch(&f);
        assert!(
            fs::metadata(&f).expect("stat").modified().expect("mtime") > before,
            "an entry older than the interval is touched"
        );
    }

    const OLD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const YOUNG: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    #[test]
    fn sweep_drops_old_entries_and_keeps_young_ones() {
        let d = tmp("sweep");
        ready(&d);
        for (name, secs) in [(OLD, 40 * DAY), (YOUNG, 60)] {
            let e = d.join(name);
            fs::create_dir_all(&e).expect("entry");
            fs::write(e.join("readset"), b"payload").expect("payload");
            back_date(&e, secs);
        }
        fs::create_dir_all(d.join("ir")).expect("a sibling store");
        let stale = d.join(".readset.1.0.tmp");
        fs::write(&stale, b"litter").expect("litter");
        back_date(&stale, 3 * DAY);

        let ttl = Duration::from_secs(TTL_DAYS * DAY);
        let dry = sweep(&d, Shape::Dirs, ttl, true);
        assert_eq!((dry.scanned, dry.removed), (2, 1));
        assert!(
            d.join(OLD).exists() && stale.exists(),
            "dry run writes nothing"
        );

        let done = sweep(&d, Shape::Dirs, ttl, false);
        assert_eq!((done.scanned, done.removed, done.bytes), (2, 1, 7));
        assert!(!d.join(OLD).exists());
        assert!(d.join(YOUNG).exists());
        assert!(d.join("ir").exists(), "only key-shaped names are swept");
        assert!(!stale.exists(), "a killed writer's temporary file goes");
    }

    #[test]
    fn sweep_ages_a_root_by_its_link_not_by_what_it_names() {
        let d = tmp("roots");
        fs::create_dir_all(&d).expect("store");
        // the target carries the store's epoch, as a store path does
        let target = d.join("target");
        fs::create_dir_all(&target).expect("target");
        back_date(&target, 400 * DAY);
        for (name, secs) in [(OLD, 40 * DAY), (YOUNG, 60)] {
            let l = d.join(name);
            std::os::unix::fs::symlink(&target, &l).expect("link");
            back_date_link(&l, secs);
        }
        let s = sweep(&d, Shape::Links, Duration::from_secs(TTL_DAYS * DAY), false);
        assert_eq!((s.scanned, s.removed, s.bytes), (2, 1, 0));
        assert!(!d.join(OLD).exists(), "a root nobody used goes");
        assert!(d.join(YOUNG).exists(), "a root in use stays");
        assert!(
            target.exists(),
            "what a root named is not ietsp's to remove"
        );

        back_date_link(&d.join(YOUNG), 40 * DAY);
        touch_link(&d.join(YOUNG));
        assert_eq!(
            sweep(&d, Shape::Links, Duration::from_secs(TTL_DAYS * DAY), false).removed,
            0,
            "a touched link is young again"
        );
    }

    #[test]
    fn sweep_walks_the_shards_of_the_ir_cache() {
        let d = tmp("shards");
        ready(&d);
        fs::create_dir_all(d.join("ab")).expect("shard");
        for (name, secs) in [(&OLD[2..], 40 * DAY), (&YOUNG[2..], 60)] {
            let f = d.join("ab").join(name);
            fs::write(&f, b"ir").expect("entry");
            back_date(&f, secs);
        }
        let s = sweep(
            &d,
            Shape::ShardedFiles,
            Duration::from_secs(TTL_DAYS * DAY),
            false,
        );
        assert_eq!((s.scanned, s.removed), (2, 1));
        assert!(d.join("ab").join(&YOUNG[2..]).exists());
    }

    #[test]
    fn a_sweep_runs_once_per_interval() {
        let d = tmp("interval");
        ready(&d);
        maybe_sweep(&d, Shape::Dirs, false);
        let stamp = fs::read(d.join(STAMP)).expect("stamp");
        let old = d.join(OLD);
        fs::create_dir_all(&old).expect("entry");
        back_date(&old, 40 * DAY);
        maybe_sweep(&d, Shape::Dirs, false);
        assert!(old.exists(), "the second sweep is inside the interval");
        assert_eq!(fs::read(d.join(STAMP)).expect("stamp"), stamp);
        fs::write(d.join(STAMP), (now_secs() - 2 * DAY).to_string()).expect("stamp");
        maybe_sweep(&d, Shape::Dirs, false);
        assert!(!old.exists());
    }
}

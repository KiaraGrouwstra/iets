//! On-disk hygiene shared by the two caches, the compiled-IR cache
//! (`src/compile/cache.rs`) and the tracing run cache (`src/trace/`): a format
//! version file, a single-writer lock, mtime touches, and TTL sweeps.
//!
//! Many `ietsp` processes share one cache directory (eval-jobs workers,
//! direnv, the classic command names). The invariant they hold together:
//! **every entry file is written to a temporary name and renamed into place,
//! so a reader either sees the whole previous entry or the whole new one and
//! never locks; a writer holds the store's `flock` for exactly one file, and a
//! writer that cannot take it within `LOCK_TIMEOUT` drops the write.** A
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

/// How a store names its entries under its root.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    /// One directory per entry, named by the 64-hex run key (tracing cache).
    Dirs,
    /// One file per entry, named by its key past the two-character shard
    /// directory the key's first two characters make (IR cache).
    ShardedFiles,
    /// One directory per entry, named by a store path's base name (the input
    /// store, `crate::inputs`).
    StoreEntries,
}

/// A sweep only ever removes something named like a key, so the store-level
/// files, the `ir/` store under a shared `--cache-dir`, and anything else a
/// user put in the directory all survive.
fn is_key(name: &std::ffi::OsStr, shape: Shape) -> bool {
    let b = name.as_bytes();
    match shape {
        Shape::StoreEntries => std::str::from_utf8(b)
            .is_ok_and(|n| crate::store::path::StorePath::from_base_name(n).is_ok()),
        Shape::Dirs => b.len() == 64 && b.iter().all(u8::is_ascii_hexdigit),
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

fn stamp_age(bytes: &[u8]) -> Option<Duration> {
    let secs: u64 = std::str::from_utf8(bytes.trim_ascii()).ok()?.parse().ok()?;
    Some(Duration::from_secs(now_secs().saturating_sub(secs)))
}

/// Writes `bytes` to `dir/name` through a temporary file, so a reader sees
/// either the old contents or the new ones. `tag` separates concurrent
/// writers of one name inside one process.
pub fn write_atomic(dir: &Path, name: &str, tag: u64, bytes: &[u8]) -> io::Result<()> {
    let tmp = dir.join(format!(".{name}.{}.{tag}.tmp", std::process::id()));
    match fs::write(&tmp, bytes) {
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
pub fn tag() -> u64 {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

// ---- store-level structure --------------------------------------------------

/// Makes `root` usable. A version mismatch or an unreadable store-level file
/// wipes the store and starts it empty: a cache is never repaired in place.
pub fn prepare(root: &Path, timings: bool) {
    if let Some(why) = broken(root) {
        if timings {
            eprintln!("cache: wiping {} ({why})", root.display());
        }
        let _ = fs::remove_dir_all(root);
    }
    if fs::create_dir_all(root).is_ok() && fs::read(root.join(VERSION)).is_err() {
        let _ = write_atomic(root, VERSION, tag(), FORMAT);
    }
}

fn empty(root: &Path) -> bool {
    fs::read_dir(root).is_ok_and(|mut d| d.next().is_none())
}

fn broken(root: &Path) -> Option<&'static str> {
    if !root.exists() {
        return None;
    }
    match fs::read(root.join(VERSION)) {
        Ok(v) if v.trim_ascii() == FORMAT => {}
        Ok(_) => return Some("format version mismatch"),
        Err(_) if empty(root) => {}
        Err(_) => return Some("no format version"),
    }
    if root.join(LOCK).is_dir() {
        return Some("lock is not a file");
    }
    match fs::read(root.join(STAMP)) {
        Ok(s) if stamp_age(&s).is_none() => Some("unreadable gc stamp"),
        _ => None,
    }
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
        Shape::ShardedFiles => fs::remove_file(path).is_ok(),
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
        let Some(a) = age(&path) else { continue };
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
        Shape::Dirs | Shape::StoreEntries => sweep_dir(root, shape, ttl, dry_run, &mut out),
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
        if write_atomic(root, STAMP, tag(), now_secs().to_string().as_bytes()).is_err() {
            return;
        }
    }
    let s = sweep(root, shape, Duration::from_secs(TTL_DAYS * DAY), false);
    if timings && s.scanned > 0 {
        eprintln!(
            "cache: swept {} of {} entries ({} KiB) from {}",
            s.removed,
            s.scanned,
            s.bytes / 1024,
            root.display()
        );
    }
}

/// The three stores a `cache gc` run walks: the tracing cache, the IR cache and
/// the input store.
pub fn stores(cache_dir: Option<&str>) -> Vec<(PathBuf, Shape)> {
    let mut v = Vec::new();
    if let Some(s) = crate::trace::store::Store::locate(cache_dir) {
        v.push((s.root().to_path_buf(), Shape::Dirs));
    }
    if let Some(c) = crate::compile::cache::IrCache::locate(cache_dir, false) {
        v.push((c.root().to_path_buf(), Shape::ShardedFiles));
    }
    if let Some(r) = crate::inputs::locate(cache_dir) {
        v.push((r, Shape::StoreEntries));
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

    #[test]
    fn version_mismatch_wipes_the_store() {
        let d = tmp("version");
        prepare(&d, false);
        fs::create_dir_all(d.join("aa")).expect("entry");
        fs::write(d.join(VERSION), b"99").expect("version");
        prepare(&d, false);
        assert!(!d.join("aa").exists());
        assert_eq!(fs::read(d.join(VERSION)).expect("version"), FORMAT);
    }

    #[test]
    fn garbled_stamp_wipes_the_store() {
        let d = tmp("stamp");
        prepare(&d, false);
        fs::create_dir_all(d.join("aa")).expect("entry");
        fs::write(d.join(STAMP), b"\x00not a time").expect("stamp");
        prepare(&d, false);
        assert!(!d.join("aa").exists());
    }

    #[test]
    fn a_store_without_a_version_file_is_wiped_once() {
        let d = tmp("legacy");
        fs::create_dir_all(d.join("aa")).expect("entry");
        prepare(&d, false);
        assert!(!d.join("aa").exists());
        prepare(&d, false);
        assert_eq!(fs::read(d.join(VERSION)).expect("version"), FORMAT);
    }

    #[test]
    fn the_writer_lock_excludes_one_holder_at_a_time() {
        let d = tmp("lock");
        prepare(&d, false);
        let held = write_lock(&d).expect("first lock");
        assert!(write_lock(&d).is_none());
        drop(held);
        assert!(write_lock(&d).is_some());
    }

    #[test]
    fn a_wait_lock_waits_for_its_holder() {
        let d = tmp("waitlock");
        prepare(&d, false);
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
        prepare(&d, false);
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
        prepare(&d, false);
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
    fn sweep_walks_the_shards_of_the_ir_cache() {
        let d = tmp("shards");
        prepare(&d, false);
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
        prepare(&d, false);
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

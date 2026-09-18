//! Tracing evaluation cache: the read set an evaluation records, its
//! validation against the file system, and the on-disk entry. Design:
//! docs/tracing-cache.md.

pub mod key;
pub mod run;
pub mod store;
pub mod validate;
pub mod wire;

use std::borrow::Cow;
use std::collections::HashSet;
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use sha2::{Digest, Sha256};

pub type Digest32 = [u8; 32];

/// What a stat saw.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Missing,
    Regular,
    Directory,
    Symlink,
    Other,
}

impl Kind {
    pub fn of(r: &io::Result<fs::Metadata>) -> Kind {
        match r {
            Ok(m) => {
                let ft = m.file_type();
                if ft.is_symlink() {
                    Kind::Symlink
                } else if ft.is_dir() {
                    Kind::Directory
                } else if ft.is_file() {
                    Kind::Regular
                } else {
                    Kind::Other
                }
            }
            Err(_) => Kind::Missing,
        }
    }

    pub fn to_u8(self) -> u8 {
        self as u8
    }

    pub fn from_u8(b: u8) -> Option<Kind> {
        Some(match b {
            0 => Kind::Missing,
            1 => Kind::Regular,
            2 => Kind::Directory,
            3 => Kind::Symlink,
            4 => Kind::Other,
            _ => return None,
        })
    }
}

/// The filter result of a `filterSource`/`builtins.path` copy: every entry
/// the filter was asked about (hashed) and the ones it accepted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Filtered {
    /// `None` when the accepted set came from outside the tree (a git
    /// index), so entries the set does not name cannot change the result.
    pub seen_hash: Option<Digest32>,
    /// Absolute paths, sorted.
    pub accepted: Vec<Vec<u8>>,
}

/// One observation of the outside world.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Obs {
    File {
        path: Vec<u8>,
        size: u64,
        mtime_ns: i64,
        ino: u64,
        hash: Digest32,
    },
    Dir {
        path: Vec<u8>,
        hash: Digest32,
    },
    Stat {
        path: Vec<u8>,
        follow: bool,
        kind: Kind,
    },
    Tree {
        root: Vec<u8>,
        nar_hash: Digest32,
        stat_hash: Digest32,
        filtered: Option<Filtered>,
    },
    Canon {
        path: Vec<u8>,
        target: Option<Vec<u8>>,
    },
    Env {
        name: Vec<u8>,
        value: Option<Vec<u8>>,
    },
    /// A command run in `dir`; `hash` covers its exit status and stdout.
    Cmd {
        dir: Vec<u8>,
        argv: Vec<Vec<u8>>,
        hash: Digest32,
    },
}

impl Obs {
    /// Identity for deduplication: kind and subject, not the observed value.
    fn dedupe_key(&self) -> (u8, Cow<'_, [u8]>, u8) {
        match self {
            Obs::File { path, .. } => (0, Cow::Borrowed(path), 0),
            Obs::Dir { path, .. } => (1, Cow::Borrowed(path), 0),
            Obs::Stat { path, follow, .. } => (2, Cow::Borrowed(path), u8::from(*follow)),
            Obs::Tree { root, filtered, .. } => {
                (3, Cow::Borrowed(root), u8::from(filtered.is_some()))
            }
            Obs::Canon { path, .. } => (4, Cow::Borrowed(path), 0),
            Obs::Env { name, .. } => (5, Cow::Borrowed(name), 0),
            Obs::Cmd { dir, argv, .. } => (6, Cow::Owned(cmd_subject(dir, argv)), 0),
        }
    }
}

fn cmd_subject(dir: &[u8], argv: &[Vec<u8>]) -> Vec<u8> {
    let mut v = dir.to_vec();
    for a in argv {
        v.push(0);
        v.extend_from_slice(a);
    }
    v
}

/// Runs `argv` in `dir` and returns its exit status and stdout; stderr is
/// inherited. `hash` is what a `Cmd` observation records.
pub fn run_cmd(dir: &Path, argv: &[Vec<u8>]) -> io::Result<(i32, Vec<u8>)> {
    let out = std::process::Command::new(std::ffi::OsStr::from_bytes(&argv[0]))
        .args(argv[1..].iter().map(|a| std::ffi::OsStr::from_bytes(a)))
        .current_dir(dir)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit())
        .output()?;
    Ok((out.status.code().unwrap_or(-1), out.stdout))
}

pub fn cmd_hash(status: i32, stdout: &[u8]) -> Digest32 {
    let mut h = Sha256::new();
    h.update(status.to_le_bytes());
    h.update(stdout);
    h.finalize().into()
}

struct Inner {
    obs: Mutex<Vec<Obs>>,
    uncacheable: AtomicBool,
}

/// Recorder handle. Clones share; the default is off and records nothing.
#[derive(Clone, Default)]
pub struct Trace(Option<Arc<Inner>>);

impl Trace {
    pub fn enabled() -> Trace {
        Trace(Some(Arc::new(Inner {
            obs: Mutex::new(Vec::new()),
            uncacheable: AtomicBool::new(false),
        })))
    }

    #[inline]
    pub fn is_on(&self) -> bool {
        self.0.is_some()
    }

    pub fn record(&self, o: Obs) {
        if let Some(i) = &self.0 {
            i.obs.lock().unwrap().push(o);
        }
    }

    pub fn mark_uncacheable(&self) {
        if let Some(i) = &self.0 {
            i.uncacheable.store(true, Ordering::Relaxed);
        }
    }

    pub fn is_cacheable(&self) -> bool {
        self.0
            .as_ref()
            .is_some_and(|i| !i.uncacheable.load(Ordering::Relaxed))
    }

    /// Every observation, deduplicated on (kind, subject); two observations
    /// of one subject with different values are both kept.
    pub fn take(&self) -> Vec<Obs> {
        let Some(i) = &self.0 else {
            return Vec::new();
        };
        let all = std::mem::take(&mut *i.obs.lock().unwrap());
        let mut seen: HashSet<(u8, Cow<'_, [u8]>, u8)> = HashSet::with_capacity(all.len());
        let mut exact: HashSet<Vec<u8>> = HashSet::new();
        let mut keep = vec![false; all.len()];
        for (idx, o) in all.iter().enumerate() {
            keep[idx] = seen.insert(o.dedupe_key())
                || (exact.insert(wire::encode_obs(o)) && !all[..idx].iter().any(|p| p == o));
        }
        all.into_iter()
            .zip(keep)
            .filter_map(|(o, k)| k.then_some(o))
            .collect()
    }

    // ---- recording helpers ----------------------------------------------

    /// A regular file whose bytes are in memory.
    pub fn file(&self, path: &[u8], meta: &fs::Metadata, bytes: &[u8]) {
        if self.is_on() {
            self.record(file_obs(path, meta, sha256(bytes)));
        }
    }

    pub fn stat(&self, path: &[u8], follow: bool, kind: Kind) {
        if self.is_on() {
            self.record(Obs::Stat {
                path: path.to_vec(),
                follow,
                kind,
            });
        }
    }

    /// Stats `path` and records what it saw; returns the kind.
    pub fn stat_now(&self, path: &[u8], follow: bool) -> Kind {
        let kind = stat_kind(Path::new(std::ffi::OsStr::from_bytes(path)), follow);
        self.stat(path, follow, kind);
        kind
    }

    pub fn env(&self, name: &[u8]) {
        if self.is_on() {
            let v = std::env::var_os(std::ffi::OsStr::from_bytes(name));
            self.record(Obs::Env {
                name: name.to_vec(),
                value: v.map(|v| v.as_bytes().to_vec()),
            });
        }
    }

    /// Runs `argv` in `dir`, records what it printed, returns (status, stdout).
    pub fn cmd(&self, dir: &Path, argv: &[Vec<u8>]) -> io::Result<(i32, Vec<u8>)> {
        let (status, stdout) = run_cmd(dir, argv)?;
        if self.is_on() {
            self.record(Obs::Cmd {
                dir: dir.as_os_str().as_bytes().to_vec(),
                argv: argv.to_vec(),
                hash: cmd_hash(status, &stdout),
            });
        }
        Ok((status, stdout))
    }

    pub fn canon(&self, path: &[u8], target: Option<&Path>) {
        if self.is_on() {
            self.record(Obs::Canon {
                path: path.to_vec(),
                target: target.map(|t| t.as_os_str().as_bytes().to_vec()),
            });
        }
    }

    /// A tree copied to the store. `accepted` is the filter result, `None`
    /// for an unfiltered copy. Walks the tree once more with `lstat` only.
    pub fn tree(&self, root: &Path, nar_hash: Digest32, accepted: Option<&HashSet<PathBuf>>) {
        self.tree_with(root, nar_hash, accepted, true);
    }

    /// A tree whose accepted set comes from outside (a git index): only the
    /// accepted entries matter, so no `seen_hash` is kept.
    pub fn tree_external(&self, root: &Path, nar_hash: Digest32, accepted: &HashSet<PathBuf>) {
        self.tree_with(root, nar_hash, Some(accepted), false);
    }

    fn tree_with(
        &self,
        root: &Path,
        nar_hash: Digest32,
        accepted: Option<&HashSet<PathBuf>>,
        keep_seen: bool,
    ) {
        if !self.is_on() {
            return;
        }
        let Ok((stat_hash, seen_hash)) = tree_fingerprint(root, accepted) else {
            self.mark_uncacheable();
            return;
        };
        let filtered = accepted.map(|a| {
            let mut accepted: Vec<Vec<u8>> = a
                .iter()
                .map(|p| p.as_os_str().as_bytes().to_vec())
                .collect();
            accepted.sort();
            Filtered {
                seen_hash: keep_seen.then_some(seen_hash),
                accepted,
            }
        });
        self.record(Obs::Tree {
            root: root.as_os_str().as_bytes().to_vec(),
            nar_hash,
            stat_hash,
            filtered,
        });
    }
}

pub fn sha256(bytes: &[u8]) -> Digest32 {
    Sha256::digest(bytes).into()
}

pub fn mtime_ns(m: &fs::Metadata) -> i64 {
    m.mtime()
        .wrapping_mul(1_000_000_000)
        .wrapping_add(m.mtime_nsec())
}

pub fn file_obs(path: &[u8], meta: &fs::Metadata, hash: Digest32) -> Obs {
    Obs::File {
        path: path.to_vec(),
        size: meta.len(),
        mtime_ns: mtime_ns(meta),
        ino: meta.ino(),
        hash,
    }
}

pub fn stat_kind(p: &Path, follow: bool) -> Kind {
    let r = if follow {
        fs::metadata(p)
    } else {
        fs::symlink_metadata(p)
    };
    Kind::of(&r)
}

/// sha256 of the sorted `(name, type)` listing of a directory, as `readDir`
/// reports it.
pub fn dir_listing_hash(entries: &mut [(Vec<u8>, Kind)]) -> Digest32 {
    entries.sort();
    let mut h = Sha256::new();
    for (name, kind) in entries.iter() {
        h.update((name.len() as u64).to_le_bytes());
        h.update(name);
        h.update([kind.to_u8()]);
    }
    h.finalize().into()
}

pub fn read_dir_kinds(p: &Path) -> io::Result<Vec<(Vec<u8>, Kind)>> {
    let mut out = Vec::new();
    for e in fs::read_dir(p)? {
        let e = e?;
        let ft = e.file_type()?;
        let kind = if ft.is_symlink() {
            Kind::Symlink
        } else if ft.is_dir() {
            Kind::Directory
        } else if ft.is_file() {
            Kind::Regular
        } else {
            Kind::Other
        };
        out.push((e.file_name().as_bytes().to_vec(), kind));
    }
    Ok(out)
}

/// The `accepted` filter of a recorded copy, or accept-all.
fn accepts(accepted: Option<&HashSet<PathBuf>>, p: &Path) -> bool {
    accepted.is_none_or(|a| a.contains(p))
}

/// Walks `root` like the NAR dump does (sorted entries, no descent into
/// rejected directories) and hashes what `lstat` reports for every accepted
/// entry (`stat_hash`) and the `(relative path, type)` of every entry the
/// filter saw (`seen_hash`). No file contents are read.
pub fn tree_fingerprint(
    root: &Path,
    accepted: Option<&HashSet<PathBuf>>,
) -> io::Result<(Digest32, Digest32)> {
    let mut stat_h = Sha256::new();
    let mut seen_h = Sha256::new();
    walk_fp(root, b"", accepted, &mut stat_h, &mut seen_h, 0)?;
    Ok((stat_h.finalize().into(), seen_h.finalize().into()))
}

fn walk_fp(
    path: &Path,
    rel: &[u8],
    accepted: Option<&HashSet<PathBuf>>,
    stat_h: &mut Sha256,
    seen_h: &mut Sha256,
    depth: usize,
) -> io::Result<()> {
    if depth > 64 {
        return Err(io::Error::other("tree too deep"));
    }
    let st = fs::symlink_metadata(path)?;
    let kind = Kind::of(&Ok(st.clone()));
    stat_h.update((rel.len() as u64).to_le_bytes());
    stat_h.update(rel);
    stat_h.update([kind.to_u8()]);
    stat_h.update(st.len().to_le_bytes());
    stat_h.update(mtime_ns(&st).to_le_bytes());
    stat_h.update(st.ino().to_le_bytes());
    stat_h.update((st.mode() & 0o100).to_le_bytes());
    match kind {
        Kind::Symlink => {
            let t = fs::read_link(path)?;
            stat_h.update(t.as_os_str().as_bytes());
        }
        Kind::Directory => {
            let mut entries = read_dir_kinds(path)?;
            entries.sort();
            for (name, ekind) in entries {
                let child = path.join(std::ffi::OsStr::from_bytes(&name));
                let mut crel = rel.to_vec();
                crel.push(b'/');
                crel.extend_from_slice(&name);
                seen_h.update((crel.len() as u64).to_le_bytes());
                seen_h.update(&crel);
                seen_h.update([ekind.to_u8()]);
                if accepts(accepted, &child) {
                    walk_fp(&child, &crel, accepted, stat_h, seen_h, depth + 1)?;
                }
            }
        }
        _ => {}
    }
    Ok(())
}

/// NAR hash of `root` under the recorded filter, plus the `seen_hash` of the
/// walk, for the slow path of `Tree` validation.
pub fn tree_rehash(
    root: &Path,
    accepted: Option<&HashSet<PathBuf>>,
) -> io::Result<(Digest32, Digest32)> {
    use crate::store::hash::HashAlgo;
    let mut seen_h = Sha256::new();
    let root_len = root.as_os_str().as_bytes().len();
    let mut filter = |p: &Path, t: crate::store::nar::FileType| -> bool {
        let rel = &p.as_os_str().as_bytes()[root_len..];
        seen_h.update((rel.len() as u64).to_le_bytes());
        seen_h.update(rel);
        seen_h.update([nar_kind(t).to_u8()]);
        accepts(accepted, p)
    };
    let (h, _) = crate::store::nar::nar_hash_filtered(root, HashAlgo::Sha256, &mut filter)?;
    let mut nar = [0u8; 32];
    nar.copy_from_slice(h.bytes());
    Ok((nar, seen_h.finalize().into()))
}

fn nar_kind(t: crate::store::nar::FileType) -> Kind {
    use crate::store::nar::FileType as F;
    match t {
        F::Regular => Kind::Regular,
        F::Directory => Kind::Directory,
        F::Symlink => Kind::Symlink,
        F::Unknown => Kind::Other,
    }
}

pub fn hash_to_digest(h: &crate::store::hash::Hash) -> Digest32 {
    let mut d = [0u8; 32];
    d.copy_from_slice(h.bytes());
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedupe_keeps_conflicting_values() {
        let t = Trace::enabled();
        let o1 = Obs::Env {
            name: b"A".to_vec(),
            value: Some(b"1".to_vec()),
        };
        let o2 = Obs::Env {
            name: b"A".to_vec(),
            value: Some(b"2".to_vec()),
        };
        t.record(o1.clone());
        t.record(o1.clone());
        t.record(o2.clone());
        t.record(o2.clone());
        let v = t.take();
        assert_eq!(v, vec![o1, o2]);
    }

    #[test]
    fn fingerprint_changes_with_content_size_and_seen_entries() {
        let dir = crate::store::nar::tests::fixture_tree("trace-fp");
        let (a, sa) = tree_fingerprint(&dir, None).unwrap();
        let (b, sb) = tree_fingerprint(&dir, None).unwrap();
        assert_eq!((a, sa), (b, sb));
        std::fs::write(dir.join("new-file"), b"x").unwrap();
        let (c, sc) = tree_fingerprint(&dir, None).unwrap();
        assert_ne!(a, c);
        assert_ne!(sa, sc);
        let (nar, seen) = tree_rehash(&dir, None).unwrap();
        assert_eq!(seen, sc);
        assert_eq!(
            nar,
            hash_to_digest(&crate::store::nar::nar_hash(&dir).unwrap())
        );
    }
}

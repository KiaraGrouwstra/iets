//! Checks a recorded read set against the world, on a few threads. Any
//! mismatch is a miss. Stat-only checks come first; a file whose stat
//! changed is rehashed and accepted when its content did not.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use super::{Obs, dir_listing_hash, mtime_ns, read_dir_kinds, stat_kind};

#[derive(Debug, Default)]
pub struct Report {
    pub entries: usize,
    /// Files and trees whose contents were rehashed because their stat changed.
    pub rehashed: usize,
    /// Entries whose stat fields were refreshed; the read set should be rewritten.
    pub refreshed: usize,
}

/// The first observation that no longer holds, for `--timings`.
#[derive(Debug)]
pub struct Mismatch(pub String);

enum Outcome {
    Same,
    /// Content still matches; the entry's stat fields are stale.
    Refresh(Obs),
    Changed(String),
}

fn os(p: &[u8]) -> &Path {
    Path::new(OsStr::from_bytes(p))
}

fn lossy(p: &[u8]) -> String {
    String::from_utf8_lossy(p).into_owned()
}

/// sha256 of a file's bytes without holding them: a rehash pass over a big `nixpkgs` file
/// would otherwise allocate the whole file to read it once.
fn file_sha256(p: &Path) -> std::io::Result<super::Digest32> {
    use crate::store::hash::{HashAlgo, Hasher};
    let mut h = Hasher::new(HashAlgo::Sha256);
    std::io::copy(&mut fs::File::open(p)?, &mut h)?;
    Ok(super::hash_to_digest(&h.finish()))
}

fn check(o: &Obs, rehashed: &AtomicUsize) -> Outcome {
    match o {
        Obs::File {
            path,
            size,
            mtime_ns: mt,
            ino,
            hash,
        } => {
            let Ok(m) = fs::metadata(os(path)) else {
                return Outcome::Changed(format!("{} is gone", lossy(path)));
            };
            if !m.is_file() {
                return Outcome::Changed(format!("{} is not a regular file", lossy(path)));
            }
            if m.len() == *size && mtime_ns(&m) == *mt && m.ino() == *ino {
                return Outcome::Same;
            }
            rehashed.fetch_add(1, Ordering::Relaxed);
            let Ok(now) = file_sha256(os(path)) else {
                return Outcome::Changed(format!("{} is unreadable", lossy(path)));
            };
            if now != *hash {
                return Outcome::Changed(format!("{} changed", lossy(path)));
            }
            Outcome::Refresh(super::file_obs(path, &m, *hash))
        }
        Obs::Dir { path, hash } => match read_dir_kinds(os(path)) {
            Ok(mut entries) => {
                if dir_listing_hash(&mut entries) == *hash {
                    Outcome::Same
                } else {
                    Outcome::Changed(format!("directory {} changed", lossy(path)))
                }
            }
            Err(_) => Outcome::Changed(format!("directory {} is gone", lossy(path))),
        },
        Obs::Stat { path, follow, kind } => {
            if stat_kind(os(path), *follow) == *kind {
                Outcome::Same
            } else {
                Outcome::Changed(format!("{} changed type or existence", lossy(path)))
            }
        }
        Obs::Tree {
            root,
            nar_hash,
            stat_hash,
            filtered,
        } => {
            let accepted: Option<HashSet<PathBuf>> = filtered.as_ref().map(|f| {
                f.accepted
                    .iter()
                    .map(|p| PathBuf::from(OsStr::from_bytes(p)))
                    .collect()
            });
            let root_p = os(root);
            match super::tree_fingerprint(root_p, accepted.as_ref()) {
                Ok((sh, _)) if sh == *stat_hash => return Outcome::Same,
                Ok(_) => {}
                Err(_) => return Outcome::Changed(format!("tree {} is gone", lossy(root))),
            }
            rehashed.fetch_add(1, Ordering::Relaxed);
            let Ok((nar, seen, sh)) = super::tree_rehash(root_p, accepted.as_ref()) else {
                return Outcome::Changed(format!("tree {} is unreadable", lossy(root)));
            };
            if nar != *nar_hash
                || filtered
                    .as_ref()
                    .is_some_and(|f| f.seen_hash.is_some_and(|s| s != seen))
            {
                return Outcome::Changed(format!("tree {} changed", lossy(root)));
            }
            Outcome::Refresh(Obs::Tree {
                root: root.clone(),
                nar_hash: *nar_hash,
                stat_hash: sh,
                filtered: filtered.clone(),
            })
        }
        Obs::Canon { path, target } => {
            let now = fs::canonicalize(os(path))
                .ok()
                .map(|p| p.as_os_str().as_bytes().to_vec());
            if now == *target {
                Outcome::Same
            } else {
                Outcome::Changed(format!("{} resolves elsewhere", lossy(path)))
            }
        }
        Obs::Env { name, value } => {
            let now = std::env::var_os(OsStr::from_bytes(name)).map(|v| v.as_bytes().to_vec());
            if now == *value {
                Outcome::Same
            } else {
                Outcome::Changed(format!("${} changed", lossy(name)))
            }
        }
        Obs::Cmd { dir, argv, hash } => match super::run_cmd(os(dir), argv) {
            Ok((status, out)) if super::cmd_hash(status, &out) == *hash => Outcome::Same,
            _ => Outcome::Changed(format!(
                "output of `{}` in {} changed",
                argv.iter().map(|a| lossy(a)).collect::<Vec<_>>().join(" "),
                lossy(dir)
            )),
        },
    }
}

/// Validates every observation on `threads` threads. On success the
/// refreshed entries are written back into `obs`.
pub fn validate(obs: &mut [Obs], threads: usize) -> Result<Report, Mismatch> {
    let next = AtomicUsize::new(0);
    let failed = AtomicBool::new(false);
    let rehashed = AtomicUsize::new(0);
    let first: Mutex<Option<String>> = Mutex::new(None);
    let refreshes: Mutex<Vec<(usize, Obs)>> = Mutex::new(Vec::new());
    let threads = threads.clamp(1, obs.len().max(1));
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                let mut mine = Vec::new();
                loop {
                    if failed.load(Ordering::Relaxed) {
                        break;
                    }
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= obs.len() {
                        break;
                    }
                    match check(&obs[i], &rehashed) {
                        Outcome::Same => {}
                        Outcome::Refresh(o) => mine.push((i, o)),
                        Outcome::Changed(why) => {
                            failed.store(true, Ordering::Relaxed);
                            first.lock().unwrap().get_or_insert(why);
                            break;
                        }
                    }
                }
                refreshes.lock().unwrap().extend(mine);
            });
        }
    });
    if failed.load(Ordering::Relaxed) {
        return Err(Mismatch(first.into_inner().unwrap().unwrap_or_default()));
    }
    let refreshes = refreshes.into_inner().unwrap();
    let refreshed = refreshes.len();
    for (i, o) in refreshes {
        obs[i] = o;
    }
    Ok(Report {
        entries: obs.len(),
        rehashed: rehashed.into_inner(),
        refreshed,
    })
}

#[cfg(test)]
mod tests {
    use super::super::{Kind, sha256};
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ietsp-trace-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn file_touch_refreshes_edit_misses() {
        let d = tmp("file");
        let f = d.join("a.nix");
        fs::write(&f, b"1").unwrap();
        let m = fs::metadata(&f).unwrap();
        let p = f.as_os_str().as_bytes().to_vec();
        let mut obs = vec![super::super::file_obs(&p, &m, sha256(b"1"))];
        let r = validate(&mut obs, 2).unwrap();
        assert_eq!((r.rehashed, r.refreshed), (0, 0));

        // same content, new mtime: rehash once, refresh
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
        fs::File::open(&f).unwrap().set_modified(later).unwrap();
        let r = validate(&mut obs, 2).unwrap();
        assert_eq!((r.rehashed, r.refreshed), (1, 1));
        let r = validate(&mut obs, 2).unwrap();
        assert_eq!((r.rehashed, r.refreshed), (0, 0));

        fs::write(&f, b"2").unwrap();
        assert!(validate(&mut obs, 2).is_err());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn dir_stat_env_checks() {
        let d = tmp("dir");
        fs::write(d.join("x"), b"").unwrap();
        let p = d.as_os_str().as_bytes().to_vec();
        let mut entries = read_dir_kinds(&d).unwrap();
        let mut obs = vec![
            Obs::Dir {
                path: p.clone(),
                hash: dir_listing_hash(&mut entries),
            },
            Obs::Stat {
                path: p.clone(),
                follow: true,
                kind: Kind::Directory,
            },
            Obs::Env {
                name: b"IETSP_TRACE_TEST_UNSET".to_vec(),
                value: None,
            },
        ];
        validate(&mut obs, 3).unwrap();
        fs::write(d.join("y"), b"").unwrap();
        assert!(validate(&mut obs, 3).is_err());
        let _ = fs::remove_dir_all(&d);
    }
}

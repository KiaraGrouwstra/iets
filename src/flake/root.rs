//! The root flake's `sourceInfo`. A git working tree is copied the way
//! git.cc:getAccessorFromWorkdir does it: the files git tracks (index
//! entries that still exist), with the working tree contents and modes;
//! nothing untracked, no `.git`, no submodules. A plain directory is copied
//! whole (path.cc). The copy is virtual: the store path is computed from
//! the NAR hash and registered as a `SourceOrigin::Tree`, so reads under it
//! go to the working tree (`store_hooks::locate`); nothing is fetched and
//! the store is never consulted. Every git query is a `Cmd` observation and
//! the copied file set a `Tree` observation, so the tracing cache follows
//! the tree.

use std::collections::HashSet;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::Source;
use super::fetch::{AttrVal, Attrs, emit_tree_attrs, source_store_path};
use crate::eval::{Error, ErrorKind, EvalResult, Evaluator, Pos, bail};
use crate::runtime::Word;
use crate::store::hash::{Hash, HashAlgo};
use crate::store::nar;
use crate::store::source::SourceOrigin;

const NULL_REV: &str = "0000000000000000000000000000000000000000";

/// The sourceInfo attrset and the flake's subdirectory inside it.
pub fn source_info(ev: &mut Evaluator, source: &Source, pos: Pos) -> EvalResult<(Word, String)> {
    match source {
        Source::Path(dir) => {
            let (hash, mtime) = path_tree(ev, dir, pos)?;
            let mut attrs = Attrs::new();
            attrs.insert("type".into(), AttrVal::Str("path".into()));
            attrs.insert(
                "path".into(),
                AttrVal::Str(dir.to_string_lossy().into_owned()),
            );
            attrs.insert("narHash".into(), AttrVal::Str(hash.to_sri()));
            attrs.insert("lastModified".into(), AttrVal::Int(mtime.max(0) as u64));
            let sp = source_store_path(&hash)
                .map_err(|e| Error::boxed(ErrorKind::Eval, e.to_string(), pos))?;
            Ok((emit_tree_attrs(ev, &sp, &attrs, pos)?, String::new()))
        }
        Source::Git { root, subdir } => Ok((git_tree(ev, root, pos)?, subdir.clone())),
    }
}

/// NAR hash of a whole directory and the newest `lstat` mtime in it, like
/// `dumpPathAndGetMtime`.
pub fn path_tree(ev: &mut Evaluator, dir: &Path, pos: Pos) -> EvalResult<(Hash, i64)> {
    let io = |e: std::io::Error| {
        Error::boxed(
            ErrorKind::Io,
            format!("copying '{}' to the store: {e}", dir.display()),
            pos,
        )
    };
    let (hash, nar_size) =
        nar::nar_hash_filtered(dir, HashAlgo::Sha256, &mut |_, _| true).map_err(io)?;
    let mtime = max_mtime(dir).map_err(io)?;
    ev.trace
        .tree(dir, crate::trace::hash_to_digest(&hash), None);
    let sp =
        source_store_path(&hash).map_err(|e| Error::boxed(ErrorKind::Eval, e.to_string(), pos))?;
    ev.sources.insert(
        sp,
        SourceOrigin::Tree {
            local: dir.to_path_buf(),
            accepted: None,
            nar_hash: hash,
            nar_size,
        },
    );
    Ok((hash, mtime))
}

fn max_mtime(p: &Path) -> std::io::Result<i64> {
    let st = std::fs::symlink_metadata(p)?;
    let mut m = st.mtime();
    if st.is_dir() {
        for e in std::fs::read_dir(p)? {
            m = m.max(max_mtime(&e?.path())?);
        }
    }
    Ok(m)
}

fn git(ev: &mut Evaluator, root: &Path, args: &[&str], pos: Pos) -> EvalResult<Vec<u8>> {
    let mut argv: Vec<Vec<u8>> = vec![b"git".to_vec()];
    argv.extend(args.iter().map(|a| a.as_bytes().to_vec()));
    let (status, out) = ev
        .trace
        .cmd(root, &argv)
        .map_err(|e| Error::boxed(ErrorKind::Io, format!("ietsp: cannot run git: {e}"), pos))?;
    if status != 0 {
        bail!(
            ErrorKind::Io,
            pos,
            "ietsp: `git {}` failed in '{}' (exit {status})",
            args.join(" "),
            root.display()
        );
    }
    Ok(out)
}

/// git-utils.cc:getWorkdirInfo: index entries minus deleted ones; dirty when
/// any tracked file differs from HEAD or the index.
pub struct Workdir {
    pub files: HashSet<PathBuf>,
    pub dirty: bool,
    pub head: Option<String>,
}

pub fn workdir(ev: &mut Evaluator, root: &Path, pos: Pos) -> EvalResult<Workdir> {
    let ls = git(ev, root, &["ls-files", "-z", "--cached", "--stage"], pos)?;
    let mut files: HashSet<PathBuf> = HashSet::new();
    for entry in ls.split(|&b| b == 0).filter(|e| !e.is_empty()) {
        let Some(tab) = entry.iter().position(|&b| b == b'\t') else {
            continue;
        };
        let mode = entry[..tab].split(|&b| b == b' ').next().unwrap_or(b"");
        if mode == b"160000" {
            continue;
        }
        files.insert(root.join(std::ffi::OsStr::from_bytes(&entry[tab + 1..])));
    }
    let status = git(
        ev,
        root,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=no",
            "--no-renames",
            "--ignore-submodules=all",
        ],
        pos,
    )?;
    let mut dirty = false;
    for entry in status.split(|&b| b == 0).filter(|e| e.len() > 3) {
        dirty = true;
        if entry[0] == b'D' || entry[1] == b'D' {
            files.remove(&root.join(std::ffi::OsStr::from_bytes(&entry[3..])));
        }
    }
    let head = ev
        .trace
        .cmd(
            root,
            &["git", "rev-parse", "--verify", "-q", "HEAD"].map(|s| s.as_bytes().to_vec()),
        )
        .map_err(|e| Error::boxed(ErrorKind::Io, format!("ietsp: cannot run git: {e}"), pos))?;
    let head = (head.0 == 0).then(|| String::from_utf8_lossy(&head.1).trim().to_string());
    Ok(Workdir { files, dirty, head })
}

/// The filter set for the NAR dump: every tracked file and the directories
/// above it (CanonPath::isAllowed on the allow-list accessor).
pub fn accepted_set(root: &Path, files: &HashSet<PathBuf>) -> HashSet<PathBuf> {
    let mut acc = HashSet::with_capacity(files.len() * 2);
    for f in files {
        let mut p = f.as_path();
        while p != root && acc.insert(p.to_path_buf()) {
            match p.parent() {
                Some(q) => p = q,
                None => break,
            }
        }
    }
    acc
}

fn git_tree(ev: &mut Evaluator, root: &Path, pos: Pos) -> EvalResult<Word> {
    let wd = workdir(ev, root, pos)?;
    let accepted = accepted_set(root, &wd.files);
    let mut filter = |p: &Path, _: nar::FileType| accepted.contains(p);
    let (hash, nar_size) =
        nar::nar_hash_filtered(root, HashAlgo::Sha256, &mut filter).map_err(|e| {
            Error::boxed(
                ErrorKind::Io,
                format!("copying '{}' to the store: {e}", root.display()),
                pos,
            )
        })?;
    ev.trace
        .tree_external(root, crate::trace::hash_to_digest(&hash), &accepted);
    let sp =
        source_store_path(&hash).map_err(|e| Error::boxed(ErrorKind::Eval, e.to_string(), pos))?;
    ev.sources.insert(
        sp.clone(),
        SourceOrigin::Tree {
            local: root.to_path_buf(),
            accepted: Some(Arc::new(accepted)),
            nar_hash: hash,
            nar_size,
        },
    );
    if let Some(h) = &wd.head {
        let rev = if wd.dirty {
            format!("{h}-dirty")
        } else {
            h.clone()
        };
        crate::provenance::record_rev(&sp.to_string(), &rev);
    }

    let mut attrs = Attrs::new();
    attrs.insert("type".into(), AttrVal::Str("git".into()));
    attrs.insert(
        "url".into(),
        AttrVal::Str(format!("file://{}", root.to_string_lossy())),
    );
    attrs.insert("narHash".into(), AttrVal::Str(hash.to_sri()));
    let last_modified = match &wd.head {
        Some(_) => {
            let t = git(ev, root, &["log", "-1", "--format=%ct", "HEAD"], pos)?;
            String::from_utf8_lossy(&t)
                .trim()
                .parse::<u64>()
                .unwrap_or(0)
        }
        None => 0,
    };
    attrs.insert("lastModified".into(), AttrVal::Int(last_modified));
    if wd.dirty {
        if let Some(h) = &wd.head {
            attrs.insert("dirtyRev".into(), AttrVal::Str(format!("{h}-dirty")));
            attrs.insert(
                "dirtyShortRev".into(),
                AttrVal::Str(format!("{}-dirty", &h[..h.len().min(7)])),
            );
        }
    } else {
        let rev = wd.head.clone().unwrap_or_else(|| NULL_REV.to_string());
        attrs.insert("rev".into(), AttrVal::Str(rev.clone()));
        let shallow_marker = root.join(".git/shallow");
        let shallow = ev
            .trace
            .stat_now(shallow_marker.as_os_str().as_bytes(), true)
            != crate::trace::Kind::Missing;
        if !shallow {
            let n = if rev == NULL_REV {
                0
            } else {
                let c = git(ev, root, &["rev-list", "--count", "HEAD"], pos)?;
                String::from_utf8_lossy(&c)
                    .trim()
                    .parse::<u64>()
                    .unwrap_or(0)
            };
            attrs.insert("revCount".into(), AttrVal::Int(n));
        }
    }
    emit_tree_attrs(ev, &sp, &attrs, pos)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepted_set_adds_parent_directories() {
        let root = PathBuf::from("/r");
        let files: HashSet<PathBuf> = ["/r/a/b/c.nix", "/r/d.nix"].map(PathBuf::from).into();
        let acc = accepted_set(&root, &files);
        let mut v: Vec<&str> = acc.iter().map(|p| p.to_str().unwrap()).collect();
        v.sort();
        assert_eq!(v, ["/r/a", "/r/a/b", "/r/a/b/c.nix", "/r/d.nix"]);
    }

    #[test]
    fn max_mtime_covers_root_and_children() {
        let d = crate::store::nar::tests::fixture_tree("mtime");
        let f = d.join("newest");
        std::fs::write(&f, b"x").unwrap();
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(1000);
        std::fs::File::open(&f)
            .unwrap()
            .set_modified(later)
            .unwrap();
        let want = std::fs::metadata(&f).unwrap().mtime();
        assert_eq!(max_mtime(&d).unwrap(), want);
    }
}

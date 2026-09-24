//! Unpacking a downloaded tarball the way Nix does, so the NAR of the result
//! has the `narHash` the lock file names.
//!
//! tarfile.cc:unpackTarfileToSink walks the archive and feeds each member to a
//! sink; git-utils.cc:GitFileSystemObjectSinkImpl builds a git tree from them,
//! and tarball.cc:downloadTarball_ then calls
//! `dereferenceSingletonDirectory` before `treeHashToNarHash` hashes the tree.
//! What survives into the hash is therefore: directories (empty ones included,
//! since the sink writes an empty tree object for them), regular files with one
//! bit of mode (`archive_entry_mode() & S_IXUSR`, tarfile.cc:226), symlink
//! targets, and hard links resolved to a second entry with the target's blob
//! and mode (git-utils.cc:1348-1364). Everything else is an error
//! (tarfile.cc:253). A member listed twice keeps the last copy
//! (git-utils.cc:1213, "only import the latest version of a path").

use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};

use crate::error::{Error, Result};

fn bad(why: String) -> Error {
    Error::Store(why)
}

/// `std::filesystem::path(name).relative_path()` plus libarchive's
/// `ARCHIVE_EXTRACT_SECURE_NODOTDOT` (tarfile.cc:135, 156): the member path with
/// any root stripped, `.` dropped, and `..` refused.
pub(crate) fn member_path(name: &Path) -> Result<PathBuf> {
    let mut out = PathBuf::new();
    for c in name.components() {
        match c {
            Component::Normal(p) => out.push(p),
            Component::CurDir | Component::RootDir | Component::Prefix(_) => {}
            Component::ParentDir => {
                return Err(bad(format!(
                    "tarball member '{}' escapes the archive",
                    name.display()
                )));
            }
        }
    }
    Ok(out)
}

/// `root` joined with a checked member path, with the missing parents created.
/// A parent that exists and is not a directory is replaced, and one reached
/// through a symlink is refused (`ARCHIVE_EXTRACT_SECURE_SYMLINKS`).
fn make_parents(root: &Path, rel: &Path) -> Result<PathBuf> {
    let mut at = root.to_path_buf();
    let n = rel.components().count();
    for (i, c) in rel.components().enumerate() {
        at.push(c);
        if i + 1 == n {
            break;
        }
        match std::fs::symlink_metadata(&at) {
            Ok(m) if m.is_dir() => {}
            Ok(m) if m.is_symlink() => {
                return Err(bad(format!(
                    "tarball member '{}' is under the symlink '{}'",
                    rel.display(),
                    at.display()
                )));
            }
            Ok(_) => {
                std::fs::remove_file(&at)
                    .map_err(|e| Error::io(format!("cannot replace {}", at.display()), e))?;
                mkdir(&at)?;
            }
            Err(_) => mkdir(&at)?,
        }
    }
    Ok(at)
}

fn mkdir(at: &Path) -> Result<()> {
    std::fs::create_dir(at).map_err(|e| Error::io(format!("cannot create {}", at.display()), e))
}

/// Whatever is at `at` goes away, whichever type it has.
fn clear(at: &Path) -> Result<()> {
    match std::fs::symlink_metadata(at) {
        Err(_) => Ok(()),
        Ok(m) if m.is_dir() => Ok(()),
        Ok(_) => std::fs::remove_file(at)
            .map_err(|e| Error::io(format!("cannot replace {}", at.display()), e)),
    }
}

/// Unpacks the tar stream in `bytes` under `dest` and answers the directory to
/// hash: `dest`, or its single entry when that is the only one and is a
/// directory (git-utils.cc:dereferenceSingletonDirectory).
pub fn unpack(bytes: Vec<u8>, dest: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(dest)
        .map_err(|e| Error::io(format!("cannot create {}", dest.display()), e))?;
    let mut links: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut archive = tar::Archive::new(std::io::Cursor::new(bytes));
    let entries = archive
        .entries()
        .map_err(|e| Error::io("cannot read the tarball", e))?;
    for entry in entries {
        let mut entry = entry.map_err(|e| Error::io("cannot read the tarball", e))?;
        let kind = entry.header().entry_type();
        // libarchive reads the pax global header as archive metadata, not as a
        // member; GitHub's tarballs start with one.
        if kind.is_pax_global_extensions() {
            continue;
        }
        let name = entry
            .path()
            .map_err(|e| Error::io("cannot read a tarball member name", e))?
            .into_owned();
        let rel = member_path(&name)?;
        if rel.as_os_str().is_empty() {
            continue;
        }
        let at = make_parents(dest, &rel)?;
        if kind.is_dir() {
            if std::fs::symlink_metadata(&at).is_ok_and(|m| !m.is_dir()) {
                std::fs::remove_file(&at)
                    .map_err(|e| Error::io(format!("cannot replace {}", at.display()), e))?;
            }
            if !at.is_dir() {
                mkdir(&at)?;
            }
        } else if kind.is_symlink() {
            let target = entry
                .link_name()
                .map_err(|e| Error::io("cannot read a symlink target", e))?
                .ok_or_else(|| bad(format!("tarball symlink '{}' has no target", rel.display())))?
                .into_owned();
            clear(&at)?;
            std::os::unix::fs::symlink(&target, &at)
                .map_err(|e| Error::io(format!("cannot create {}", at.display()), e))?;
        } else if kind.is_hard_link() {
            let target = entry
                .link_name()
                .map_err(|e| Error::io("cannot read a hard link target", e))?
                .ok_or_else(|| bad(format!("tarball link '{}' has no target", rel.display())))?
                .into_owned();
            links.push((rel, member_path(&target)?));
        } else if kind.is_file() || kind.is_contiguous() || kind.is_gnu_sparse() {
            let executable = entry.header().mode().unwrap_or(0o644) & 0o100 != 0;
            let mut contents = Vec::new();
            entry.read_to_end(&mut contents).map_err(|e| {
                Error::io(
                    format!("cannot read '{}' from the tarball", rel.display()),
                    e,
                )
            })?;
            clear(&at)?;
            std::fs::write(&at, &contents)
                .map_err(|e| Error::io(format!("cannot write {}", at.display()), e))?;
            std::fs::set_permissions(
                &at,
                std::fs::Permissions::from_mode(if executable { 0o755 } else { 0o644 }),
            )
            .map_err(|e| Error::io(format!("cannot set the mode of {}", at.display()), e))?;
        } else {
            return Err(bad(format!(
                "file '{}' in tarball has unsupported file type {kind:?}",
                name.display()
            )));
        }
    }
    for (rel, target) in links {
        let at = make_parents(dest, &rel)?;
        let from = dest.join(&target);
        if from.is_dir() {
            return Err(bad(format!(
                "tarball member '{}' is a hard link to the directory '{}'",
                rel.display(),
                target.display()
            )));
        }
        clear(&at)?;
        std::fs::copy(&from, &at).map_err(|e| {
            Error::io(
                format!("cannot link {} to {}", at.display(), from.display()),
                e,
            )
        })?;
    }
    Ok(singleton(dest))
}

/// git-utils.cc:dereferenceSingletonDirectory: one entry, and a directory.
fn singleton(dest: &Path) -> PathBuf {
    let mut entries = match std::fs::read_dir(dest) {
        Ok(e) => e,
        Err(_) => return dest.to_path_buf(),
    };
    let Some(Ok(first)) = entries.next() else {
        return dest.to_path_buf();
    };
    if entries.next().is_some() {
        return dest.to_path_buf();
    }
    match first.file_type() {
        Ok(t) if t.is_dir() => first.path(),
        _ => dest.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// The unpacked tree, flattened: a directory is `dir`, a symlink `-> target`,
    /// a file its contents with a leading `x` when it is executable. Only what
    /// the NAR of the tree holds.
    fn tree(dest: &Path) -> BTreeMap<String, String> {
        fn walk(root: &Path, at: &Path, out: &mut BTreeMap<String, String>) {
            let mut names: Vec<PathBuf> = std::fs::read_dir(at)
                .unwrap()
                .map(|e| e.unwrap().path())
                .collect();
            names.sort();
            for p in names {
                let rel = p.strip_prefix(root).unwrap().display().to_string();
                let m = std::fs::symlink_metadata(&p).unwrap();
                if m.is_symlink() {
                    out.insert(
                        rel,
                        format!("-> {}", std::fs::read_link(&p).unwrap().display()),
                    );
                } else if m.is_dir() {
                    out.insert(rel, "dir".into());
                    walk(root, &p, out);
                } else {
                    let x = if m.permissions().mode() & 0o100 != 0 {
                        "x"
                    } else {
                        ""
                    };
                    out.insert(
                        rel,
                        format!(
                            "{x}{}",
                            String::from_utf8_lossy(&std::fs::read(&p).unwrap())
                        ),
                    );
                }
            }
        }
        let mut out = BTreeMap::new();
        walk(dest, dest, &mut out);
        out
    }

    fn work(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("ietsp-unpack-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn tar_gz(entries: &[(&str, tar::EntryType, u32, &[u8])]) -> Vec<u8> {
        let mut b = tar::Builder::new(Vec::new());
        for (name, kind, mode, data) in entries {
            let mut h = tar::Header::new_ustar();
            h.set_entry_type(*kind);
            h.set_mode(*mode);
            h.set_size(if *kind == tar::EntryType::Regular {
                data.len() as u64
            } else {
                0
            });
            if *kind == tar::EntryType::Symlink || *kind == tar::EntryType::Link {
                h.set_link_name(std::str::from_utf8(data).unwrap()).unwrap();
            }
            let body: &[u8] = if *kind == tar::EntryType::Regular {
                data
            } else {
                &[]
            };
            b.append_data(&mut h, name, body).unwrap();
        }
        let tar = b.into_inner().unwrap();
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut gz, &tar).unwrap();
        gz.finish().unwrap()
    }

    const DIR: tar::EntryType = tar::EntryType::Directory;
    const REG: tar::EntryType = tar::EntryType::Regular;

    /// One leading directory and nothing beside it: the hash is of its inside.
    #[test]
    fn strips_one_leading_component() {
        let w = work("strip");
        let bytes = tar_gz(&[
            ("repo-abc/", DIR, 0o755, b""),
            ("repo-abc/flake.nix", REG, 0o644, b"{}\n"),
            ("repo-abc/sub/", DIR, 0o755, b""),
        ]);
        let root = unpack(crate::fetch::http::decompress(bytes).unwrap(), &w.join("x")).unwrap();
        assert_eq!(root.file_name().unwrap(), "repo-abc");
        assert_eq!(
            tree(&root),
            BTreeMap::from([
                ("flake.nix".to_string(), "{}\n".to_string()),
                ("sub".to_string(), "dir".to_string()),
            ])
        );
        std::fs::remove_dir_all(&w).unwrap();
    }

    /// Two top-level entries, or one that is a file: nothing is stripped.
    #[test]
    fn keeps_the_root_when_it_is_not_a_single_directory() {
        let w = work("nostrip");
        let two = tar_gz(&[("a/", DIR, 0o755, b""), ("b", REG, 0o644, b"x")]);
        let root = unpack(crate::fetch::http::decompress(two).unwrap(), &w.join("two")).unwrap();
        assert_eq!(root, w.join("two"));
        assert_eq!(
            tree(&root),
            BTreeMap::from([
                ("a".to_string(), "dir".to_string()),
                ("b".to_string(), "x".to_string()),
            ])
        );

        let one = tar_gz(&[("only", REG, 0o644, b"x")]);
        let root = unpack(crate::fetch::http::decompress(one).unwrap(), &w.join("one")).unwrap();
        assert_eq!(root, w.join("one"));
        std::fs::remove_dir_all(&w).unwrap();
    }

    #[test]
    fn modes_symlinks_and_empty_dirs() {
        let w = work("modes");
        let bytes = tar_gz(&[
            ("t/", DIR, 0o755, b""),
            ("t/run", REG, 0o755, b"#!/bin/sh\n"),
            ("t/plain", REG, 0o644, b"p"),
            ("t/empty/", DIR, 0o700, b""),
            ("t/link", tar::EntryType::Symlink, 0o777, b"plain"),
            ("t/same", tar::EntryType::Link, 0o644, b"t/plain"),
        ]);
        let root = unpack(crate::fetch::http::decompress(bytes).unwrap(), &w.join("x")).unwrap();
        assert_eq!(
            tree(&root),
            BTreeMap::from([
                ("empty".to_string(), "dir".to_string()),
                ("link".to_string(), "-> plain".to_string()),
                ("plain".to_string(), "p".to_string()),
                ("run".to_string(), "x#!/bin/sh\n".to_string()),
                ("same".to_string(), "p".to_string()),
            ])
        );
        std::fs::remove_dir_all(&w).unwrap();
    }

    /// The last copy of a member wins, as the git sink's `id` ordering does.
    #[test]
    fn a_repeated_member_keeps_the_last_copy() {
        let w = work("dup");
        let bytes = tar_gz(&[
            ("t/", DIR, 0o755, b""),
            ("t/f", REG, 0o644, b"first"),
            ("t/f", REG, 0o755, b"second"),
        ]);
        let root = unpack(crate::fetch::http::decompress(bytes).unwrap(), &w.join("x")).unwrap();
        assert_eq!(
            tree(&root),
            BTreeMap::from([("f".to_string(), "xsecond".to_string())])
        );
        std::fs::remove_dir_all(&w).unwrap();
    }

    /// A device, socket or fifo member is an error, as tarfile.cc:253 is.
    #[test]
    fn an_unsupported_member_type_is_an_error() {
        let w = work("kinds");
        let bytes = tar_gz(&[("dev", tar::EntryType::Fifo, 0o644, b"")]);
        let e = unpack(crate::fetch::http::decompress(bytes).unwrap(), &w.join("y"));
        assert!(e.unwrap_err().to_string().contains("unsupported file type"));
        std::fs::remove_dir_all(&w).unwrap();
    }

    /// An absolute member name loses its root, as `relative_path()` does; `..`
    /// is refused, as `ARCHIVE_EXTRACT_SECURE_NODOTDOT` refuses it.
    #[test]
    fn member_names_are_made_relative_and_cannot_escape() {
        assert_eq!(member_path(Path::new("/a/b")).unwrap(), Path::new("a/b"));
        assert_eq!(member_path(Path::new("./a")).unwrap(), Path::new("a"));
        assert_eq!(member_path(Path::new("a/./b")).unwrap(), Path::new("a/b"));
        let e = member_path(Path::new("../outside")).unwrap_err();
        assert!(e.to_string().contains("escapes the archive"));
        let e = member_path(Path::new("a/../../b")).unwrap_err();
        assert!(e.to_string().contains("escapes the archive"));
    }

    /// The NAR hash of an unpacked tree is what `nix hash path` prints for it;
    /// the expected value comes from a tarball whose `narHash` a lock file
    /// records (`tests/data/tarball`).
    #[test]
    fn nar_hash_of_the_fixture() {
        let w = work("golden");
        let bytes = std::fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/tarball/tree.tar.gz"),
        )
        .unwrap();
        let root = unpack(crate::fetch::http::decompress(bytes).unwrap(), &w.join("x")).unwrap();
        let want = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/tarball/narHash"),
        )
        .unwrap();
        assert_eq!(
            crate::store::nar::nar_hash(&root).unwrap().to_sri(),
            want.trim()
        );
        std::fs::remove_dir_all(&w).unwrap();
    }
}

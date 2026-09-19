//! Evaluator-side use of the pure `store` module: copying source paths to
//! the (virtual) store, and reading a virtual store path back from the
//! local tree it was copied from or from the spool of a fetched input.
//!
//! This is the one place that maps a store path to its bytes. Everything the
//! evaluator reads for a store path goes through [`locate`]: `import`,
//! `readFile`, `readDir`, `pathExists`, `readFileType`, `hashFile`,
//! `builtins.path`, `filterSource` and string coercion. The three answers are
//! the real file system, a local tree a virtual copy stands for, and a member
//! of a fetched flake input served out of its spool by offset
//! (`crate::inputs`).

use std::collections::{BTreeSet, HashSet};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::eval::{Error, ErrorKind, EvalResult, Evaluator, Pos};
use crate::store::path::{IngestionMethod, STORE_DIR, StorePath, make_fixed_output_path};
use crate::store::source::SourceOrigin;

/// Where a read of `path` goes.
pub enum Access {
    /// Not under a tree this evaluation added: read `path` itself.
    Store,
    /// Under a tree this evaluation added from a local directory: read the
    /// local file. `accepted` is the tree's filter result, `None` for none.
    Local {
        path: PathBuf,
        accepted: Option<Arc<HashSet<PathBuf>>>,
    },
    /// Under a fetched flake input the input store holds: read the member at
    /// `rel` out of the entry's spool. `rel` is empty for the tree itself.
    Input {
        entry: Arc<crate::inputs::Entry>,
        base: StorePath,
        rel: Vec<u8>,
    },
    /// Under such a tree, but its filter left the entry out (an untracked
    /// file of a git tree): the store copy has no such entry.
    Missing,
}

/// Nix reads a copied tree from the store; ietsp never writes the store, so
/// a path under a store path this evaluation produced from a local tree
/// (`"${./x}"`, `builtins.path`, the flake root, a `path:` input) reads the
/// local tree instead, and a path under a fetched input reads that input's
/// spool. The content is the same by construction: the store path is the NAR
/// hash of exactly those bytes.
pub fn locate(ev: &Evaluator, path: &[u8]) -> Access {
    let Some(rest) = path
        .strip_prefix(STORE_DIR.as_bytes())
        .and_then(|r| r.strip_prefix(b"/"))
    else {
        return Access::Store;
    };
    let (base, sub) = match rest.iter().position(|&b| b == b'/') {
        Some(i) => (&rest[..i], &rest[i + 1..]),
        None => (rest, &b""[..]),
    };
    let Ok(base) = std::str::from_utf8(base) else {
        return Access::Store;
    };
    let Ok(sp) = StorePath::parse(&format!("{STORE_DIR}/{base}")) else {
        return Access::Store;
    };
    let (local, accepted) = match ev.sources.get(&sp) {
        Some(SourceOrigin::Tree {
            local, accepted, ..
        }) => (local, accepted),
        Some(SourceOrigin::Input { entry, rel, .. }) => {
            return crate::inputs::access(&entry, &rel, sub);
        }
        _ => return Access::Store,
    };
    let path = if sub.is_empty() {
        local
    } else {
        local.join(std::ffi::OsStr::from_bytes(sub))
    };
    if !sub.is_empty()
        && let Some(a) = &accepted
        && !a.contains(&path)
    {
        return Access::Missing;
    }
    Access::Local { path, accepted }
}

/// What a read of `path` opens.
pub enum Whence {
    /// A real file on disk, at this path.
    Fs(Vec<u8>),
    /// A member of a fetched flake input, served from the entry's spool.
    In(Arc<crate::inputs::Entry>, StorePath, Vec<u8>),
}

/// Where the bytes of `path` are: the file itself, its local original, or a
/// member of a fetched input; `None` when a tree's filter left the entry out.
pub fn whence(ev: &Evaluator, path: &[u8]) -> Option<Whence> {
    match locate(ev, path) {
        Access::Store => Some(Whence::Fs(path.to_vec())),
        Access::Local { path, .. } => Some(Whence::Fs(path.into_os_string().into_vec())),
        Access::Input { entry, base, rel } => Some(Whence::In(entry, base, rel)),
        Access::Missing => None,
    }
}

/// Store path a source path would get when copied (`"${./foo}"`). Nothing
/// is written; the NAR hash is what `fetchToStore` would compute.
pub fn copy_path_to_store(
    ev: &mut Evaluator,
    path: &'static [u8],
    pos: Pos,
) -> EvalResult<&'static [u8]> {
    if let Some(sp) = ev.src_to_store.get(path) {
        return Ok(sp);
    }
    let copy_err = |e: std::io::Error| {
        Error::boxed(
            ErrorKind::Io,
            format!(
                "copying '{}' to the store: {}",
                String::from_utf8_lossy(path),
                e
            ),
            pos,
        )
    };
    let (local, accepted) = match locate(ev, path) {
        Access::Store => (
            Path::new(std::ffi::OsStr::from_bytes(path)).to_path_buf(),
            None,
        ),
        Access::Local { path, accepted } => (path, accepted),
        Access::Input { entry, base, rel } => {
            let full = crate::inputs::copy_to_store(ev, &entry, &base, &rel, path, pos)?;
            return Ok(full);
        }
        Access::Missing => return Err(copy_err(std::io::Error::from_raw_os_error(2))),
    };
    // a plain local directory, not a tree the flake machinery filtered
    if ev.lint_on(crate::lint::LintId::DirCopyUnfiltered)
        && accepted.is_none()
        && !ev.is_store_path_text(path)
        && ev.trace.stat_now(local.as_os_str().as_bytes(), true) == crate::trace::Kind::Directory
    {
        crate::lint::purity::dir_copy_unfiltered(ev, path, pos)?;
    }
    let git_lint = ev.lint_on(crate::lint::LintId::GitInSource);
    let mut saw_git = false;
    let mut fp = ev.trace.fp(&local);
    let mut filter = |p: &Path, t: crate::store::nar::FileType| {
        let keep = accepted.as_ref().is_none_or(|a| a.contains(p));
        if let Some(fp) = &mut fp {
            fp.entry(p, t, keep);
        }
        if keep && git_lint && is_git_entry(&local, p) {
            saw_git = true;
        }
        keep
    };
    let (hash, nar_size) = crate::store::nar::nar_hash_filtered(
        &local,
        crate::store::hash::HashAlgo::Sha256,
        &mut filter,
    )
    .map_err(copy_err)?;
    ev.trace.tree(
        &local,
        crate::trace::hash_to_digest(&hash),
        accepted.as_deref(),
        fp,
    );
    if saw_git {
        crate::lint::purity::git_in_source(ev, path, pos)?;
    }
    crate::lint::purity::large_source_copy(ev, path, nar_size, pos)?;
    let name = Path::new(std::ffi::OsStr::from_bytes(path))
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "source".into());
    let sp = make_fixed_output_path(IngestionMethod::Recursive, &hash, &name, &BTreeSet::new())
        .map_err(|e| Error::boxed(ErrorKind::Eval, e.0, pos))?;
    let full = ev.arena.alloc_bytes(sp.to_full_string().as_bytes());
    ev.lints.note_copy(full);
    ev.src_to_store.insert(path.to_vec(), full);
    ev.sources.insert(
        sp,
        SourceOrigin::Tree {
            local,
            accepted,
            nar_hash: hash,
            nar_size,
        },
    );
    Ok(full)
}

/// `p` is the `.git` entry at the top of the tree rooted at `root`.
pub fn is_git_entry(root: &Path, p: &Path) -> bool {
    p.file_name().is_some_and(|n| n == ".git") && p.parent() == Some(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn virtual_tree_reads_go_to_the_local_files() {
        let ev = Evaluator::new();
        let root = PathBuf::from("/r");
        let sp = StorePath::parse("/nix/store/z4wkdlk69gck09s64a8ghpn6h7bxhkyy-source").unwrap();
        let accepted: HashSet<PathBuf> = ["/r/flake.nix", "/r/a", "/r/a/b.nix"]
            .map(PathBuf::from)
            .into();
        ev.sources.insert(
            sp.clone(),
            SourceOrigin::Tree {
                local: root.clone(),
                accepted: Some(Arc::new(accepted)),
                nar_hash: crate::store::hash::Hash::from_bytes(
                    crate::store::hash::HashAlgo::Sha256,
                    &[0u8; 32],
                )
                .unwrap(),
                nar_size: 0,
            },
        );
        let on_disk = |p: &[u8]| match whence(&ev, p) {
            Some(Whence::Fs(p)) => Some(p),
            _ => None,
        };
        let full = sp.to_full_string();
        assert_eq!(on_disk(full.as_bytes()).unwrap(), b"/r");
        assert_eq!(
            on_disk(format!("{full}/a/b.nix").as_bytes()).unwrap(),
            b"/r/a/b.nix"
        );
        assert!(on_disk(format!("{full}/untracked").as_bytes()).is_none());
        assert!(on_disk(format!("{full}/.git/HEAD").as_bytes()).is_none());
        let other = b"/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-other/x";
        assert_eq!(on_disk(other).unwrap(), other);
        assert_eq!(on_disk(b"/etc/passwd").unwrap(), b"/etc/passwd");
        assert!(matches!(
            locate(&ev, b"/nix/store/not-a-store-path"),
            Access::Store
        ));
    }
}

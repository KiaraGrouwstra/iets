//! Evaluator-side use of the pure `store` module: copying source paths to
//! the (virtual) store, and reading a virtual store path back from the
//! local tree it was copied from.

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
    /// Under such a tree, but its filter left the entry out (an untracked
    /// file of a git tree): the store copy has no such entry.
    Missing,
}

/// Nix reads a copied tree from the store; ietsp never writes the store, so
/// a path under a store path this evaluation produced from a local tree
/// (`"${./x}"`, `builtins.path`, the flake root, a `path:` input) reads the
/// local tree instead. The content is the same by construction: the store
/// path is the NAR hash of exactly those files.
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
    let Some(SourceOrigin::Tree {
        local, accepted, ..
    }) = ev.sources.get(&sp)
    else {
        return Access::Store;
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

/// The file to open for `path`: `path` itself or its local original; `None`
/// when the tree's filter left the entry out.
pub fn physical(ev: &Evaluator, path: &[u8]) -> Option<Vec<u8>> {
    match locate(ev, path) {
        Access::Store => Some(path.to_vec()),
        Access::Local { path, .. } => Some(path.into_os_string().into_vec()),
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
        Access::Missing => return Err(copy_err(std::io::Error::from_raw_os_error(2))),
    };
    let mut filter =
        |p: &Path, _: crate::store::nar::FileType| accepted.as_ref().is_none_or(|a| a.contains(p));
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
    );
    let name = Path::new(std::ffi::OsStr::from_bytes(path))
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "source".into());
    let sp = make_fixed_output_path(IngestionMethod::Recursive, &hash, &name, &BTreeSet::new())
        .map_err(|e| Error::boxed(ErrorKind::Eval, e.0, pos))?;
    let full = ev.arena.alloc_bytes(sp.to_full_string().as_bytes());
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
        let full = sp.to_full_string();
        assert_eq!(physical(&ev, full.as_bytes()).unwrap(), b"/r");
        assert_eq!(
            physical(&ev, format!("{full}/a/b.nix").as_bytes()).unwrap(),
            b"/r/a/b.nix"
        );
        assert!(physical(&ev, format!("{full}/untracked").as_bytes()).is_none());
        assert!(physical(&ev, format!("{full}/.git/HEAD").as_bytes()).is_none());
        let other = b"/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-other/x";
        assert_eq!(physical(&ev, other).unwrap(), other);
        assert_eq!(physical(&ev, b"/etc/passwd").unwrap(), b"/etc/passwd");
        assert!(matches!(
            locate(&ev, b"/nix/store/not-a-store-path"),
            Access::Store
        ));
    }
}

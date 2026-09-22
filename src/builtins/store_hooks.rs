//! Evaluator-side use of the pure `store` module: copying source paths to
//! the (virtual) store, and reading a virtual store path back from the
//! local tree it was copied from, from the spool of a fetched input, or from
//! memory.
//!
//! This is the one place that maps a store path to its bytes. Everything the
//! evaluator reads for a store path goes through [`locate`]: `import`,
//! `readFile`, `readDir`, `pathExists`, `readFileType`, `hashFile`,
//! `findFile`, `storePath`, `fetchTree`, `builtins.path`, `filterSource` and
//! string coercion, and a read of a file through [`open`], which applies
//! pure evaluation's allow list first. The answer is a [`SrcPath`]: the real
//! file system, a local tree a virtual copy stands for, a member of a fetched
//! flake input served out of its spool by offset (`crate::inputs`), or a
//! `toFile` text or `.drv` this evaluation holds in memory. Its readers below
//! record what they saw in the tracing cache's read set.

use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{self, Read};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::eval::{Error, ErrorKind, EvalResult, Evaluator, Pos};
use crate::flake::spool::Node;
use crate::store::hash::{HashAlgo, Hasher};
use crate::store::nar::{FileType, PathFilter};
use crate::store::path::{IngestionMethod, STORE_DIR, StorePath, make_fixed_output_path};
use crate::store::source::{SourceOrigin, Src, SrcPath};
use crate::trace::Kind;

/// Nix reads a copied tree from the store; ietsp never writes the store, so
/// a path under a store path this evaluation produced from a local tree
/// (`"${./x}"`, `builtins.path`, the flake root, a `path:` input) reads the
/// local tree instead, and a path under a fetched input reads that input's
/// spool. The content is the same by construction: the store path is the NAR
/// hash of exactly those bytes. `None` when `path` is under no such store
/// path.
pub fn locate(ev: &Evaluator, path: &[u8]) -> Option<SrcPath> {
    let rest = path
        .strip_prefix(STORE_DIR.as_bytes())?
        .strip_prefix(b"/")?;
    let (base, sub) = match rest.iter().position(|&b| b == b'/') {
        Some(i) => (&rest[..i], &rest[i + 1..]),
        None => (rest, &b""[..]),
    };
    let sp = StorePath::parse(&format!("{STORE_DIR}/{}", std::str::from_utf8(base).ok()?)).ok()?;
    let at = match ev.sources.get(&sp) {
        Some(o) => o.at,
        None => SrcPath {
            src: Src::Mem(ev.drv.drvs.get(&sp)?.aterm?),
            rel: Vec::new(),
        },
    };
    // a vanished entry falls back to the store, which a build has usually filled by then
    if let Src::Spool(name) = &at.src
        && crate::inputs::open(name).is_none()
    {
        return None;
    }
    Some(at.join(sub))
}

/// [`locate`], else `path` itself on the real file system.
pub fn resolve(ev: &Evaluator, path: &[u8]) -> SrcPath {
    locate(ev, path).unwrap_or_else(|| SrcPath::fs(PathBuf::from(OsStr::from_bytes(path)), None))
}

/// Whether a pure evaluation may read `path` (eval.cc: rootFS is the store
/// behind an allow list): only under a store path this evaluation added or a
/// fetch found on disk. Always true when impure.
pub fn allowed(ev: &Evaluator, path: &[u8]) -> bool {
    if !ev.pure {
        return true;
    }
    let Some(rest) = path
        .strip_prefix(STORE_DIR.as_bytes())
        .and_then(|r| r.strip_prefix(b"/"))
    else {
        return false;
    };
    let base = rest.split(|&b| b == b'/').next().unwrap_or_default();
    let Ok(sp) = std::str::from_utf8(base).map(StorePath::from_base_name) else {
        return false;
    };
    let Ok(sp) = sp else {
        return false;
    };
    ev.sources.lock().contains_key(&sp) || ev.allowed.lock().contains_key(&sp)
}

/// [`resolve`] behind the pure-eval allow list, with the error Nix's
/// `AllowListSourceAccessor` throws: what every read of a file goes through.
/// `pathExists` (false, not an error) and a `.drv` import (read through the
/// store, as Nix does) are the readers that resolve the path themselves.
pub fn open(ev: &Evaluator, path: &[u8], pos: Pos) -> EvalResult<SrcPath> {
    if allowed(ev, path) {
        return Ok(resolve(ev, path));
    }
    Err(Error::boxed(
        ErrorKind::Eval,
        format!(
            "access to absolute path '{}' is forbidden in pure evaluation mode (use '--impure' to override)",
            String::from_utf8_lossy(path)
        ),
        pos,
    ))
}

/// Copying a [`Src::Mem`] path again would need a source origin that
/// uploads a NAR from memory. It is refused instead, so the classic names
/// fall back to nix.
pub fn recopy_unsupported(path: &[u8], pos: Pos) -> Box<Error> {
    Error::boxed(
        ErrorKind::Unsupported,
        format!(
            "ietsp: copying '{}' to the store again is not supported",
            String::from_utf8_lossy(path)
        ),
        pos,
    )
}

/// [`SrcPath::stat`] of `path`, `Missing` for any error.
pub fn stat(ev: &Evaluator, path: &[u8], follow: bool) -> Kind {
    resolve(ev, path).stat(ev, follow).unwrap_or(Kind::Missing)
}

/// What a [`SrcPath`] opens.
pub enum At<'a> {
    Disk(Vec<u8>),
    Spool(Arc<crate::inputs::Entry>, &'a str),
    Mem(&'a [u8]),
}

/// A failed read: the verb Nix names the failed step with, and the error.
pub type Failed = (&'static str, io::Error);

const STAT: &str = "getting status of";
const READ: &str = "reading file";

fn os(p: &[u8]) -> &Path {
    Path::new(OsStr::from_bytes(p))
}

fn enoent() -> io::Error {
    io::Error::from_raw_os_error(libc::ENOENT)
}

/// `strerror(ENOENT)` alone, the text `import`'s stat has always carried (no
/// Rust " (os error 2)" suffix).
fn missing() -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, "No such file or directory")
}

fn below(root: &Path, rel: &[u8]) -> PathBuf {
    if rel.is_empty() {
        root.to_path_buf()
    } else {
        root.join(OsStr::from_bytes(rel))
    }
}

fn kind_of(n: &Node) -> Kind {
    match n {
        Node::Dir(_) => Kind::Directory,
        Node::File { .. } => Kind::Regular,
        Node::Symlink(_) => Kind::Symlink,
    }
}

impl SrcPath {
    /// The path on disk, for [`Src::Fs`].
    pub fn local(&self) -> Option<PathBuf> {
        let Src::Fs { root, .. } = &self.src else {
            return None;
        };
        Some(below(root, &self.rel))
    }

    /// Whether the tree's filter kept `p`, a path on disk.
    pub fn accepts(&self, p: &Path) -> bool {
        match &self.src {
            Src::Fs {
                accepted: Some(a), ..
            } => a.contains(p),
            _ => true,
        }
    }

    /// `None` when the store copy has no such entry: the tree's filter left
    /// it out, or it lies below a file in memory.
    pub fn at(&self) -> Option<At<'_>> {
        match &self.src {
            Src::Fs { .. } => {
                let p = self.local()?;
                (self.rel.is_empty() || self.accepts(&p))
                    .then(|| At::Disk(p.into_os_string().into_vec()))
            }
            Src::Spool(name) => Some(At::Spool(crate::inputs::open(name)?, name)),
            Src::Mem(b) => self.rel.is_empty().then_some(At::Mem(b)),
        }
    }

    pub fn hidden(&self) -> bool {
        self.at().is_none()
    }

    /// `stat`, or `lstat` unless `follow`, recorded in the read set.
    pub fn stat(&self, ev: &Evaluator, follow: bool) -> io::Result<Kind> {
        match self.at().ok_or_else(missing)? {
            At::Disk(p) => {
                let r = if follow {
                    fs::metadata(os(&p))
                } else {
                    fs::symlink_metadata(os(&p))
                };
                let kind = Kind::of(&r);
                ev.trace.stat(&p, follow, kind);
                r.map(|_| kind)
            }
            At::Spool(entry, name) => {
                crate::inputs::record(ev, &entry, name);
                let node = entry.tree.resolve(&self.rel, follow).ok_or_else(missing)?;
                Ok(kind_of(node))
            }
            At::Mem(_) => Ok(Kind::Regular),
        }
    }

    /// `import`'s stat: whether `/default.nix` is appended. A regular file on
    /// disk records nothing: its `File` observation checks the type.
    pub fn is_dir(&self, ev: &Evaluator) -> io::Result<bool> {
        if let Some(At::Disk(p)) = self.at() {
            let dir = fs::metadata(os(&p))?.is_dir();
            if dir {
                ev.trace.stat(&p, true, Kind::Directory);
            }
            return Ok(dir);
        }
        Ok(self.stat(ev, true)? == Kind::Directory)
    }

    /// A regular file's bytes, recorded in the read set. `sized` is `readFile`'s
    /// read: a stat first, `EISDIR` for a file that is not regular, then
    /// `st_size` bytes. Otherwise the file is read to its end (`import`,
    /// `hashFile`).
    pub fn read(&self, ev: &Evaluator, sized: bool) -> Result<Vec<u8>, Failed> {
        match self.at().ok_or((STAT, enoent()))? {
            At::Disk(p) => {
                let (meta, f) = if sized {
                    let meta = fs::metadata(os(&p)).map_err(|e| (STAT, e))?;
                    if !meta.is_file() {
                        return Err((STAT, io::Error::from_raw_os_error(libc::EISDIR)));
                    }
                    (meta, fs::File::open(os(&p)).map_err(|e| (READ, e))?)
                } else {
                    let f = fs::File::open(os(&p)).map_err(|e| (READ, e))?;
                    (f.metadata().map_err(|e| (STAT, e))?, f)
                };
                let mut bytes = Vec::with_capacity(meta.len() as usize);
                f.take(if sized { meta.len() } else { u64::MAX })
                    .read_to_end(&mut bytes)
                    .map_err(|e| (READ, e))?;
                ev.trace.file(&p, &meta, &bytes);
                Ok(bytes)
            }
            At::Spool(entry, name) => {
                let at = match entry.tree.resolve(&self.rel, true) {
                    Some(Node::File { at, .. }) => at,
                    Some(_) => return Err((STAT, io::Error::from_raw_os_error(libc::EISDIR))),
                    None => return Err((STAT, enoent())),
                };
                crate::inputs::record(ev, &entry, name);
                entry.tree.read(at).map_err(|e| (READ, e))
            }
            At::Mem(b) => Ok(b.to_vec()),
        }
    }

    /// A directory's entries, recorded in the read set; `ENOTDIR` for
    /// anything else. The listing hides entries the tree's filter left out
    /// (untracked files of a git tree); the `Dir` observation keeps the full
    /// listing, the one validation can re-read.
    pub fn read_dir(&self, ev: &Evaluator) -> Result<Vec<(Vec<u8>, FileType)>, Failed> {
        let notdir = || (STAT, io::Error::from_raw_os_error(libc::ENOTDIR));
        match self.at().ok_or((STAT, enoent()))? {
            At::Disk(p) => {
                let rd = fs::read_dir(os(&p)).map_err(|e| ("opening directory", e))?;
                let mut out = Vec::new();
                let mut listing = Vec::new();
                for e in rd {
                    let e = e.map_err(|e| ("reading directory", e))?;
                    let ft = e.file_type().map_err(|e| (STAT, e))?;
                    let (t, kind) = if ft.is_symlink() {
                        (FileType::Symlink, Kind::Symlink)
                    } else if ft.is_dir() {
                        (FileType::Directory, Kind::Directory)
                    } else if ft.is_file() {
                        (FileType::Regular, Kind::Regular)
                    } else {
                        (FileType::Unknown, Kind::Other)
                    };
                    let name = e.file_name().into_vec();
                    if ev.trace.is_on() {
                        listing.push((name.clone(), kind));
                    }
                    if self.accepts(&e.path()) {
                        out.push((name, t));
                    }
                }
                if ev.trace.is_on() {
                    ev.trace.record(crate::trace::Obs::Dir {
                        path: p,
                        hash: crate::trace::dir_listing_hash(&mut listing),
                    });
                }
                Ok(out)
            }
            At::Spool(entry, name) => {
                let node = entry
                    .tree
                    .resolve(&self.rel, true)
                    .ok_or((STAT, enoent()))?;
                let Node::Dir(d) = node else {
                    return Err(notdir());
                };
                crate::inputs::record(ev, &entry, name);
                Ok(d.iter()
                    .map(|(n, child)| (n.clone(), child.file_type()))
                    .collect())
            }
            At::Mem(_) => Err(notdir()),
        }
    }

    /// The NAR of this path, `filter` asked about every entry below it: on
    /// disk under its path on disk, in a spool under `/`. The tree's own
    /// filter is the caller's to apply ([`SrcPath::accepts`]), because a dump
    /// that records the tree must see what the filter hides.
    pub fn dump(&self, sink: &mut dyn io::Write, filter: &mut PathFilter<'_>) -> io::Result<()> {
        match &self.src {
            Src::Fs { root, .. } => {
                crate::store::nar::dump_path(&below(root, &self.rel), sink, filter)
            }
            Src::Spool(name) => {
                let entry = crate::inputs::open(name).ok_or_else(|| {
                    io::Error::other(format!(
                        "the input store no longer holds '{name}'; evaluate again to fetch it"
                    ))
                })?;
                let node = entry.tree.resolve(&self.rel, true).ok_or_else(enoent)?;
                entry.tree.dump_filtered(node, Path::new("/"), sink, filter)
            }
            Src::Mem(b) => crate::store::nar::dump_string(b, sink),
        }
    }

    /// The NAR of the store copy: under the tree's own filter.
    pub fn nar(&self, sink: &mut dyn io::Write) -> io::Result<()> {
        self.dump(sink, &mut |p, _| self.accepts(p))
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
    let at = open(ev, path, pos)?;
    let copy_err = |e: io::Error| {
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
    let name = Path::new(OsStr::from_bytes(path))
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "source".into());
    let fixed = |hash| {
        make_fixed_output_path(IngestionMethod::Recursive, hash, &name, &BTreeSet::new())
            .map_err(|e| Error::boxed(ErrorKind::Eval, e.0, pos))
    };
    let (sp, origin, hash, nar_size) = match at.at() {
        None => return Err(copy_err(enoent())),
        Some(At::Mem(_)) => return Err(recopy_unsupported(path, pos)),
        // the input itself needs no hashing: its store path is the one the
        // lock named and its NAR hash is in the index
        Some(At::Spool(entry, base)) if at.rel.is_empty() => {
            crate::inputs::record(ev, &entry, base);
            let sp = StorePath::from_base_name(base)
                .map_err(|e| Error::boxed(ErrorKind::Eval, e.0, pos))?;
            (sp, at.clone(), entry.nar_hash, entry.nar_size)
        }
        Some(At::Spool(entry, base)) => {
            crate::inputs::record(ev, &entry, base);
            let mut h = Hasher::new(HashAlgo::Sha256);
            at.dump(&mut h, &mut |_, _| true).map_err(copy_err)?;
            let size = h.bytes_written();
            let hash = h.finish();
            (fixed(&hash)?, at.clone(), hash, size)
        }
        Some(At::Disk(local)) => {
            let local = PathBuf::from(OsString::from_vec(local));
            let accepted = match &at.src {
                Src::Fs { accepted, .. } => accepted.clone(),
                _ => None,
            };
            // a plain local directory, not a tree the flake machinery filtered
            if ev.lint_on(crate::lint::LintId::DirCopyUnfiltered)
                && accepted.is_none()
                && !ev.is_store_path_text(path)
                && ev.trace.stat_now(local.as_os_str().as_bytes(), true) == Kind::Directory
            {
                crate::lint::purity::dir_copy_unfiltered(ev, path, pos)?;
            }
            let git_lint = ev.lint_on(crate::lint::LintId::GitInSource);
            let mut saw_git = false;
            let mut fp = ev.trace.fp(&local);
            let mut filter = |p: &Path, t: FileType| {
                let keep = at.accepts(p);
                if let Some(fp) = &mut fp {
                    fp.entry(p, t, keep);
                }
                if keep && git_lint && is_git_entry(&local, p) {
                    saw_git = true;
                }
                keep
            };
            let (hash, nar_size) =
                crate::store::nar::nar_hash_filtered(&local, HashAlgo::Sha256, &mut filter)
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
            let sp = fixed(&hash)?;
            ev.lints.note_copy(sp.to_full_string().as_bytes());
            (sp, SrcPath::fs(local, accepted), hash, nar_size)
        }
    };
    let full = ev.arena.alloc_bytes(sp.to_full_string().as_bytes());
    ev.src_to_store.insert(path.to_vec(), full);
    ev.sources
        .insert(sp, SourceOrigin::tree(origin, hash, nar_size));
    Ok(full)
}

/// `p` is the `.git` entry at the top of the tree rooted at `root`.
pub fn is_git_entry(root: &Path, p: &Path) -> bool {
    p.file_name().is_some_and(|n| n == ".git") && p.parent() == Some(root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

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
            SourceOrigin::tree(
                SrcPath::fs(root, Some(Arc::new(accepted))),
                crate::store::hash::Hash::from_bytes(HashAlgo::Sha256, &[0u8; 32]).unwrap(),
                0,
            ),
        );
        let on_disk = |p: &[u8]| match resolve(&ev, p).at() {
            Some(At::Disk(p)) => Some(p),
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
        assert!(locate(&ev, b"/nix/store/not-a-store-path").is_none());
    }

    #[test]
    fn a_relative_path_inside_an_entry_joins_once() {
        let rel = |rel: &[u8], sub: &[u8]| SrcPath::spool("x", rel).join(sub).rel;
        assert_eq!(rel(b"", b""), b"");
        assert_eq!(rel(b"lib", b""), b"lib");
        assert_eq!(rel(b"", b"lib"), b"lib");
        assert_eq!(rel(b"lib", b"x.nix"), b"lib/x.nix");
    }
}

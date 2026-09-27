//! The source control system (SCM) of a local working tree. An [`Scm`]
//! supplies what such a tree gives to an evaluation: its root, the files it
//! tracks, the dirty flag and the metadata of its revision. `KNOWN` is the
//! one list of the implementations, and the callers reach them through the
//! functions below it, never by name. A remote input (`git+https:`, `hg+`)
//! is a fetcher and is not here.

mod git;

use std::collections::HashSet;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crate::trace::Trace;

/// A working tree as its SCM reports it.
pub struct Snapshot {
    /// The tracked files that are in the working tree, as absolute paths.
    /// The working tree supplies their contents and their modes.
    pub files: HashSet<PathBuf>,
    /// A tracked file differs from the revision.
    pub dirty: bool,
    /// The revision the tree is at, in ASCII, as the SCM spells it: `rev`
    /// on a clean tree, the start of `dirtyRev` on a dirty one. `None` when
    /// the SCM has no name for it.
    pub rev: Option<String>,
    /// The date of the revision in seconds since the epoch, 0 without one.
    pub last_modified: u64,
    /// The number of revisions up to this one, `None` when the SCM cannot
    /// count them. A dirty tree shows none (fetchTree.cc:emitTreeAttrs).
    pub rev_count: Option<u64>,
}

pub trait Scm: Sync {
    /// The `type` of the tree as an input and the scheme of its URL: `git`
    /// in `git+file://`.
    fn name(&self) -> &'static str;

    /// The name as a message spells it: `Git`.
    fn title(&self) -> &'static str;

    /// `dir` is the root of a working tree of this SCM.
    fn is_root(&self, dir: &Path) -> bool;

    /// Reads the working tree at `root`. Each answer that does not come
    /// from a tracked file is recorded in `trace`, so the tracing cache
    /// asks again.
    fn snapshot(&self, trace: &Trace, root: &Path) -> Result<Snapshot, String>;

    /// For `rel`, a file on disk below `root` that the SCM does not track:
    /// the command that makes the SCM track it. `None` when the SCM tracks
    /// `rel` or gives no answer.
    fn untracked(&self, root: &Path, rel: &OsStr) -> Option<String>;
}

impl PartialEq for dyn Scm {
    fn eq(&self, other: &dyn Scm) -> bool {
        self.name() == other.name()
    }
}

impl Eq for dyn Scm {}

impl std::fmt::Debug for dyn Scm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// Every SCM that ietsp knows, in the order detection asks them.
static KNOWN: &[&dyn Scm] = &[&git::Git];

/// The SCM that has a working tree with its root at `root`.
pub fn at(root: &Path) -> Option<&'static dyn Scm> {
    KNOWN.iter().copied().find(|s| s.is_root(root))
}

/// The working tree that holds `dir`: its SCM and its root, the nearest
/// ancestor (or `dir` itself) that an SCM knows as one.
pub fn detect(dir: &Path) -> Option<(&'static dyn Scm, PathBuf)> {
    dir.ancestors()
        .find_map(|d| Some((at(d)?, d.to_path_buf())))
}

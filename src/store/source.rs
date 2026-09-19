//! Where an input source came from during evaluation. The remote client ships sources from
//! these records, so it never needs a local store.

use std::collections::{BTreeSet, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use super::hash::Hash;
use super::path::StorePath;

/// The entries a tree filter kept, `None` for no filter.
pub type Accepted = Option<Arc<HashSet<PathBuf>>>;

#[derive(Clone, Debug)]
pub enum SourceOrigin {
    /// A local tree copied by `./path`, `filterSource`, `builtins.path` or the flake
    /// machinery. `accepted` holds every entry the filter kept; `None` means there was no
    /// filter. Reads under the store path go to `local` (`builtins::store_hooks::locate`),
    /// so the store copy need not exist. `nar_hash` and `nar_size` are what the dump under
    /// that filter produced, the exact pair `AddMultipleToStore` needs, so no upload path
    /// hashes the tree a second time.
    Tree {
        local: PathBuf,
        accepted: Accepted,
        nar_hash: Hash,
        nar_size: u64,
    },
    /// `builtins.toFile`: the contents and references, held in memory.
    Text {
        contents: Vec<u8>,
        references: BTreeSet<StorePath>,
    },
}

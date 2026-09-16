//! Where an input source came from during evaluation. The remote client ships sources from
//! these records, so it never needs a local store.

use std::collections::{BTreeSet, HashSet};
use std::path::PathBuf;

use super::path::StorePath;

#[derive(Clone, Debug)]
pub enum SourceOrigin {
    /// A local tree copied by `./path`, `filterSource`, or `builtins.path`. `accepted` holds
    /// every entry the filter kept; `None` means there was no filter.
    Tree {
        local: PathBuf,
        accepted: Option<HashSet<PathBuf>>,
    },
    /// `builtins.toFile`: the contents and references, held in memory.
    Text {
        contents: Vec<u8>,
        references: BTreeSet<StorePath>,
    },
}

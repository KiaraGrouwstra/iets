//! Paths and names as bytes, the way the store and the wire format see them.

pub use std::os::unix::ffi::{OsStrExt, OsStringExt};

/// How many leading components of a path `..` cannot remove. Unix has none.
pub fn anchor_len(_comps: &[&[u8]]) -> usize {
    0
}

//! amber-store's object model, reimplemented so that a tree ietsp uploads yields exactly the
//! keys amber-store (core-rs, the rev amberfs pins) produces for the same tree. ietsp cannot
//! depend on the core-rs crate (a private git dependency the nix build cannot fetch), so
//! `tools/amber-diff/` proves the equality differentially against that crate.

pub mod cdc;
pub mod key;
pub mod objects;
pub mod tree;

pub use key::{Key, Type};
pub use objects::Object;

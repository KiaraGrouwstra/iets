//! amber-store's object model, reimplemented so that a tree ietsp uploads yields exactly the
//! keys amber-store (core-rs, github.com/amber-store/core-rs at rev 72f38e9e) produces for the
//! same tree with our deployment's parameters. ietsp does not depend on the core-rs crate (a git
//! dependency with a large dependency tree), so `tools/amber-diff/` proves the equality
//! differentially against that crate.

pub mod cdc;
pub mod key;
pub mod objects;
pub mod tree;

pub use key::{Key, Type};
pub use objects::Object;

//! Fetching: an HTTP client, a spooled archive read by offset, and an archive
//! unpacker. None of it knows about flakes; a locked flake input is one caller
//! (`crate::source`), `ietsp prefetch` is another (docs/plumbing.md).

pub mod http;
pub mod spool;
pub mod unpack;

//! Nix's own C API, implemented on ietsp.
//!
//! The headers of `nix_api_util.h`, `nix_api_expr.h` and `nix_api_value.h`
//! are the specification: same symbol names, same signatures, same error
//! codes, same ownership rules. A program compiled against those headers
//! links this library in place of Nix's `libnixexprc`/`libnixutilc`/
//! `libnixstorec` and needs no source change.
//!
//! Everything here is built on `ietsp::api`, the safe embedding surface
//! (docs/library.md). docs/c-api.md lists what is implemented, what is
//! not, and every place the two cannot agree.

#![allow(non_camel_case_types)]
// every entry point's safety contract is the header's; repeating it in a
// `# Safety` section per function would be a second, drifting copy
#![allow(clippy::missing_safety_doc)]

pub mod ctx;
pub mod state;
pub mod value;

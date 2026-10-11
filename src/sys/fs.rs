//! File modes, inodes, symlinks and open flags, which `std::fs` only
//! exposes per platform.

pub use std::os::unix::fs::{
    DirBuilderExt, FileExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt, symlink,
};

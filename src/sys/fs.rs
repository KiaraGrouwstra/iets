//! File modes, inodes, symlinks and open flags, which `std::fs` only
//! exposes per platform.

pub use std::os::unix::fs::{
    DirBuilderExt, FileExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt, symlink,
};

use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;

use rustix::fs::{
    AtFlags, CWD, FlockOperation, OFlags, Timespec, Timestamps, UTIME_NOW, flock, futimens,
    utimensat,
};
use rustix::io::Errno;

/// Refuses a final symbolic link.
pub fn no_follow(o: &mut OpenOptions) -> &mut OpenOptions {
    o.custom_flags(OFlags::NOFOLLOW.bits() as i32)
}

/// Refuses a final symbolic link, and does not wait on a FIFO for a writer.
pub fn no_follow_no_block(o: &mut OpenOptions) -> &mut OpenOptions {
    o.custom_flags((OFlags::NOFOLLOW | OFlags::NONBLOCK).bits() as i32)
}

/// Takes an exclusive lock on `f` if nobody holds it.
pub fn try_lock(f: &File) -> bool {
    flock(f, FlockOperation::NonBlockingLockExclusive).is_ok()
}

/// Takes an exclusive lock on `f`, waiting for it. `false` on a real error.
pub fn lock(f: &File) -> bool {
    loop {
        match flock(f, FlockOperation::LockExclusive) {
            Ok(()) => return true,
            Err(Errno::INTR) => {}
            Err(_) => return false,
        }
    }
}

pub fn unlock(f: &File) {
    let _ = flock(f, FlockOperation::Unlock);
}

const NOW: Timestamps = Timestamps {
    last_access: Timespec {
        tv_sec: 0,
        tv_nsec: UTIME_NOW,
    },
    last_modification: Timespec {
        tv_sec: 0,
        tv_nsec: UTIME_NOW,
    },
};

/// Sets both stamps of `path` to now. On a symbolic link, `follow` picks the
/// link or its target.
pub fn touch(path: &Path, follow: bool) {
    let _ = utimensat(CWD, path, &NOW, at_flags(follow));
}

/// Sets both stamps of the open file to now.
pub fn touch_open(f: &File) {
    let _ = futimens(f, &NOW);
}

/// Sets both stamps of `path` to `secs` after the epoch.
pub fn set_times(path: &Path, secs: i64, follow: bool) -> io::Result<()> {
    let ts = Timespec {
        tv_sec: secs,
        tv_nsec: 0,
    };
    let times = Timestamps {
        last_access: ts,
        last_modification: ts,
    };
    Ok(utimensat(CWD, path, &times, at_flags(follow))?)
}

fn at_flags(follow: bool) -> AtFlags {
    if follow {
        AtFlags::empty()
    } else {
        AtFlags::SYMLINK_NOFOLLOW
    }
}

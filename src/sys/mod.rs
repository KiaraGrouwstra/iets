//! What differs between operating systems, behind one interface. Callers
//! never test the platform. The Linux backend uses the kernel's own
//! facilities. The portable one gives the same answers more slowly.

use std::env;
use std::path::PathBuf;
use std::time::{Duration, Instant};

#[cfg_attr(target_os = "linux", path = "linux.rs")]
#[cfg_attr(not(target_os = "linux"), path = "portable.rs")]
mod imp;

/// What to do about a fault that belongs to this process.
pub struct Fault {
    pub message: &'static [u8],
    /// Abort for a core dump, or exit with status 1.
    pub abort: bool,
}

pub mod context;
#[cfg_attr(windows, path = "errno_windows.rs")]
pub mod errno;
#[cfg_attr(windows, path = "fault_windows.rs")]
pub mod fault;
#[cfg_attr(windows, path = "ffi_windows.rs")]
mod ffi;
#[cfg_attr(windows, path = "fs_windows.rs")]
pub mod fs;
#[cfg_attr(windows, path = "net_windows.rs")]
pub mod net;
#[cfg_attr(windows, path = "process_windows.rs")]
pub mod process;
#[cfg_attr(windows, path = "tty_windows.rs")]
pub mod tty;
#[cfg_attr(windows, path = "vm_windows.rs")]
pub mod vm;
pub mod winpath;

pub use ffi::{
    OsStrExt, OsStringExt, anchor_len, from_native, path_from_internal, path_to_internal,
};
pub use imp::{discard, futex_wait, futex_wake, prefault};
pub use vm::resident;

/// The temporary directory, resolved. Windows hands out the short `RUNNER~1`
/// form, which a resolved path never matches.
pub fn temp_dir() -> PathBuf {
    let t = env::temp_dir();
    fs::canonicalize(&t).unwrap_or(t)
}

/// Fills `buf` from the system's random source. A failure leaves zeros: the
/// bytes are a cache key or a name suffix, and nothing here can act on it.
pub fn random(buf: &mut [u8]) {
    let _ = getrandom::fill(buf);
}

/// The time since `t0`, for timings and statistics. Zero in a fuzzing build:
/// libFuzzer records the values the code compares, and a clock reading in
/// them makes two runs with one seed keep different inputs.
pub fn elapsed(t0: Instant) -> Duration {
    if cfg!(fuzzing) {
        Duration::ZERO
    } else {
        t0.elapsed()
    }
}

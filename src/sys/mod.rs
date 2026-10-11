//! What differs between operating systems, behind one interface. Callers
//! never test the platform. The Linux backend uses the kernel's own
//! facilities. The portable one gives the same answers more slowly.

#[cfg_attr(target_os = "linux", path = "linux.rs")]
#[cfg_attr(not(target_os = "linux"), path = "portable.rs")]
mod imp;

pub mod errno;
pub mod fault;
#[cfg_attr(windows, path = "ffi_windows.rs")]
mod ffi;
pub mod fs;
pub mod net;
pub mod process;
pub mod tty;
pub mod vm;

pub use ffi::{OsStrExt, OsStringExt};
pub use imp::{discard, futex_wait, futex_wake, prefault};

/// Bytes of `[base, base + len)` that are resident, in whole pages.
pub fn resident(base: *mut u8, len: usize) -> usize {
    let page = rustix::param::page_size();
    let mut v = vec![0u8; len / page];
    // SAFETY: one byte per page of the range, which the caller maps. The
    // vector's element type is `c_char` or `u8` depending on the platform.
    unsafe { libc::mincore(base.cast(), len, v.as_mut_ptr().cast()) };
    v.iter().filter(|b| **b & 1 != 0).count() * page
}

/// Fills `buf` from the system's random source. A failure leaves zeros: the
/// bytes are a cache key or a name suffix, and nothing here can act on it.
pub fn random(buf: &mut [u8]) {
    let _ = getrandom::fill(buf);
}

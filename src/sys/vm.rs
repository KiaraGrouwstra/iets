//! Anonymous memory: mapping, giving back and fencing off address space.

use std::io;
use std::ptr;

use rustix::mm::{MapFlags, MprotectFlags, ProtFlags, mmap_anonymous, munmap};

pub fn page_size() -> usize {
    rustix::param::page_size()
}

/// `len` zeroed bytes, readable and writable. A `lazy` mapping reserves
/// address space and charges nothing until the pages are touched.
pub fn map(len: usize, lazy: bool) -> io::Result<*mut u8> {
    let flags = if lazy {
        MapFlags::PRIVATE | MapFlags::NORESERVE
    } else {
        MapFlags::PRIVATE
    };
    // SAFETY: an anonymous mapping aliases nothing.
    let p = unsafe {
        mmap_anonymous(
            ptr::null_mut(),
            len,
            ProtFlags::READ | ProtFlags::WRITE,
            flags,
        )
    };
    Ok(p?.cast())
}

/// # Safety
/// `[p, p + len)` is a mapping of the caller's that nothing uses any more.
pub unsafe fn unmap(p: *mut u8, len: usize) {
    let _ = unsafe { munmap(p.cast(), len) };
}

/// Makes the low `len` bytes of a mapping fault when touched.
///
/// # Safety
/// The range is inside a mapping of the caller's and nothing needs it.
pub unsafe fn fence(p: *mut u8, len: usize) {
    let _ = unsafe { rustix::mm::mprotect(p.cast(), len, MprotectFlags::empty()) };
}

/// Replaces the range with address space that faults and costs nothing.
/// `false` leaves the pages as they were.
///
/// # Safety
/// The range is a mapping of the caller's and nothing may touch it again.
pub unsafe fn revoke(p: *mut u8, len: usize) -> bool {
    unsafe {
        mmap_anonymous(
            p.cast(),
            len,
            ProtFlags::empty(),
            MapFlags::PRIVATE | MapFlags::NORESERVE | MapFlags::FIXED,
        )
    }
    .is_ok()
}

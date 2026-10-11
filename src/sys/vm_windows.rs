//! Anonymous memory through `VirtualAlloc`. Every mapping is committed
//! when it is made, so a lazy one costs commit charge here.

use std::io;
use std::mem;
use std::ptr;

use windows_sys::Win32::System::Memory::{
    MEM_COMMIT, MEM_DECOMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_NOACCESS, PAGE_READWRITE,
    VirtualAlloc, VirtualFree, VirtualProtect,
};
use windows_sys::Win32::System::ProcessStatus::{
    K32QueryWorkingSetEx, PSAPI_WORKING_SET_EX_INFORMATION,
};
use windows_sys::Win32::System::SystemInformation::{GetSystemInfo, SYSTEM_INFO};
use windows_sys::Win32::System::Threading::GetCurrentProcess;

pub fn page_size() -> usize {
    // SAFETY: the struct is the one the call fills.
    unsafe {
        let mut info: SYSTEM_INFO = mem::zeroed();
        GetSystemInfo(&mut info);
        info.dwPageSize as usize
    }
}

/// Bytes of `[base, base + len)` that are in the working set, in whole pages.
pub fn resident(base: *mut u8, len: usize) -> usize {
    let page = page_size();
    let mut pages: Vec<PSAPI_WORKING_SET_EX_INFORMATION> = (0..len / page)
        .map(|i| {
            // SAFETY: all-zero is a valid value of this plain struct.
            let mut p: PSAPI_WORKING_SET_EX_INFORMATION = unsafe { mem::zeroed() };
            p.VirtualAddress = base.wrapping_add(i * page).cast();
            p
        })
        .collect();
    let bytes = (pages.len() * size_of::<PSAPI_WORKING_SET_EX_INFORMATION>()) as u32;
    // SAFETY: the buffer holds `bytes` bytes of the structs the call fills.
    let ok = unsafe { K32QueryWorkingSetEx(GetCurrentProcess(), pages.as_mut_ptr().cast(), bytes) };
    if ok == 0 {
        return 0;
    }
    // SAFETY: the call filled the attributes, whose first bit is `Valid`.
    let valid =
        |p: &PSAPI_WORKING_SET_EX_INFORMATION| unsafe { p.VirtualAttributes.Flags & 1 != 0 };
    pages.iter().filter(|p| valid(p)).count() * page
}

/// `len` zeroed bytes, readable and writable.
pub fn map(len: usize, _lazy: bool) -> io::Result<*mut u8> {
    // SAFETY: a fresh allocation aliases nothing.
    let p = unsafe { VirtualAlloc(ptr::null(), len, MEM_RESERVE | MEM_COMMIT, PAGE_READWRITE) };
    if p.is_null() {
        return Err(io::Error::last_os_error());
    }
    Ok(p.cast())
}

/// Releases the whole allocation `p` starts. Windows takes no length.
///
/// # Safety
/// `p` is the start of a mapping of the caller's that nothing uses any more.
pub unsafe fn unmap(p: *mut u8, _len: usize) {
    let _ = unsafe { VirtualFree(p.cast(), 0, MEM_RELEASE) };
}

/// Makes the low `len` bytes of a mapping fault when touched.
///
/// # Safety
/// The range is inside a mapping of the caller's and nothing needs it.
pub unsafe fn fence(p: *mut u8, len: usize) {
    let mut old = 0;
    let _ = unsafe { VirtualProtect(p.cast(), len, PAGE_NOACCESS, &mut old) };
}

/// Decommits the range, which keeps the address space and makes it fault.
/// `false` leaves the pages as they were.
///
/// # Safety
/// The range is a mapping of the caller's and nothing may touch it again.
pub unsafe fn revoke(p: *mut u8, len: usize) -> bool {
    unsafe { VirtualFree(p.cast(), len, MEM_DECOMMIT) != 0 }
}

/// Hands the pages back and commits fresh zero pages in their place.
///
/// # Safety
/// The range is a mapping of the caller's and nothing in it is needed.
pub unsafe fn reset(p: *mut u8, len: usize) {
    unsafe {
        if VirtualFree(p.cast(), len, MEM_DECOMMIT) != 0 {
            let _ = VirtualAlloc(p.cast(), len, MEM_COMMIT, PAGE_READWRITE);
        }
    }
}

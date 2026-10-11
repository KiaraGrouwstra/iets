//! Turning a fault on a known address into a message and an exit, through a
//! vectored exception handler.

use std::process;
use std::ptr;
use std::sync::OnceLock;

use windows_sys::Win32::Foundation::EXCEPTION_ACCESS_VIOLATION;
use windows_sys::Win32::Storage::FileSystem::WriteFile;
use windows_sys::Win32::System::Console::{GetStdHandle, STD_ERROR_HANDLE};
use windows_sys::Win32::System::Diagnostics::Debug::{
    AddVectoredExceptionHandler, EXCEPTION_CONTINUE_SEARCH, EXCEPTION_POINTERS,
};

pub use super::Fault;

static CLASSIFY: OnceLock<fn(usize) -> Option<Fault>> = OnceLock::new();

/// Adds the handler. `classify` gets the faulting address and runs inside
/// the handler, so it must not lock or allocate. A fault it declines goes
/// on to the next handler.
pub fn install(classify: fn(usize) -> Option<Fault>) {
    if CLASSIFY.set(classify).is_ok() {
        // SAFETY: the handler is a plain function that lives for the process.
        unsafe { AddVectoredExceptionHandler(1, Some(on_fault)) };
    }
}

unsafe extern "system" fn on_fault(info: *mut EXCEPTION_POINTERS) -> i32 {
    // SAFETY: the system passes valid pointers to the handler.
    let record = unsafe { &*(*info).ExceptionRecord };
    if record.ExceptionCode != EXCEPTION_ACCESS_VIOLATION || record.NumberParameters < 2 {
        return EXCEPTION_CONTINUE_SEARCH;
    }
    // the second parameter is the address that was touched
    let addr = record.ExceptionInformation[1];
    if let Some(f) = CLASSIFY.get().and_then(|c| c(addr)) {
        let mut written = 0;
        // SAFETY: the buffer is the message and stderr's handle is valid.
        unsafe {
            WriteFile(
                GetStdHandle(STD_ERROR_HANDLE),
                f.message.as_ptr(),
                f.message.len() as u32,
                &mut written,
                ptr::null_mut(),
            );
        }
        if f.abort {
            process::abort();
        }
        process::exit(1);
    }
    EXCEPTION_CONTINUE_SEARCH
}

/// Windows runs the handler on the faulting stack, so there is no separate
/// stack to give. A fault from an exhausted fiber stack can fail to report.
pub fn alt_stack() {}

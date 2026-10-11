use std::sync::atomic::AtomicU32;

use rustix::mm::{Advice, madvise};
use rustix::thread::futex;

/// Sleeps while `*word == expected`. Returns early when the word moved or a
/// signal arrived. The caller looks again.
pub fn futex_wait(word: &AtomicU32, expected: u32) {
    let _ = futex::wait(word, futex::Flags::PRIVATE, expected, None);
}

/// Wakes up to `n` threads sleeping on `word`.
pub fn futex_wake(word: &AtomicU32, n: u32) {
    let _ = futex::wake(word, futex::Flags::PRIVATE, n);
}

/// Populates `[p, p + len)` now, with huge pages if asked, so the first touch
/// does not fault.
///
/// # Safety
/// The range is an anonymous mapping of the caller's.
pub unsafe fn prefault(p: *mut u8, len: usize, huge: bool) {
    unsafe {
        if huge {
            let _ = madvise(p.cast(), len, Advice::LinuxHugepage);
        }
        let _ = madvise(p.cast(), len, Advice::LinuxPopulateWrite);
    }
}

/// Gives the pages back. The range stays mapped and reads as zeros.
///
/// # Safety
/// The range is an anonymous mapping of the caller's, and nothing in it is
/// needed any more.
pub unsafe fn discard(p: *mut u8, len: usize) {
    let _ = unsafe { madvise(p.cast(), len, Advice::LinuxDontNeed) };
}

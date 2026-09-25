//! Stackful coroutines with mmap'd stacks. A fiber is resumed by a worker
//! thread and yields back to it; a suspended fiber can be resumed from any
//! thread because its whole state lives on its own stack plus `sp`.
//!
//! The switch saves the callee-saved registers on the current stack and
//! swaps stack pointers. Nothing else is saved: the compiler treats the
//! switch as an ordinary call, so caller-saved registers are already dead.

use std::sync::atomic::{AtomicU8, Ordering};

pub const DEFAULT_STACK: usize = 256 << 20;
const GUARD: usize = 4096;

/// Address space the fiber stacks of one runtime may reserve. The mappings
/// are `MAP_NORESERVE`, so they cost address space and page tables, not
/// memory; the cap turns an absurd `--workers` or `IETSP_FIBER_STACK_MB`
/// into an error at startup instead of a failed mmap in the middle of a run.
pub const MAX_RESERVE: usize = 64 << 30;

/// A fiber stack that could not be reserved.
#[derive(Debug)]
pub struct StackError {
    /// Bytes the reservation asked for.
    pub bytes: usize,
    pub why: Why,
}

#[derive(Debug)]
pub enum Why {
    /// More than `MAX_RESERVE`, before anything was mapped.
    OverLimit,
    Mmap(std::io::Error),
}

impl std::fmt::Display for StackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "cannot reserve {} MiB of fiber stacks: ",
            self.bytes >> 20
        )?;
        match &self.why {
            Why::OverLimit => write!(
                f,
                "over the {} MiB limit, lower --workers or IETSP_FIBER_STACK_MB",
                MAX_RESERVE >> 20
            ),
            Why::Mmap(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for StackError {}

/// Check the reservation `workers` fibers of `stack_size` bytes make.
pub fn check_reservation(workers: usize, stack_size: usize) -> Result<(), StackError> {
    let bytes = stack_size.saturating_mul(workers);
    if bytes > MAX_RESERVE {
        return Err(StackError {
            bytes,
            why: Why::OverLimit,
        });
    }
    Ok(())
}

/// Fiber lifecycle. Transitions are documented in docs/explanation/evaluation.md.
pub const NEW: u8 = 0;
pub const RUNNING: u8 = 1;
pub const PARKING: u8 = 2;
pub const WOKEN: u8 = 3;
pub const SUSPENDED: u8 = 4;
pub const RUNNABLE: u8 = 5;
pub const FINISHED: u8 = 6;

pub struct Stack {
    base: *mut u8,
    size: usize,
}

impl Stack {
    pub fn new(size: usize) -> Result<Stack, StackError> {
        // SAFETY: anonymous private mapping; the low page is made
        // inaccessible as a guard. NORESERVE keeps large reservations cheap.
        let base = unsafe {
            let p = libc::mmap(
                std::ptr::null_mut(),
                size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE,
                -1,
                0,
            );
            if p == libc::MAP_FAILED {
                return Err(StackError {
                    bytes: size,
                    why: Why::Mmap(std::io::Error::last_os_error()),
                });
            }
            libc::mprotect(p, GUARD, libc::PROT_NONE);
            p as *mut u8
        };
        Ok(Stack { base, size })
    }

    #[inline]
    pub fn top(&self) -> *mut u8 {
        // SAFETY: base + size is one past the mapping, 16-byte aligned.
        unsafe { self.base.add(self.size) }
    }

    /// Return every page except the top `keep` bytes to the OS. Only valid
    /// while the fiber is not running (its frames are garbage).
    pub fn release(&self, keep: usize) {
        let len = self.size - GUARD - keep;
        // SAFETY: range is inside the mapping and above the guard page.
        unsafe {
            libc::madvise(self.base.add(GUARD) as *mut _, len, libc::MADV_DONTNEED);
        }
    }

    /// Number of resident bytes, for diagnostics.
    pub fn resident(&self) -> usize {
        let pages = self.size / 4096;
        let mut v = vec![0u8; pages];
        // SAFETY: vector has one byte per page of the mapping.
        unsafe { libc::mincore(self.base as *mut _, self.size, v.as_mut_ptr()) };
        v.iter().filter(|b| **b & 1 != 0).count() * 4096
    }
}

// Stacks are leaked with their fiber; nothing to drop.

pub type Entry = unsafe fn(*mut ());

pub struct Fiber {
    /// Saved stack pointer of this fiber while suspended.
    sp: *mut u8,
    /// Saved stack pointer of the resumer while this fiber runs.
    caller_sp: *mut u8,
    pub stack: Stack,
    pub state: AtomicU8,
    entry: Entry,
    arg: *mut (),
}

impl Fiber {
    pub fn new(stack_size: usize, entry: Entry, arg: *mut ()) -> Result<Fiber, StackError> {
        Ok(Fiber {
            sp: std::ptr::null_mut(),
            caller_sp: std::ptr::null_mut(),
            stack: Stack::new(stack_size)?,
            state: AtomicU8::new(NEW),
            entry,
            arg,
        })
    }

    /// Rewind a NEW or FINISHED fiber so the next resume starts `entry`.
    /// The stack is seeded at resume time, so the fiber may still move
    /// between `reset` and its first `resume`.
    pub fn reset(&mut self, entry: Entry, arg: *mut ()) {
        debug_assert!(matches!(self.state.load(Ordering::Relaxed), NEW | FINISHED));
        self.entry = entry;
        self.arg = arg;
        self.sp = std::ptr::null_mut();
        self.state.store(NEW, Ordering::Relaxed);
    }

    /// Switch onto the fiber. Returns when it yields or finishes. The
    /// caller must own the fiber (state NEW or RUNNABLE) and must set
    /// `state` to RUNNING first.
    ///
    /// # Safety
    /// Exactly one thread resumes a given fiber at a time.
    #[inline(never)]
    pub unsafe fn resume(&mut self) {
        if self.sp.is_null() {
            self.sp = seed_stack(self.stack.top(), self as *mut Fiber);
        }
        unsafe { switch(&mut self.caller_sp, self.sp) };
    }

    /// Switch back to the resumer. Called on the fiber's own stack.
    ///
    /// # Safety
    /// Must be called from code running on this fiber's stack.
    #[inline(never)]
    pub unsafe fn yield_(&mut self) {
        unsafe { switch(&mut self.sp, self.caller_sp) };
    }
}

/// First frame of every fiber: runs the entry, marks FINISHED, switches
/// back for good. Never returns.
unsafe extern "C" fn fiber_main(f: *mut Fiber) -> ! {
    // SAFETY: `f` is the fiber whose stack we are on; it outlives us.
    unsafe {
        let fiber = &mut *f;
        (fiber.entry)(fiber.arg);
        fiber.state.store(FINISHED, Ordering::Release);
        fiber.yield_();
    }
    unreachable!("finished fiber resumed")
}

#[cfg(target_arch = "x86_64")]
mod arch {
    use super::Fiber;

    /// Saves rbx rbp r12-r15 mxcsr x87cw, stores rsp into `*save`, loads
    /// `load` as rsp, restores, returns into the target's frame.
    #[unsafe(naked)]
    pub unsafe extern "C" fn switch(save: *mut *mut u8, load: *mut u8) {
        core::arch::naked_asm!(
            "push rbp",
            "push rbx",
            "push r12",
            "push r13",
            "push r14",
            "push r15",
            "sub rsp, 8",
            "stmxcsr [rsp]",
            "fnstcw [rsp + 4]",
            "mov [rdi], rsp",
            "mov rsp, rsi",
            "ldmxcsr [rsp]",
            "fldcw [rsp + 4]",
            "add rsp, 8",
            "pop r15",
            "pop r14",
            "pop r13",
            "pop r12",
            "pop rbx",
            "pop rbp",
            "ret",
        )
    }

    /// Entered by the first `ret` of a fresh fiber with the fiber pointer
    /// in r12 and rsp 16-byte aligned.
    #[unsafe(naked)]
    unsafe extern "C" fn trampoline() {
        core::arch::naked_asm!(
            "mov rdi, r12",
            "call {main}",
            "ud2",
            main = sym super::fiber_main,
        )
    }

    /// Lay out the frame `switch` will pop: fp control word, r15..rbp, then
    /// the return address (the trampoline).
    pub fn seed_stack(top: *mut u8, fiber: *mut Fiber) -> *mut u8 {
        // SAFETY: writes the 64 bytes below `top`, inside the mapping.
        unsafe {
            let t = top as *mut u64;
            *t.sub(1) = trampoline as *const () as usize as u64; // ret
            *t.sub(2) = 0; // rbp
            *t.sub(3) = 0; // rbx
            *t.sub(4) = fiber as u64; // r12
            *t.sub(5) = 0; // r13
            *t.sub(6) = 0; // r14
            *t.sub(7) = 0; // r15
            *t.sub(8) = 0x1F80 | (0x037F << 32); // mxcsr, x87 cw
            t.sub(8) as *mut u8
        }
    }
}

#[cfg(target_arch = "aarch64")]
mod arch {
    use super::Fiber;

    /// Saves x19-x30, d8-d15, fpcr; swaps sp. Untested: no aarch64 box.
    #[unsafe(naked)]
    pub unsafe extern "C" fn switch(save: *mut *mut u8, load: *mut u8) {
        core::arch::naked_asm!(
            "sub sp, sp, #176",
            "stp x19, x20, [sp, #0]",
            "stp x21, x22, [sp, #16]",
            "stp x23, x24, [sp, #32]",
            "stp x25, x26, [sp, #48]",
            "stp x27, x28, [sp, #64]",
            "stp x29, x30, [sp, #80]",
            "stp d8, d9, [sp, #96]",
            "stp d10, d11, [sp, #112]",
            "stp d12, d13, [sp, #128]",
            "stp d14, d15, [sp, #144]",
            "mrs x9, fpcr",
            "str x9, [sp, #160]",
            "mov x9, sp",
            "str x9, [x0]",
            "mov sp, x1",
            "ldr x9, [sp, #160]",
            "msr fpcr, x9",
            "ldp d14, d15, [sp, #144]",
            "ldp d12, d13, [sp, #128]",
            "ldp d10, d11, [sp, #112]",
            "ldp d8, d9, [sp, #96]",
            "ldp x29, x30, [sp, #80]",
            "ldp x27, x28, [sp, #64]",
            "ldp x25, x26, [sp, #48]",
            "ldp x23, x24, [sp, #32]",
            "ldp x21, x22, [sp, #16]",
            "ldp x19, x20, [sp, #0]",
            "add sp, sp, #176",
            "ret",
        )
    }

    #[unsafe(naked)]
    unsafe extern "C" fn trampoline() {
        core::arch::naked_asm!(
            "mov x0, x19",
            "bl {main}",
            "brk #1",
            main = sym super::fiber_main,
        )
    }

    pub fn seed_stack(top: *mut u8, fiber: *mut Fiber) -> *mut u8 {
        // SAFETY: writes 176 bytes below `top`, inside the mapping.
        unsafe {
            let sp = top.sub(176);
            std::ptr::write_bytes(sp, 0, 176);
            let t = sp as *mut u64;
            *t = fiber as u64; // x19
            *t.add(11) = trampoline as *const () as usize as u64; // x30
            sp
        }
    }
}

use arch::{seed_stack, switch};

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    struct Counter {
        fiber: *mut Fiber,
        n: AtomicUsize,
    }

    unsafe fn count_entry(p: *mut ()) {
        let c = unsafe { &*(p as *const Counter) };
        for _ in 0..3 {
            c.n.fetch_add(1, Ordering::Relaxed);
            unsafe { (*c.fiber).yield_() };
        }
    }

    #[test]
    fn resume_yield_finish() {
        let c = Box::leak(Box::new(Counter {
            fiber: std::ptr::null_mut(),
            n: AtomicUsize::new(0),
        }));
        let f = Box::leak(Box::new(
            Fiber::new(1 << 20, count_entry, c as *mut Counter as *mut ()).expect("fiber stack"),
        ));
        c.fiber = f;
        for i in 1..=3 {
            f.state.store(RUNNING, Ordering::Relaxed);
            unsafe { f.resume() };
            assert_eq!(c.n.load(Ordering::Relaxed), i);
            assert_eq!(f.state.load(Ordering::Relaxed), RUNNING);
        }
        unsafe { f.resume() };
        assert_eq!(f.state.load(Ordering::Relaxed), FINISHED);
        f.reset(count_entry, c as *mut Counter as *mut ());
        f.state.store(RUNNING, Ordering::Relaxed);
        unsafe { f.resume() };
        assert_eq!(c.n.load(Ordering::Relaxed), 4);
    }

    #[test]
    fn migrates_between_threads() {
        let c = Box::leak(Box::new(Counter {
            fiber: std::ptr::null_mut(),
            n: AtomicUsize::new(0),
        }));
        let f = Box::leak(Box::new(
            Fiber::new(1 << 20, count_entry, c as *mut Counter as *mut ()).expect("fiber stack"),
        ));
        c.fiber = f;
        let fp = f as *mut Fiber as usize;
        for i in 1..=3 {
            std::thread::spawn(move || {
                let f = unsafe { &mut *(fp as *mut Fiber) };
                f.state.store(RUNNING, Ordering::Relaxed);
                unsafe { f.resume() };
            })
            .join()
            .unwrap();
            assert_eq!(c.n.load(Ordering::Relaxed), i);
        }
    }

    #[test]
    fn reservation_is_capped_and_mmap_failure_is_an_error() {
        check_reservation(4, DEFAULT_STACK).expect("the default fits");
        let e = check_reservation(1024, DEFAULT_STACK).expect_err("over the cap");
        assert_eq!(
            e.to_string(),
            format!(
                "cannot reserve 262144 MiB of fiber stacks: over the {} MiB limit, \
                 lower --workers or IETSP_FIBER_STACK_MB",
                MAX_RESERVE >> 20
            )
        );
        let Err(e) = Stack::new(usize::MAX - 4095) else {
            panic!("the whole address space was mapped");
        };
        assert!(
            matches!(e.why, Why::Mmap(_)) && e.to_string().starts_with("cannot reserve"),
            "unexpected message: {e}"
        );
    }

    #[inline(never)]
    fn deep(n: u64) -> u64 {
        let buf = [n; 64];
        let p = std::hint::black_box(&buf);
        if n == 0 {
            p[3]
        } else {
            deep(n - 1) + std::hint::black_box(p)[5] - n
        }
    }

    unsafe fn deep_entry(p: *mut ()) {
        let c = unsafe { &*(p as *const Counter) };
        c.n.store(deep(20000) as usize, Ordering::Relaxed);
    }

    #[test]
    fn large_stack_use_and_release() {
        let c = Box::leak(Box::new(Counter {
            fiber: std::ptr::null_mut(),
            n: AtomicUsize::new(0),
        }));
        let f = Box::leak(Box::new(
            Fiber::new(64 << 20, deep_entry, c as *mut Counter as *mut ()).expect("fiber stack"),
        ));
        f.state.store(RUNNING, Ordering::Relaxed);
        unsafe { f.resume() };
        assert_eq!(f.state.load(Ordering::Relaxed), FINISHED);
        assert!(f.stack.resident() > 20000 * 512);
        f.stack.release(64 << 10);
        assert!(f.stack.resident() <= 64 << 10);
    }
}

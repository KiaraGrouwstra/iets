//! Worker threads, per-worker deques, parking, and the fiber wake
//! handshake. Evaluator-agnostic: the `Driver` supplies the per-fiber
//! payload and runs tasks.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering, fence};

use super::deque::Deque;
use super::fiber::{self, Fiber};
use crate::runtime::Arena;

pub const READY_CAP: usize = 4096;
pub const TASK_CAP: usize = 4096;
const PREWARM: usize = 4;
/// Pool fibers beyond this many are trimmed when the worker parks.
const TRIM_ABOVE: usize = 16;
const SPIN_SCANS: u32 = 2000;
const WARM_STACK: usize = 64 << 10;

/// A unit of not-yet-started work. `obj` is a `Word` or anything else the
/// driver packs; `start`/`len` address a range inside it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Task {
    pub kind: u8,
    pub obj: u64,
    pub start: u32,
    pub len: u32,
}

impl Task {
    #[inline]
    fn pack(self) -> (u64, u64) {
        (
            self.obj,
            self.kind as u64 | ((self.start as u64 & 0xFF_FFFF) << 8) | ((self.len as u64) << 32),
        )
    }

    #[inline]
    fn unpack(a: u64, b: u64) -> Task {
        Task {
            kind: b as u8,
            obj: a,
            start: ((b >> 8) & 0xFF_FFFF) as u32,
            len: (b >> 32) as u32,
        }
    }
}

pub trait Driver: Sync {
    /// Payload for a new fiber slot (an `Evaluator`).
    fn new_user(&self, rt: &Runtime) -> *mut ();
    /// Run `slot.task`. Called on the fiber's own stack.
    fn run_task(&self, rt: &Runtime, slot: &mut Slot);
    /// Called on the worker's stack right before switching into `slot`
    /// and right after it switched out (arena swap).
    fn on_switch(&self, slot: &mut Slot, worker: &mut Worker);
}

pub struct Slot {
    pub fiber: Fiber,
    /// Claimer id: nonzero, unique per slot, stable across migration.
    pub id: u32,
    pub cancel: AtomicBool,
    pub is_main: bool,
    pub user: *mut (),
    pub task: Task,
    main_entry: Option<Box<dyn FnOnce(&mut Slot)>>,
    /// Worker currently running this fiber. Valid while RUNNING.
    pub worker: *mut Worker,
    pub rt: *const Runtime,
    /// Stack pages beyond the warm top were returned to the OS.
    trimmed: bool,
}

pub struct Worker {
    pub idx: usize,
    pub arena: Arena,
    pool: Vec<*mut Slot>,
    spinning: bool,
    rng: u64,
    pub tasks_run: u64,
    pub steals: u64,
    pub parks: u64,
    pub fibers_created: u64,
}

struct Lanes {
    ready: Deque,
    urgent: Deque,
    spec: Deque,
}

#[repr(align(128))]
struct Padded<T>(T);

pub struct Runtime {
    pub n: usize,
    lanes: Vec<Lanes>,
    overflow: Mutex<Vec<*mut Slot>>,
    overflow_len: Padded<AtomicUsize>,
    /// Summaries of queued work so idle workers poll two words instead of
    /// touching every lane's deque lines.
    pending_tasks: Padded<AtomicUsize>,
    pending_ready: Padded<AtomicUsize>,
    /// Workers in the pre-park spin; capped so a wide pool does not keep
    /// every idle core hammering the summary lines.
    spinners: Padded<AtomicUsize>,
    sleepers: Padded<AtomicUsize>,
    epoch: Padded<AtomicU32>,
    pub shutdown: AtomicBool,
    pub main_done: AtomicBool,
    /// Set once the main result exists: helpers stop starting new work.
    pub suppress: AtomicBool,
    next_id: AtomicU32,
    all: Mutex<Vec<*mut Slot>>,
    driver: Box<dyn Driver>,
    stack_size: usize,
    pub waiters: super::thunk::WaiterTable,
    pub deadlocks: AtomicUsize,
    pub stat_tasks: AtomicUsize,
    pub stat_steals: AtomicUsize,
    pub stat_parks: AtomicUsize,
    pub stat_main_parks: AtomicUsize,
    /// Nanoseconds the main fiber spent parked.
    pub stat_main_wait_ns: AtomicUsize,
    /// Nanoseconds all workers spent inside fibers.
    pub stat_busy_ns: AtomicUsize,
    pub stat_fiber_parks: AtomicUsize,
}

// SAFETY: raw slot pointers are only dereferenced under the ownership
// rules in docs/parallel.md (a fiber is resumed by one thread at a time;
// slot fields other than atomics are touched only by its current owner).
unsafe impl Sync for Runtime {}
unsafe impl Send for Runtime {}
unsafe impl Send for Worker {}

impl Runtime {
    /// Create the runtime and spawn workers `1..n`. The caller's thread is
    /// worker 0 and must call `run_main`.
    pub fn start(n: usize, driver: Box<dyn Driver>, stack_size: usize) -> &'static Runtime {
        assert!(n >= 1);
        let rt: &'static Runtime = Box::leak(Box::new(Runtime {
            n,
            lanes: (0..n)
                .map(|_| Lanes {
                    ready: Deque::new(READY_CAP),
                    urgent: Deque::new(TASK_CAP),
                    spec: Deque::new(TASK_CAP),
                })
                .collect(),
            overflow: Mutex::new(Vec::new()),
            overflow_len: Padded(AtomicUsize::new(0)),
            pending_tasks: Padded(AtomicUsize::new(0)),
            pending_ready: Padded(AtomicUsize::new(0)),
            spinners: Padded(AtomicUsize::new(0)),
            sleepers: Padded(AtomicUsize::new(0)),
            epoch: Padded(AtomicU32::new(0)),
            shutdown: AtomicBool::new(false),
            main_done: AtomicBool::new(false),
            suppress: AtomicBool::new(false),
            next_id: AtomicU32::new(1),
            all: Mutex::new(Vec::new()),
            driver,
            stack_size,
            waiters: super::thunk::WaiterTable::new(),
            deadlocks: AtomicUsize::new(0),
            stat_tasks: AtomicUsize::new(0),
            stat_steals: AtomicUsize::new(0),
            stat_parks: AtomicUsize::new(0),
            stat_main_parks: AtomicUsize::new(0),
            stat_main_wait_ns: AtomicUsize::new(0),
            stat_busy_ns: AtomicUsize::new(0),
            stat_fiber_parks: AtomicUsize::new(0),
        }));
        for i in 1..n {
            std::thread::Builder::new()
                .name(format!("ietsp-w{i}"))
                .stack_size(4 << 20)
                .spawn(move || {
                    let mut w = Worker::new(i);
                    rt.prewarm(&mut w);
                    rt.drain_loop(&mut w, || rt.shutdown.load(Ordering::Acquire));
                })
                .expect("spawn worker");
        }
        rt
    }

    /// Run `entry` in the main fiber on this thread (worker 0), draining
    /// other work while it is parked. Returns when `entry` has returned.
    pub fn run_main(&'static self, user: *mut (), entry: Box<dyn FnOnce(&mut Slot)>) {
        let mut w = Worker::new(0);
        let slot = self.new_slot(&mut w, user);
        // SAFETY: fresh slot, owned by this thread.
        unsafe {
            (*slot).is_main = true;
            (*slot).main_entry = Some(entry);
            (*slot).fiber.reset(slot_entry, slot as *mut ());
        }
        self.run_slot(&mut w, slot);
        self.drain_loop(&mut w, || self.main_done.load(Ordering::Acquire));
    }

    pub fn driver(&self) -> &dyn Driver {
        &*self.driver
    }

    fn drain_loop(&self, w: &mut Worker, done: impl Fn() -> bool) {
        let mut idle: u32 = 0;
        loop {
            if done() {
                return;
            }
            if let Some(s) = self.take_ready(w) {
                idle = 0;
                self.stop_spinning(w);
                self.run_slot(w, s);
                continue;
            }
            if let Some(t) = self.take_task(w) {
                idle = 0;
                self.stop_spinning(w);
                self.run_task(w, t);
                continue;
            }
            idle += 1;
            if idle < SPIN_SCANS && self.may_spin(w) {
                // Poll the two summary words until something appears.
                for _ in 0..64 {
                    if self.has_work() {
                        break;
                    }
                    std::hint::spin_loop();
                }
                continue;
            }
            if w.spinning {
                w.spinning = false;
                self.spinners.0.fetch_sub(1, Ordering::Relaxed);
            }
            idle = 0;
            self.park(w, &done);
        }
    }

    /// Worker 0 always spins (it carries the demand fiber); helpers only
    /// while fewer than max(3, n/4) are spinning.
    #[inline]
    fn may_spin(&self, w: &mut Worker) -> bool {
        if w.spinning || w.idx == 0 {
            return true;
        }
        let cap = (self.n / 4).max(3);
        let cur = self.spinners.0.load(Ordering::Relaxed);
        if cur < cap
            && self
                .spinners
                .0
                .compare_exchange(cur, cur + 1, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
        {
            w.spinning = true;
            return true;
        }
        false
    }

    #[inline]
    fn stop_spinning(&self, w: &mut Worker) {
        if w.spinning {
            w.spinning = false;
            self.spinners.0.fetch_sub(1, Ordering::Relaxed);
        }
    }

    // ---- fibers -----------------------------------------------------------

    fn new_slot(&self, w: &mut Worker, user: *mut ()) -> *mut Slot {
        w.fibers_created += 1;
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let slot = Box::into_raw(Box::new(Slot {
            fiber: Fiber::new(self.stack_size, slot_entry, std::ptr::null_mut()),
            id,
            cancel: AtomicBool::new(false),
            is_main: false,
            user,
            task: Task {
                kind: 0,
                obj: 0,
                start: 0,
                len: 0,
            },
            main_entry: None,
            worker: std::ptr::null_mut(),
            rt: self,
            trimmed: false,
        }));
        self.all.lock().unwrap().push(slot);
        slot
    }

    fn prewarm(&self, w: &mut Worker) {
        for _ in 0..PREWARM {
            let user = self.driver.new_user(self);
            let s = self.new_slot(w, user);
            w.pool.push(s);
        }
    }

    fn run_task(&self, w: &mut Worker, t: Task) {
        let slot = match w.pool.pop() {
            Some(s) => s,
            None => {
                let user = self.driver.new_user(self);
                self.new_slot(w, user)
            }
        };
        w.tasks_run += 1;
        self.stat_tasks.fetch_add(1, Ordering::Relaxed);
        // SAFETY: the slot is idle (in the pool) and owned by this thread.
        unsafe {
            (*slot).task = t;
            (*slot).cancel.store(false, Ordering::Relaxed);
            (*slot).fiber.reset(slot_entry, slot as *mut ());
        }
        self.run_slot(w, slot);
    }

    /// Resume `slot` on this worker. Requires ownership: the slot is NEW,
    /// or this thread took its single ready entry.
    fn run_slot(&self, w: &mut Worker, slot: *mut Slot) {
        // SAFETY: ownership per the wake handshake (docs/parallel.md).
        unsafe {
            let s = &mut *slot;
            s.fiber.state.store(fiber::RUNNING, Ordering::Relaxed);
            s.worker = w;
            self.driver.on_switch(s, w);
            let t0 = std::time::Instant::now();
            s.fiber.resume();
            self.stat_busy_ns
                .fetch_add(t0.elapsed().as_nanos() as usize, Ordering::Relaxed);
            self.driver.on_switch(s, w);
            match s.fiber.state.load(Ordering::Acquire) {
                fiber::FINISHED => {
                    s.worker = std::ptr::null_mut();
                    w.pool.push(slot);
                }
                // A waker may already have moved PARKING to WOKEN.
                fiber::PARKING | fiber::WOKEN => {
                    if s.fiber.state.swap(fiber::SUSPENDED, Ordering::AcqRel) == fiber::WOKEN {
                        self.push_ready(w, slot);
                    }
                }
                st => panic!("fiber switched out in state {st}"),
            }
        }
    }

    /// Make a parked fiber runnable. Safe to call from any thread and at
    /// any time; a spurious wake just makes the fiber re-check its wait.
    pub fn wake(&self, w: &mut Worker, slot: *mut Slot) {
        // SAFETY: only the atomic state is touched.
        let st = unsafe { &(*slot).fiber.state };
        loop {
            match st.load(Ordering::Acquire) {
                fiber::SUSPENDED => {
                    if st
                        .compare_exchange(
                            fiber::SUSPENDED,
                            fiber::RUNNABLE,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_ok()
                    {
                        self.push_ready(w, slot);
                        return;
                    }
                }
                fiber::PARKING => {
                    if st
                        .compare_exchange(
                            fiber::PARKING,
                            fiber::WOKEN,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_ok()
                    {
                        return;
                    }
                }
                _ => return,
            }
        }
    }

    fn push_ready(&self, w: &mut Worker, slot: *mut Slot) {
        self.pending_ready.0.fetch_add(1, Ordering::SeqCst);
        if !self.lanes[w.idx].ready.push(slot as u64, 0) {
            self.overflow.lock().unwrap().push(slot);
            self.overflow_len.0.fetch_add(1, Ordering::Release);
        }
        self.notify();
    }

    fn take_ready(&self, w: &mut Worker) -> Option<*mut Slot> {
        if self.pending_ready.0.load(Ordering::Acquire) == 0 {
            return None;
        }
        let got = self.take_ready_scan(w);
        if got.is_some() {
            self.pending_ready.0.fetch_sub(1, Ordering::SeqCst);
        }
        got
    }

    fn take_ready_scan(&self, w: &mut Worker) -> Option<*mut Slot> {
        if let Some((a, _)) = self.lanes[w.idx].ready.pop() {
            return Some(a as *mut Slot);
        }
        if self.overflow_len.0.load(Ordering::Acquire) > 0 {
            let mut o = self.overflow.lock().unwrap();
            if let Some(s) = o.pop() {
                self.overflow_len.0.fetch_sub(1, Ordering::Release);
                return Some(s);
            }
        }
        let start = w.next_victim();
        for k in 0..self.n {
            let v = (start + k) % self.n;
            if v == w.idx {
                continue;
            }
            if let Some((a, _)) = self.lanes[v].ready.steal() {
                w.steals += 1;
                self.stat_steals.fetch_add(1, Ordering::Relaxed);
                return Some(a as *mut Slot);
            }
        }
        None
    }

    // ---- tasks ------------------------------------------------------------

    /// Owner push from the worker currently running the caller.
    pub fn submit_urgent(&self, w: &Worker, t: Task) -> bool {
        let (a, b) = t.pack();
        if self.lanes[w.idx].urgent.push(a, b) {
            self.pending_tasks.0.fetch_add(1, Ordering::SeqCst);
            self.notify();
            true
        } else {
            false
        }
    }

    pub fn submit_spec(&self, w: &Worker, t: Task) -> bool {
        let (a, b) = t.pack();
        if self.lanes[w.idx].spec.push(a, b) {
            self.pending_tasks.0.fetch_add(1, Ordering::SeqCst);
            self.notify();
            true
        } else {
            false
        }
    }

    fn take_task(&self, w: &mut Worker) -> Option<Task> {
        if self.suppress.load(Ordering::Relaxed)
            || self.pending_tasks.0.load(Ordering::Acquire) == 0
        {
            return None;
        }
        let got = self.take_task_scan(w);
        if got.is_some() {
            self.pending_tasks.0.fetch_sub(1, Ordering::SeqCst);
        }
        got
    }

    fn take_task_scan(&self, w: &mut Worker) -> Option<Task> {
        if let Some((a, b)) = self.lanes[w.idx].urgent.pop() {
            return Some(Task::unpack(a, b));
        }
        let start = w.next_victim();
        for k in 0..self.n {
            let v = (start + k) % self.n;
            if v == w.idx {
                continue;
            }
            if let Some((a, b)) = self.lanes[v].urgent.steal() {
                w.steals += 1;
                return Some(Task::unpack(a, b));
            }
        }
        if let Some((a, b)) = self.lanes[w.idx].spec.pop() {
            return Some(Task::unpack(a, b));
        }
        for k in 0..self.n {
            let v = (start + k) % self.n;
            if v == w.idx {
                continue;
            }
            if let Some((a, b)) = self.lanes[v].spec.steal() {
                w.steals += 1;
                return Some(Task::unpack(a, b));
            }
        }
        None
    }

    #[inline]
    fn has_work(&self) -> bool {
        self.pending_ready.0.load(Ordering::Acquire) > 0
            || (!self.suppress.load(Ordering::Relaxed)
                && self.pending_tasks.0.load(Ordering::Acquire) > 0)
    }

    // ---- parking ----------------------------------------------------------

    fn notify(&self) {
        fence(Ordering::SeqCst);
        if self.sleepers.0.load(Ordering::Relaxed) > 0 {
            self.epoch.0.fetch_add(1, Ordering::Release);
            futex_wake(&self.epoch.0, 1);
        }
    }

    pub fn wake_all(&self) {
        self.epoch.0.fetch_add(1, Ordering::Release);
        futex_wake(&self.epoch.0, i32::MAX);
    }

    fn park(&self, w: &mut Worker, done: &impl Fn() -> bool) {
        w.parks += 1;
        self.stat_parks.fetch_add(1, Ordering::Relaxed);
        self.trim_pool(w);
        let s = self.sleepers.0.fetch_add(1, Ordering::SeqCst) + 1;
        let e = self.epoch.0.load(Ordering::Acquire);
        if done() || self.has_work() {
            self.sleepers.0.fetch_sub(1, Ordering::SeqCst);
            return;
        }
        if s == self.n && !self.main_done.load(Ordering::Acquire) {
            // Everyone is asleep and main is not finished: every fiber is
            // parked on another parked fiber. Break the cycle.
            self.resolve_deadlock(w);
            self.sleepers.0.fetch_sub(1, Ordering::SeqCst);
            return;
        }
        futex_wait(&self.epoch.0, e);
        self.sleepers.0.fetch_sub(1, Ordering::SeqCst);
    }

    fn resolve_deadlock(&self, w: &mut Worker) {
        let all = self.all.lock().unwrap();
        let mut woken = 0;
        for &s in all.iter() {
            // SAFETY: slots live forever; only atomics are read here.
            let slot = unsafe { &*s };
            if slot.is_main || slot.fiber.state.load(Ordering::Acquire) != fiber::SUSPENDED {
                continue;
            }
            slot.cancel.store(true, Ordering::Release);
            self.wake(w, s);
            woken += 1;
        }
        drop(all);
        self.deadlocks.fetch_add(1, Ordering::Relaxed);
        if woken == 0 {
            if self.has_work() {
                return;
            }
            eprintln!("ietsp: internal error: all workers idle with the main fiber parked");
            std::process::abort();
        }
    }

    /// Give the stack pages of idle pool fibers beyond the prewarm count
    /// back to the OS. Runs only when the worker is about to park.
    fn trim_pool(&self, w: &mut Worker) {
        if w.parks % 64 != 0 {
            return;
        }
        for &s in w.pool.iter().skip(TRIM_ABOVE) {
            // SAFETY: pool fibers are idle and owned by this worker.
            let slot = unsafe { &mut *s };
            if !slot.trimmed {
                slot.fiber.stack.release(WARM_STACK);
                slot.trimmed = true;
            }
        }
    }

    pub fn stats_fibers(&self) -> usize {
        self.all.lock().unwrap().len()
    }
}

unsafe fn slot_entry(p: *mut ()) {
    // SAFETY: `p` is the slot whose fiber stack we run on.
    let slot = unsafe { &mut *(p as *mut Slot) };
    let rt = unsafe { &*slot.rt };
    if let Some(entry) = slot.main_entry.take() {
        entry(slot);
        rt.main_done.store(true, Ordering::Release);
        rt.suppress.store(true, Ordering::Release);
        rt.shutdown.store(true, Ordering::Release);
        rt.wake_all();
    } else {
        // SAFETY: `trimmed` only describes idle pages; running clears it.
        slot.trimmed = false;
        rt.driver.run_task(rt, slot);
    }
}

impl Slot {
    /// Park the current fiber. The caller has already stored PARKING (or a
    /// waker may have moved it to WOKEN). Returns when resumed.
    ///
    /// # Safety
    /// Must be called on this slot's own fiber stack.
    #[inline(never)]
    pub unsafe fn park(&mut self) {
        debug_assert!(matches!(
            self.fiber.state.load(Ordering::Relaxed),
            fiber::PARKING | fiber::WOKEN
        ));
        unsafe { self.fiber.yield_() };
    }

    #[inline]
    pub fn worker(&self) -> &mut Worker {
        // SAFETY: valid while the fiber runs; set by `run_slot`.
        unsafe { &mut *self.worker }
    }

    #[inline]
    pub fn runtime(&self) -> &'static Runtime {
        // SAFETY: the runtime is leaked and outlives every slot.
        unsafe { &*self.rt }
    }
}

impl Worker {
    fn new(idx: usize) -> Worker {
        Worker {
            idx,
            arena: Arena::new(),
            pool: Vec::new(),
            spinning: false,
            rng: 0x9E37_79B9_7F4A_7C15 ^ (idx as u64 + 1),
            tasks_run: 0,
            steals: 0,
            parks: 0,
            fibers_created: 0,
        }
    }

    #[inline]
    fn next_victim(&mut self) -> usize {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        (self.rng >> 32) as usize
    }
}

fn futex_wait(word: &AtomicU32, expected: u32) {
    // SAFETY: plain futex wait on a word that lives forever.
    unsafe {
        libc::syscall(
            libc::SYS_futex,
            word as *const AtomicU32,
            libc::FUTEX_WAIT | libc::FUTEX_PRIVATE_FLAG,
            expected,
            std::ptr::null::<libc::timespec>(),
        );
    }
}

fn futex_wake(word: &AtomicU32, n: i32) {
    // SAFETY: plain futex wake.
    unsafe {
        libc::syscall(
            libc::SYS_futex,
            word as *const AtomicU32,
            libc::FUTEX_WAKE | libc::FUTEX_PRIVATE_FLAG,
            n,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicPtr, AtomicU64};

    struct Counter {
        sum: AtomicU64,
        done: AtomicU64,
        parked: AtomicPtr<Slot>,
    }

    struct TestDriver(&'static Counter);

    const ADD: u8 = 1;
    const SPAWN: u8 = 2;
    const PARK: u8 = 3;
    const WAKE: u8 = 4;

    impl Driver for TestDriver {
        fn new_user(&self, _rt: &Runtime) -> *mut () {
            std::ptr::null_mut()
        }
        fn run_task(&self, rt: &Runtime, slot: &mut Slot) {
            let c = self.0;
            let t = slot.task;
            match t.kind {
                ADD => {
                    for i in t.start..t.start + t.len {
                        c.sum.fetch_add(i as u64, Ordering::Relaxed);
                    }
                    c.done.fetch_add(1, Ordering::Release);
                }
                SPAWN => {
                    for i in 0..t.len {
                        let sub = Task {
                            kind: ADD,
                            obj: 0,
                            start: i * 16,
                            len: 16,
                        };
                        if !rt.submit_urgent(slot.worker(), sub) {
                            self.run_task_inline(c, sub);
                        }
                    }
                    c.done.fetch_add(1, Ordering::Release);
                }
                PARK => {
                    slot.fiber.state.store(fiber::PARKING, Ordering::Release);
                    c.parked.store(slot, Ordering::Release);
                    unsafe { slot.park() };
                    assert!(!slot.cancel.load(Ordering::Relaxed));
                    c.sum.fetch_add(1000, Ordering::Relaxed);
                    c.done.fetch_add(1, Ordering::Release);
                }
                WAKE => loop {
                    let p = c.parked.load(Ordering::Acquire);
                    if !p.is_null() {
                        rt.wake(slot.worker(), p);
                        c.done.fetch_add(1, Ordering::Release);
                        return;
                    }
                    std::hint::spin_loop();
                },
                _ => unreachable!(),
            }
        }
        fn on_switch(&self, _slot: &mut Slot, _w: &mut Worker) {}
    }

    impl TestDriver {
        fn run_task_inline(&self, c: &Counter, t: Task) {
            for i in t.start..t.start + t.len {
                c.sum.fetch_add(i as u64, Ordering::Relaxed);
            }
            c.done.fetch_add(1, Ordering::Release);
        }
    }

    fn counter() -> &'static Counter {
        Box::leak(Box::new(Counter {
            sum: AtomicU64::new(0),
            done: AtomicU64::new(0),
            parked: AtomicPtr::new(std::ptr::null_mut()),
        }))
    }

    fn wait_done(c: &Counter, n: u64) {
        let t0 = std::time::Instant::now();
        while c.done.load(Ordering::Acquire) < n {
            assert!(t0.elapsed().as_secs() < 20, "timeout");
            std::thread::yield_now();
        }
    }

    #[test]
    fn fan_out_from_main() {
        let c = counter();
        let rt = Runtime::start(4, Box::new(TestDriver(c)), 1 << 20);
        rt.run_main(
            std::ptr::null_mut(),
            Box::new(move |slot: &mut Slot| {
                let rt = slot.runtime();
                let n = 64;
                for i in 0..n {
                    let t = Task {
                        kind: SPAWN,
                        obj: 0,
                        start: 0,
                        len: 8,
                    };
                    let _ = i;
                    assert!(rt.submit_urgent(slot.worker(), t));
                }
                // 64 spawners, 8 adds each.
                wait_done(c, 64 * 9);
            }),
        );
        let expect: u64 = 64 * (0..128u64).sum::<u64>();
        assert_eq!(c.sum.load(Ordering::Relaxed), expect);
    }

    #[test]
    fn park_and_wake_across_workers() {
        let c = counter();
        let rt = Runtime::start(3, Box::new(TestDriver(c)), 1 << 20);
        rt.run_main(
            std::ptr::null_mut(),
            Box::new(move |slot: &mut Slot| {
                let rt = slot.runtime();
                let park = Task {
                    kind: PARK,
                    obj: 0,
                    start: 0,
                    len: 0,
                };
                let wake = Task {
                    kind: WAKE,
                    obj: 0,
                    start: 0,
                    len: 0,
                };
                assert!(rt.submit_urgent(slot.worker(), park));
                assert!(rt.submit_urgent(slot.worker(), wake));
                wait_done(c, 2);
            }),
        );
        assert_eq!(c.sum.load(Ordering::Relaxed), 1000);
    }

    #[test]
    fn main_parks_and_is_woken_by_helper() {
        let c = counter();
        let rt = Runtime::start(2, Box::new(TestDriver(c)), 1 << 20);
        rt.run_main(
            std::ptr::null_mut(),
            Box::new(move |slot: &mut Slot| {
                let rt = slot.runtime();
                let wake = Task {
                    kind: WAKE,
                    obj: 0,
                    start: 0,
                    len: 0,
                };
                assert!(rt.submit_urgent(slot.worker(), wake));
                slot.fiber.state.store(fiber::PARKING, Ordering::Release);
                c.parked.store(slot, Ordering::Release);
                unsafe { slot.park() };
                c.sum.fetch_add(7, Ordering::Relaxed);
                wait_done(c, 1);
            }),
        );
        assert_eq!(c.sum.load(Ordering::Relaxed), 7);
    }
}

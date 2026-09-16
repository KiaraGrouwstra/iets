//! Claim / park / publish protocol on a thunk's result `Cell`, and the
//! side table of parked fibers. See docs/parallel.md "Thunk claim
//! protocol".

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use super::fiber;
use super::sched::{Runtime, Slot};
use crate::runtime::{Cell, Word};

const SHARDS: usize = 64;
const SPIN: u32 = 256;
/// Set in a BLACKHOLE word when the waiter table may hold fibers for it.
pub const WAITERS_BIT: u64 = 0x80;

#[inline]
pub fn blackhole(id: u32) -> Word {
    Word(Word::BLACKHOLE.0 | ((id as u64) << 8))
}

#[inline]
pub fn claimer(w: Word) -> u32 {
    (w.0 >> 8) as u32
}

#[inline]
fn atomic(cell: &Cell) -> &AtomicU64 {
    // SAFETY: `Cell` is `#[repr(transparent)]` over `AtomicU64`.
    unsafe { &*(cell as *const Cell as *const AtomicU64) }
}

pub enum Claim {
    /// We own the thunk and must publish or reset it.
    Won,
    Value(Word),
    /// Claimed by this fiber already: infinite recursion.
    Mine,
    /// Claimed by another fiber; the word as observed.
    Busy(Word),
}

#[inline]
pub fn try_claim(cell: &Cell, me: u32) -> Claim {
    match atomic(cell).compare_exchange(
        Word::EMPTY.0,
        blackhole(me).0,
        Ordering::Acquire,
        Ordering::Acquire,
    ) {
        Ok(_) => Claim::Won,
        Err(cur) => {
            let cur = Word(cur);
            if cur.is_blackhole() {
                if claimer(cur) == me {
                    Claim::Mine
                } else {
                    Claim::Busy(cur)
                }
            } else {
                Claim::Value(cur)
            }
        }
    }
}

/// Store `v` (a value, or EMPTY to reset) and wake every parked waiter.
pub fn publish(slot: &Slot, cell: &Cell, v: Word) {
    let old = atomic(cell).swap(v.0, Ordering::AcqRel);
    if old & WAITERS_BIT != 0 {
        let rt = slot.runtime();
        let list = rt.waiters.take(cell);
        let w = slot.worker();
        for f in list {
            rt.wake(w, f);
        }
    }
}

/// Park until the cell leaves the claimed state `cur`. `Err` means the
/// fiber was cancelled while parked.
pub fn wait(slot: &mut Slot, cell: &Cell, cur: Word) -> Result<(), ()> {
    let base = cur.0 & !WAITERS_BIT;
    for _ in 0..SPIN {
        if cell.get().0 & !WAITERS_BIT != base {
            return Ok(());
        }
        std::hint::spin_loop();
    }
    let me = slot as *mut Slot;
    let rt: &Runtime = slot.runtime();
    let shard = rt.waiters.shard(cell);
    {
        let mut g = shard.lock().unwrap();
        let now = cell.get();
        if now.0 & !WAITERS_BIT != base {
            return Ok(());
        }
        // PARKING before the entry is visible, so a waker that finds the
        // entry always sees PARKING or a later state.
        slot.fiber.state.store(fiber::PARKING, Ordering::Release);
        if atomic(cell)
            .compare_exchange(
                now.0,
                now.0 | WAITERS_BIT,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            slot.fiber.state.store(fiber::RUNNING, Ordering::Release);
            return Ok(());
        }
        g.entry(cell as *const Cell as usize).or_default().push(me);
    }
    // SAFETY: we are on this slot's fiber stack.
    unsafe { slot.park() };
    if slot.cancel.load(Ordering::Acquire) {
        return Err(());
    }
    Ok(())
}

pub struct WaiterTable {
    shards: Vec<Mutex<HashMap<usize, Vec<*mut Slot>>>>,
}

// SAFETY: slot pointers are opaque handles here; see `Runtime`.
unsafe impl Sync for WaiterTable {}
unsafe impl Send for WaiterTable {}

impl WaiterTable {
    pub fn new() -> WaiterTable {
        WaiterTable {
            shards: (0..SHARDS).map(|_| Mutex::new(HashMap::new())).collect(),
        }
    }

    #[inline]
    fn shard(&self, cell: &Cell) -> &Mutex<HashMap<usize, Vec<*mut Slot>>> {
        let a = cell as *const Cell as usize;
        &self.shards[(a >> 4) % SHARDS]
    }

    fn take(&self, cell: &Cell) -> Vec<*mut Slot> {
        self.shard(cell)
            .lock()
            .unwrap()
            .remove(&(cell as *const Cell as usize))
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parallel::sched::{Driver, Task, Worker};
    use std::sync::atomic::AtomicUsize;

    /// A synthetic thunk: value = 1 + sum of dependency values.
    struct Node {
        cell: Cell,
        deps: Vec<usize>,
    }

    struct Graph {
        nodes: Vec<Node>,
        done: AtomicUsize,
        parks: AtomicUsize,
    }

    // SAFETY: cells are atomic; deps are immutable.
    unsafe impl Sync for Graph {}

    struct GraphDriver(&'static Graph);

    fn force(g: &Graph, slot: &mut Slot, i: usize) -> i64 {
        let cell = &g.nodes[i].cell;
        loop {
            match try_claim(cell, slot.id) {
                Claim::Won => {
                    // Some work per node so claims overlap and fibers park.
                    let mut x = i as u64;
                    for _ in 0..3000 {
                        x = x
                            .wrapping_mul(6364136223846793005)
                            .wrapping_add(1442695040888963407);
                    }
                    std::hint::black_box(x);
                    let mut v = 1;
                    for &d in &g.nodes[i].deps {
                        v += force(g, slot, d);
                    }
                    publish(slot, cell, Word::imm_int(v));
                    return v;
                }
                Claim::Value(w) => return w.imm_int_value(),
                Claim::Mine => panic!("cycle"),
                Claim::Busy(cur) => {
                    g.parks.fetch_add(1, Ordering::Relaxed);
                    wait(slot, cell, cur).expect("not cancelled");
                }
            }
        }
    }

    impl Driver for GraphDriver {
        fn new_user(&self, _rt: &Runtime) -> *mut () {
            std::ptr::null_mut()
        }
        fn run_task(&self, _rt: &Runtime, slot: &mut Slot) {
            let t = slot.task;
            for i in t.start..t.start + t.len {
                force(self.0, slot, i as usize);
            }
            self.0.done.fetch_add(1, Ordering::Release);
        }
        fn on_switch(&self, _slot: &mut Slot, _w: &mut Worker) {}
    }

    fn expected(g: &Graph, i: usize, memo: &mut Vec<Option<i64>>) -> i64 {
        if let Some(v) = memo[i] {
            return v;
        }
        let v = 1 + g.nodes[i]
            .deps
            .clone()
            .iter()
            .map(|&d| expected(g, d, memo))
            .sum::<i64>();
        memo[i] = Some(v);
        v
    }

    #[test]
    fn many_fibers_force_one_shared_graph() {
        for _ in 0..10 {
            shared_graph_round();
        }
    }

    fn shared_graph_round() {
        const N: usize = 4000;
        let mut nodes = Vec::with_capacity(N);
        let mut seed = 12345u64;
        for i in 0..N {
            let mut deps = Vec::new();
            // Dependencies point at lower indices: a DAG with heavy sharing
            // of the low nodes, so many fibers collide on the same thunks.
            for _ in 0..3 {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                if i > 0 {
                    let d = if seed % 4 == 0 {
                        (seed % 8) as usize % i
                    } else {
                        (seed as usize) % i
                    };
                    deps.push(d);
                }
            }
            nodes.push(Node {
                cell: Cell::new(Word::EMPTY),
                deps,
            });
        }
        let g: &'static Graph = Box::leak(Box::new(Graph {
            nodes,
            done: AtomicUsize::new(0),
            parks: AtomicUsize::new(0),
        }));
        let rt = Runtime::start(8, Box::new(GraphDriver(g)), 1 << 20);
        rt.run_main(
            std::ptr::null_mut(),
            Box::new(move |slot: &mut Slot| {
                let rt = slot.runtime();
                let mut ntasks = 0;
                // Every fiber forces a range of roots, from the top down so
                // the deep shared nodes are contended.
                for start in (0..N as u32).rev().step_by(16) {
                    let t = Task {
                        kind: 1,
                        obj: 0,
                        start: start.saturating_sub(15),
                        len: start - start.saturating_sub(15) + 1,
                    };
                    assert!(rt.submit_urgent(slot.worker(), t));
                    ntasks += 1;
                }
                let t0 = std::time::Instant::now();
                while g.done.load(Ordering::Acquire) < ntasks {
                    assert!(t0.elapsed().as_secs() < 30, "timeout");
                    // Main also forces roots, so it parks and migrates.
                    force(g, slot, N - 1);
                    std::thread::yield_now();
                }
            }),
        );
        let mut memo = vec![None; N];
        for i in 0..N {
            let want = expected(g, i, &mut memo);
            let got = g.nodes[i].cell.get();
            assert!(
                !got.is_blackhole() && got != Word::EMPTY,
                "node {i} unresolved"
            );
            assert_eq!(got.imm_int_value(), want, "node {i}");
        }
        assert_eq!(rt.deadlocks.load(Ordering::Relaxed), 0);
        eprintln!(
            "parks={} fibers={}",
            g.parks.load(Ordering::Relaxed),
            rt.stats_fibers()
        );
    }
}

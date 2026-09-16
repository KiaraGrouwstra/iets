//! Parallel evaluation: fibers, work-stealing scheduler, thunk claim
//! protocol. Design: docs/parallel.md.

pub mod deque;
pub mod fiber;
pub mod sched;
pub mod thunk;

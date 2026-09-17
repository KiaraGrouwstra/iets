//! Creation-time speculation: a thunk whose body is substantial is also
//! submitted to the speculative lane, so an idle helper evaluates it
//! before demand arrives. Gated by `par.speculating` (no cascades) and a
//! backlog cap. See docs/parallel.md "Work sources" (2).

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use super::force::{T_THUNK, ev_slot};
use super::sched::Task;
use crate::compile::ir::{AttrElemIr, Ir, ParamIr};
use crate::eval::Evaluator;
use crate::runtime::*;

/// Minimum IR node count of a body worth a speculative fiber.
fn min_nodes() -> u32 {
    static V: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *V.get_or_init(|| {
        std::env::var("IETSP_SPEC_MIN")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(24)
    })
}

/// `IETSP_SPEC` bitmask: 1 thunk bodies, 2 map/mapAttrs applications.
/// Off by default: without a GC the undemanded work it pulls in stays
/// resident, and the wall gain was within noise (docs/parallel.md).
fn mode() -> u32 {
    static V: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *V.get_or_init(|| {
        std::env::var("IETSP_SPEC")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0)
    })
}

/// Thunk creations a speculative task may make before it bails.
pub fn budget() -> u64 {
    static V: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *V.get_or_init(|| {
        std::env::var("IETSP_SPEC_BUDGET")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(4000)
    })
}

/// Speculative tasks queued per helper before submission stops.
const BACKLOG_PER_HELPER: usize = 128;
/// Candidates a fiber holds while their env is under construction.
const BUF_CAP: usize = 256;

pub static SUBMITTED: AtomicUsize = AtomicUsize::new(0);

const CACHE_SIZE: usize = 1 << 16;

/// Direct-mapped cache: IR address -> substantial bit (in bit 0; IR nodes
/// are 8-byte aligned). Zero means empty.
struct SizeCache([AtomicU64; CACHE_SIZE]);

static CACHE: SizeCache = SizeCache([const { AtomicU64::new(0) }; CACHE_SIZE]);

#[inline]
fn substantial(ir: &Ir) -> bool {
    let addr = ir as *const Ir as u64;
    let slot = &CACHE.0[((addr >> 3) as usize) & (CACHE_SIZE - 1)];
    let e = slot.load(Ordering::Relaxed);
    if e & !1 == addr {
        return e & 1 != 0;
    }
    let mut budget = min_nodes();
    count(ir, &mut budget);
    let big = budget == 0;
    slot.store(addr | big as u64, Ordering::Relaxed);
    big
}

/// Decrement `budget` per node; stop at zero.
fn count(ir: &Ir, budget: &mut u32) {
    if *budget == 0 {
        return;
    }
    *budget -= 1;
    match ir {
        Ir::Const(_) | Ir::Local { .. } | Ir::WithVar { .. } | Ir::CurPos(_) => {}
        Ir::Concat { parts, .. } | Ir::List(parts) => {
            for p in parts.iter() {
                count(p, budget);
            }
        }
        Ir::SearchPath { name, .. } => count(name, budget),
        Ir::Select(s) => {
            count(s.subject, budget);
            for e in s.path.iter() {
                if let AttrElemIr::Dynamic(d) = e {
                    count(d, budget);
                }
            }
            if let Some(d) = s.default {
                count(d, budget);
            }
        }
        Ir::Select1 {
            subject, default, ..
        } => {
            count(subject, budget);
            if let Some(d) = default {
                count(d, budget);
            }
        }
        Ir::HasAttr { subject, path } => {
            count(subject, budget);
            for e in path.iter() {
                if let AttrElemIr::Dynamic(d) = e {
                    count(d, budget);
                }
            }
        }
        Ir::Attrs(a) => {
            for b in a.binds.iter() {
                count(b.value, budget);
            }
            for f in a.from.iter() {
                count(f, budget);
            }
            for d in a.dynamic.iter() {
                count(d.name, budget);
                count(d.value, budget);
            }
        }
        Ir::Lambda(l) => {
            if let ParamIr::Formals { formals, .. } = &l.param {
                for f in formals.iter() {
                    if let Some(d) = f.default {
                        count(d, budget);
                    }
                }
            }
            count(l.body, budget);
        }
        Ir::Apply { func, args, .. } => {
            count(func, budget);
            for a in args.iter() {
                count(a, budget);
            }
        }
        Ir::Let(l) => {
            for b in l.binds.iter() {
                count(b.value, budget);
            }
            for f in l.from.iter() {
                count(f, budget);
            }
            count(l.body, budget);
        }
        Ir::With { scope, body, .. } => {
            count(scope, budget);
            count(body, budget);
        }
        Ir::If {
            cond, then, else_, ..
        } => {
            count(cond, budget);
            count(then, budget);
            count(else_, budget);
        }
        Ir::Assert { cond, body, .. } => {
            count(cond, budget);
            count(body, budget);
        }
        Ir::Not(a, _) | Ir::Neg(a, _) => count(a, budget),
        Ir::Eq(a, b, _)
        | Ir::Ne(a, b, _)
        | Ir::Lt(a, b, _)
        | Ir::Gt(a, b, _)
        | Ir::Le(a, b, _)
        | Ir::Ge(a, b, _)
        | Ir::And(a, b, _)
        | Ir::Or(a, b, _)
        | Ir::Impl(a, b, _)
        | Ir::Update(a, b, _)
        | Ir::ConcatLists(a, b, _)
        | Ir::Sub(a, b, _)
        | Ir::Mul(a, b, _)
        | Ir::Div(a, b, _) => {
            count(a, budget);
            count(b, budget);
        }
    }
}

/// Record `w` (a fresh thunk over `ir`) as a speculation candidate when
/// its body is substantial. Called from `mk_thunk_counted`. The thunk is
/// not submitted here: its env may still be under construction, and a
/// helper reading an EMPTY slot would crash. `flush` submits later.
#[inline]
pub fn speculate(ev: &mut Evaluator, w: Word, ir: &Ir) {
    if ev.par.speculating || mode() & 1 == 0 || !substantial(ir) {
        return;
    }
    if ev.par.spec_buf.len() >= BUF_CAP {
        ev.par.spec_buf.remove(0);
    }
    ev.par.spec_buf.push(w);
}

/// Submit the buffered candidates. Called at `force_thunk` after the
/// claim, so the thunk being forced is never handed to a helper first.
/// Every env is complete by then: envs carry no length, and the check
/// rests on `maybe_thunk` never forcing, so no `force_thunk` runs while
/// a fiber is still filling an env.
pub fn flush(ev: &mut Evaluator) {
    let buf = std::mem::take(&mut ev.par.spec_buf);
    for w in buf {
        submit(ev, w);
    }
}

/// `map f x` style thunk applications: speculate when `f` is a lambda
/// with a substantial body.
#[inline]
pub fn speculate_app(ev: &Evaluator, w: Word, f: Word) {
    if !ev.par.on || ev.par.speculating || mode() & 2 == 0 {
        return;
    }
    // `f` is usually still a thunk here; only an already forced lambda
    // tells us anything.
    let f = if f.is_thunk() {
        match f.thunk_forced() {
            Some(r) => r,
            None => return,
        }
    } else {
        f
    };
    let body = match f.view() {
        Value::Lambda(l) => l.lam.body,
        _ => return,
    };
    if substantial(body) {
        submit(ev, w);
    }
}

fn submit(ev: &Evaluator, w: Word) {
    let slot = ev_slot(ev);
    let rt = slot.runtime();
    if rt.n <= 1 || rt.pending_spec() >= BACKLOG_PER_HELPER * (rt.n - 1) {
        return;
    }
    let t = Task {
        kind: T_THUNK,
        obj: w.0,
        start: 0,
        len: 0,
    };
    if rt.submit_spec(slot.worker(), t) {
        SUBMITTED.fetch_add(1, Ordering::Relaxed);
    }
}

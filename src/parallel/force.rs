//! Evaluator side of the parallel runtime: the driver that gives every
//! fiber an `Evaluator`, the parallel `force_thunk`, strict-demand fan-out,
//! and the range tasks helpers run.

use std::collections::HashSet;
use std::sync::atomic::Ordering;

use super::Ctx;
use super::sched::{Driver, Runtime, Slot, Task, Worker};
use super::thunk::{self, Claim};
use crate::eval::{Error, ErrorKind, EvalResult, Evaluator, Pos};
use crate::runtime::*;

pub const MIN_FANOUT: usize = 4;
pub const BATCH: u32 = 16;

/// `IETSP_FANOUT` bitmask for experiments: 1 deep, 2 coerce, 4 list builtins.
fn fanout_mask() -> u32 {
    static MASK: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *MASK.get_or_init(|| {
        std::env::var("IETSP_FANOUT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(7)
    })
}

pub const T_LIST_WHNF: u8 = 1;
pub const T_ATTRS_WHNF: u8 = 2;
pub const T_LIST_DEEP: u8 = 3;
pub const T_ATTRS_DEEP: u8 = 4;
pub const T_THUNK: u8 = 5;
/// Coercion-shaped walk: lists element-wise, derivations to `outPath`,
/// other attrsets attribute-wise. What `derivationStrict`, `toJSON` and
/// `concatStringsSep` demand.
pub const T_LIST_COERCE: u8 = 6;
pub const T_ATTRS_COERCE: u8 = 7;
/// `ietsp eval-jobs` prefetch of a range of attrs (`crate::jobs`).
pub const T_JOB: u8 = 8;
const COERCE_BATCH: u32 = 4;

/// The root evaluator, cloned for every fiber.
pub struct EvalDriver(pub SyncEv);

pub struct SyncEv(pub Evaluator);
// SAFETY: the template is only read through `Clone`, which copies scalars
// and bumps reference counts on the shared handles.
unsafe impl Sync for SyncEv {}
unsafe impl Send for SyncEv {}

impl Driver for EvalDriver {
    fn new_user(&self, _rt: &Runtime) -> *mut () {
        Box::into_raw(Box::new(self.0.0.clone())) as *mut ()
    }

    fn run_task(&self, _rt: &Runtime, slot: &mut Slot) {
        let ev = ev_of(slot);
        init_ctx(ev, slot);
        let t = slot.task;
        run_task(ev, t);
    }

    fn on_switch(&self, slot: &mut Slot, w: &mut Worker) {
        std::mem::swap(&mut ev_of(slot).arena, &mut w.arena);
    }
}

#[inline]
pub fn ev_of(slot: &Slot) -> &'static mut Evaluator {
    // SAFETY: `user` was created by `new_user` (or is the root evaluator)
    // and is only touched by the fiber that owns the slot.
    unsafe { &mut *(slot.user as *mut Evaluator) }
}

/// Bind a slot's evaluator to the fiber that is about to run.
pub fn init_ctx(ev: &mut Evaluator, slot: &mut Slot) {
    ev.par = Ctx {
        on: true,
        id: slot.id,
        is_main: slot.is_main,
        cancelled: false,
        speculating: false,
        spec_buf: std::mem::take(&mut ev.par.spec_buf),
        task_thunks: ev.nr_thunks,
        slot,
    };
    ev.par.spec_buf.clear();
    ev.call_depth = 0;
    ev.try_level = 0;
}

#[inline]
pub fn ev_slot(ev: &Evaluator) -> &'static mut Slot {
    slot_of(ev)
}

#[inline]
fn slot_of(ev: &Evaluator) -> &'static mut Slot {
    // SAFETY: set by `init_ctx`; the slot outlives the process.
    unsafe { &mut *ev.par.slot }
}

fn cancelled() -> Box<Error> {
    Error::boxed(ErrorKind::Cancelled, "cancelled", Pos::NONE)
}

/// True once this helper fiber must stop: cancelled by deadlock
/// resolution, or speculative after the main result exists.
#[inline]
pub fn bail_requested(ev: &Evaluator) -> bool {
    if ev.par.is_main {
        return false;
    }
    if ev.par.cancelled {
        return true;
    }
    let slot = slot_of(ev);
    slot.cancel.load(Ordering::Relaxed)
        || (ev.par.speculating
            && (slot.runtime().suppress.load(Ordering::Relaxed)
                || ev.nr_thunks - ev.par.task_thunks > super::spec::budget()))
}

/// Position of the thunk the main fiber forced last (diagnostics: the
/// `IETSP_PAR_STATS=2` timeline samples it).
pub static MAIN_POS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static SAMPLE_POS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
pub static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

/// Parallel `Evaluator::force_thunk`: claim, evaluate, publish.
#[inline(never)]
pub fn force_thunk(ev: &mut Evaluator, w: Word) -> EvalResult<Word> {
    let cell = w.thunk_result_cell();
    let me = ev.par.id;
    if SAMPLE_POS.load(Ordering::Relaxed) && ev.par.is_main && w.thunk_kind() == Kind::Thunk {
        let p = crate::eval::ir_pos(w.as_thunk().ir);
        MAIN_POS.store(p.0 as u64, Ordering::Relaxed);
    }
    loop {
        match thunk::try_claim(cell, me) {
            Claim::Won => break,
            Claim::Value(v) => return Ok(v),
            Claim::Mine => {
                let pos = match w.thunk_kind() {
                    Kind::Thunk => crate::eval::ir_pos(w.as_thunk().ir),
                    _ => Pos::NONE,
                };
                return Err(Error::boxed(
                    ErrorKind::InfiniteRecursion,
                    "infinite recursion encountered",
                    pos,
                ));
            }
            Claim::Busy(cur) => {
                if thunk::wait(slot_of(ev), cell, cur).is_err() {
                    ev.par.cancelled = true;
                    return Err(cancelled());
                }
            }
        }
    }
    if bail_requested(ev) {
        ev.par.cancelled = true;
        thunk::publish(slot_of(ev), cell, Word::EMPTY);
        return Err(cancelled());
    }
    if !ev.par.spec_buf.is_empty() {
        super::spec::flush(ev);
    }
    let r = match w.thunk_kind() {
        Kind::Thunk => {
            let t = w.as_thunk();
            ev.eval(t.env, t.ir)
        }
        Kind::ThunkApp => {
            let t = w.as_thunk_app();
            ev.apply(t.func, &[t.arg], Pos::NONE)
        }
        _ => {
            let t = w.as_thunk_app2();
            ev.apply(t.func, &[t.arg1, t.arg2], Pos::NONE)
        }
    };
    match r {
        Ok(v) => {
            debug_assert!(!v.is_thunk());
            if ev.par.cancelled {
                // Bailed (or hit an effect) somewhere below: transient.
                thunk::publish(slot_of(ev), cell, Word::EMPTY);
                Err(cancelled())
            } else {
                thunk::publish(slot_of(ev), cell, v);
                Ok(v)
            }
        }
        Err(e) => {
            thunk::publish(slot_of(ev), cell, Word::EMPTY);
            Err(e)
        }
    }
}

/// `trace`/`warn` in a helper fiber: the helper abandons its task instead
/// of printing. Cancel is sticky, so every thunk still open in this fiber
/// is reset and no value built after the effect escapes into a cell; the
/// demand path re-evaluates and prints. Returns false when the caller
/// should print.
#[inline]
pub fn journal(ev: &mut Evaluator, _msg: String) -> bool {
    if ev.par.on && !ev.par.is_main {
        ev.par.cancelled = true;
        EFFECT_BAILS.fetch_add(1, Ordering::Relaxed);
        true
    } else {
        false
    }
}

pub static EFFECT_BAILS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

// ---- fan-out --------------------------------------------------------------

pub enum CellState {
    Ready(Word),
    Unclaimed,
    Busy,
}

#[inline]
pub fn cell_state(cell: &Cell) -> CellState {
    let w = cell.get();
    if !w.is_thunk() {
        return CellState::Ready(w);
    }
    let r = w.thunk_result_cell().get();
    if r == Word::EMPTY {
        CellState::Unclaimed
    } else if r.is_blackhole() {
        CellState::Busy
    } else {
        CellState::Ready(r)
    }
}

/// Submit `n` cells of `obj` as range tasks on the urgent lane.
pub fn fanout(ev: &Evaluator, kind: u8, obj: Word, n: usize) {
    fanout_batched(ev, kind, obj, n, BATCH)
}

fn fanout_batched(ev: &Evaluator, kind: u8, obj: Word, n: usize, batch: u32) {
    fanout_range(ev, kind, obj, 0, n, batch)
}

/// Submit cells `lo..hi` of `obj` as range tasks of `batch` cells.
pub fn fanout_range(ev: &Evaluator, kind: u8, obj: Word, lo: usize, hi: usize, batch: u32) {
    if !ev.par.on || hi - lo < MIN_FANOUT {
        return;
    }
    let slot = slot_of(ev);
    let rt = slot.runtime();
    let w = slot.worker();
    let cell = |i: usize| -> &'static Cell {
        match kind {
            T_LIST_WHNF | T_LIST_DEEP | T_LIST_COERCE => &obj.as_list().unwrap().cells()[i],
            _ => &obj.as_attrs().unwrap().attrs()[i].cell,
        }
    };
    let n = hi;
    let mut start = lo as u32;
    while (start as usize) < n {
        let len = batch.min(n as u32 - start);
        // Only ranges with an unclaimed thunk are worth a task.
        let worth = (start..start + len)
            .any(|i| matches!(cell_state(cell(i as usize)), CellState::Unclaimed));
        if worth {
            let t = Task {
                kind,
                obj: obj.0,
                start,
                len,
            };
            if !rt.submit_urgent(w, t) {
                return;
            }
        }
        start += len;
    }
}

/// Fan out a list a strict builtin is about to walk (elements to WHNF).
#[inline]
pub fn fanout_list(ev: &Evaluator, l: &'static ListObj) {
    if ev.par.on && fanout_mask() & 4 != 0 {
        fanout(ev, T_LIST_WHNF, Word::from_ptr(l), l.len as usize);
    }
}

#[inline]
pub fn fanout_attrs(ev: &mut Evaluator, a: &'static AttrsObj) {
    if ev.par.on && fanout_mask() & 4 != 0 {
        let a = ev.arena.flat(a);
        fanout(ev, T_ATTRS_WHNF, Word::from_ptr(a), a.len());
    }
}

/// A list whose elements are about to be coerced to strings.
#[inline]
pub fn fanout_list_coerce(ev: &Evaluator, l: &'static ListObj) {
    if ev.par.on && fanout_mask() & 2 != 0 {
        fanout_batched(
            ev,
            T_LIST_COERCE,
            Word::from_ptr(l),
            l.len as usize,
            COERCE_BATCH,
        );
    }
}

/// Attrs whose values are about to be coerced (`derivationStrict`, JSON).
#[inline]
pub fn fanout_attrs_coerce(ev: &mut Evaluator, a: &'static AttrsObj) {
    if ev.par.on && fanout_mask() & 2 != 0 {
        let a = ev.arena.flat(a);
        fanout_batched(ev, T_ATTRS_COERCE, Word::from_ptr(a), a.len(), COERCE_BATCH);
    }
}

/// Helper-side coercion walk of a forced value. Never parks on busy
/// cells; returns false when the task should stop.
fn coerce_walk(ev: &mut Evaluator, v: Word, depth: u32) -> bool {
    if depth > 64 || bail_requested(ev) {
        return depth <= 64;
    }
    match v.view() {
        Value::List(l) => {
            fanout_batched(ev, T_LIST_COERCE, v, l.len as usize, COERCE_BATCH);
            for c in l.cells() {
                if !coerce_step(ev, c, depth) {
                    return false;
                }
            }
            true
        }
        Value::Attrs(a) => {
            if a.get(ev.s.to_string).is_some() {
                return true;
            }
            if let Some(o) = a.get(ev.s.out_path) {
                return coerce_step(ev, &o.cell, depth);
            }
            let a = ev.arena.flat(a);
            fanout_batched(ev, T_ATTRS_COERCE, Word::from_ptr(a), a.len(), COERCE_BATCH);
            for attr in a.attrs() {
                if !coerce_step(ev, &attr.cell, depth) {
                    return false;
                }
            }
            true
        }
        _ => true,
    }
}

fn coerce_step(ev: &mut Evaluator, c: &Cell, depth: u32) -> bool {
    let v = match cell_state(c) {
        CellState::Busy => return true,
        CellState::Ready(v) => {
            c.set(v);
            v
        }
        CellState::Unclaimed => match ev.force_cell(c) {
            Ok(v) => v,
            Err(_) => return false,
        },
    };
    coerce_walk(ev, v, depth + 1)
}

/// Parallel `Evaluator::force_deep`.
pub fn force_deep(ev: &mut Evaluator, w: Word) -> EvalResult<Word> {
    let mut seen = HashSet::new();
    let w = ev.force(w)?;
    deep_inner(ev, w, &mut seen, true)?;
    Ok(w)
}

/// Walk `w` deeply. `demand`: park on busy cells (main-style walk) after
/// first doing every unclaimed cell; otherwise skip busy cells.
fn deep_inner(
    ev: &mut Evaluator,
    w: Word,
    seen: &mut HashSet<u64>,
    demand: bool,
) -> EvalResult<()> {
    match w.view() {
        Value::Attrs(a) => {
            if !seen.insert(w.0) {
                return Ok(());
            }
            let a = ev.arena.flat(a);
            if fanout_mask() & 1 != 0 {
                fanout(ev, T_ATTRS_DEEP, Word::from_ptr(a), a.len());
            }
            let attrs = a.attrs();
            walk(
                ev,
                attrs.len(),
                seen,
                demand,
                |i| &attrs[i].cell,
                |ev, i, e| {
                    let mut e = e;
                    e.add_trace(
                        attrs[i].pos,
                        format!(
                            "while evaluating the attribute '{}'",
                            ev.interner.name_str(attrs[i].sym)
                        ),
                    );
                    e
                },
            )
        }
        Value::List(l) => {
            if !seen.insert(w.0) {
                return Ok(());
            }
            if fanout_mask() & 1 != 0 {
                fanout(ev, T_LIST_DEEP, w, l.len as usize);
            }
            let cells = l.cells();
            walk(ev, cells.len(), seen, demand, |i| &cells[i], |_, _, e| e)
        }
        _ => Ok(()),
    }
}

/// Two-pass walk over `n` cells: unclaimed cells first, then (demand
/// only) the cells another fiber was busy with, in index order so that
/// the first error in sequential order is the one reported.
fn walk(
    ev: &mut Evaluator,
    n: usize,
    seen: &mut HashSet<u64>,
    demand: bool,
    cell: impl Fn(usize) -> &'static Cell,
    decorate: impl Fn(&mut Evaluator, usize, Box<Error>) -> Box<Error>,
) -> EvalResult<()> {
    let mut deferred: Vec<usize> = Vec::new();
    let mut first_err: Option<(usize, Box<Error>)> = None;
    for i in 0..n {
        let c = cell(i);
        match cell_state(c) {
            CellState::Busy => {
                if demand {
                    deferred.push(i);
                }
                continue;
            }
            CellState::Ready(v) => c.set(v),
            CellState::Unclaimed => {}
        }
        if !demand && bail_requested(ev) {
            return Err(cancelled());
        }
        let r = ev
            .force_cell(c)
            .and_then(|v| deep_inner(ev, v, seen, demand));
        if let Err(e) = r {
            if demand {
                first_err = Some((i, decorate(ev, i, e)));
                break;
            }
            return Err(e);
        }
    }
    let limit = first_err.as_ref().map_or(n, |(i, _)| *i);
    for i in deferred {
        if i >= limit {
            break;
        }
        let c = cell(i);
        let r = ev
            .force_cell(c)
            .and_then(|v| deep_inner(ev, v, seen, demand));
        if let Err(e) = r {
            return Err(decorate(ev, i, e));
        }
    }
    match first_err {
        Some((_, e)) => Err(e),
        None => Ok(()),
    }
}

// ---- tasks ------------------------------------------------------------------

fn run_task(ev: &mut Evaluator, t: Task) {
    let obj = Word(t.obj);
    let (start, end) = (t.start as usize, (t.start + t.len) as usize);
    match t.kind {
        T_LIST_WHNF | T_LIST_DEEP => {
            let l = obj.as_list().expect("list task");
            let deep = t.kind == T_LIST_DEEP;
            let mut seen = HashSet::new();
            for c in &l.cells()[start..end] {
                if !range_step(ev, c, deep, &mut seen) {
                    return;
                }
            }
        }
        T_ATTRS_WHNF | T_ATTRS_DEEP => {
            let a = obj.as_attrs().expect("attrs task");
            let deep = t.kind == T_ATTRS_DEEP;
            let mut seen = HashSet::new();
            for attr in &a.attrs()[start..end] {
                if !range_step(ev, &attr.cell, deep, &mut seen) {
                    return;
                }
            }
        }
        T_LIST_COERCE => {
            let l = obj.as_list().expect("list task");
            for c in &l.cells()[start..end] {
                if !coerce_step(ev, c, 0) {
                    return;
                }
            }
        }
        T_ATTRS_COERCE => {
            let a = obj.as_attrs().expect("attrs task");
            for attr in &a.attrs()[start..end] {
                if !coerce_step(ev, &attr.cell, 0) {
                    return;
                }
            }
        }
        T_JOB => {
            let a = obj.as_attrs().expect("attrs task");
            crate::jobs::prefetch_range(ev, a, start, end);
        }
        T_THUNK => {
            ev.par.speculating = true;
            if obj.thunk_result_cell().get() == Word::EMPTY {
                let _ = ev.force(obj);
            }
        }
        k => panic!("unknown task kind {k}"),
    }
}

/// Force one cell of a range task. Skips cells someone else claimed.
/// Returns false when the task should stop.
fn range_step(ev: &mut Evaluator, c: &Cell, deep: bool, seen: &mut HashSet<u64>) -> bool {
    if bail_requested(ev) {
        return false;
    }
    match cell_state(c) {
        CellState::Unclaimed => {}
        CellState::Ready(v) if deep => {
            c.set(v);
            return deep_inner(ev, v, seen, false).is_ok();
        }
        _ => return true,
    }
    match ev.force_cell(c) {
        Ok(v) => {
            if deep {
                deep_inner(ev, v, seen, false).is_ok()
            } else {
                true
            }
        }
        Err(_) => false,
    }
}

// ---- imports ------------------------------------------------------------------

/// Lazy import: the cache maps a resolved path to this thunk so that the
/// claim protocol serialises concurrent importers of one file.
pub fn import_thunk(ev: &mut Evaluator, resolved: &[u8], pos: Pos) -> Word {
    let path = ev.mk_string(resolved);
    let posw = Word::imm_int(pos.0 as i64);
    let op = ev.arena.mk_primop("importRaw", 2, import_raw);
    let app = ev.arena.mk_primop_app(op.view_primop(), &[path]);
    ev.arena.mk_thunk_app(app, posw)
}

fn import_raw(ev: &mut Evaluator, args: &[Word], _pos: Pos) -> EvalResult<Word> {
    let resolved = args[0].as_string().expect("path string").bytes;
    let pos = Pos(args[1].imm_int_value() as u32);
    let t0 = SAMPLE_POS
        .load(Ordering::Relaxed)
        .then(std::time::Instant::now);
    let src = ev.read_source(resolved, pos)?;
    let dir = crate::eval::dir_of(resolved).to_vec();
    let ir = ev.compile_source(resolved, src, &dir, None)?;
    if let Some(t0) = t0 {
        eprintln!(
            "import t={}ms {}us main={} w{} {}",
            START.get().map_or(0, |s| s.elapsed().as_millis()),
            t0.elapsed().as_micros(),
            ev.par.is_main,
            slot_of(ev).worker().idx,
            String::from_utf8_lossy(resolved)
        );
    }
    let root = ev.root_env;
    ev.eval(root, ir)
}

trait ViewPrimop {
    fn view_primop(self) -> &'static PrimOpObj;
}

impl ViewPrimop for Word {
    fn view_primop(self) -> &'static PrimOpObj {
        match self.view() {
            Value::PrimOp(p) => p,
            _ => unreachable!(),
        }
    }
}

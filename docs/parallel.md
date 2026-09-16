# Parallel evaluation

Implicit parallelism in the style of fix: worker threads force thunks ahead
of and beside the demand path. The result is identical to sequential
evaluation. `--workers 1` (or `IETSP_WORKERS=1`) runs the sequential path
unchanged.

Code: `src/parallel/`. Hooks outside that directory are one-line call sites
marked `// parallel:`.

## Model

- **Fiber**: a stackful coroutine with its own mmap'd stack and its own
  `Evaluator`. A fiber is the unit of blocking. Fibers migrate between
  worker threads: a fiber that parks on worker A can resume on worker B.
- **Worker**: an OS thread that runs a drain loop. Worker 0 is the thread
  that called `parallel::run`. Each worker owns a bump `Arena`, a fiber
  pool, and work-stealing deques.
- **Task**: a unit of not-yet-started work: force a range of list cells or
  attr cells (fan-out), or force one thunk (speculation). A task runs in a
  pooled fiber.
- **Main fiber**: runs the CLI closure (import, force, print). When it
  returns, evaluation is done. Its `Evaluator` is the one `main.rs` built.

Every fiber's `Evaluator` is a clone of the root one. Per-fiber state
(`call_depth`, `try_level`, counters, `par`) is private. Shared state lives
behind handles that clone by reference (`Arc`): interner, file table,
import cache, `src_to_store`, derivation table, derivation hash cache.
`Globals` is cloned by value (read-only after startup). The `Arena` field
of a fiber's `Evaluator` is swapped with the worker's arena on every resume
and swapped back on every yield, so N workers own N arenas and no fiber
allocates from an arena another thread is using. `Word` pointers are
global; cross-arena references are fine because arenas are never freed.

## Thunk claim protocol

The thunk result `Cell` is the synchronisation word. States:

```
EMPTY ──CAS──> BLACKHOLE(claimer, waiters bit) ──store──> value
  ^                          │
  └────────── reset ─────────┘   (error, cancel, or traced-in-helper)
```

- `BLACKHOLE(id)` is `Word::BLACKHOLE | id << 8`. Bit 7 is the
  *has-waiters* flag. The sequential path stores plain `Word::BLACKHOLE`
  (id 0); `Word::is_blackhole` matches every variant.
- **Claim**: `compare_exchange(EMPTY, BLACKHOLE(me), Acquire, Acquire)`.
  Winner evaluates. Loser reads the current word:
  - value: return it (Acquire load pairs with the Release publish).
  - `BLACKHOLE(me)`: infinite recursion. The claimer id is per fiber, not
    per thread, so it follows a migrated computation.
  - `BLACKHOLE(other)`: park (below).
- **Publish**: `swap(value, Release)`. If the old word had the has-waiters
  bit, drain the waiter list for this thunk and wake each fiber.
- **Reset**: same as publish with `EMPTY`; waiters wake and retry the
  claim. Used for errors (errors are never cached, as in the sequential
  path), for cancelled fibers, and for helper results that carried a
  `trace`/`warn` (see Effects).

Waiter list: a side table sharded by thunk address (64 shards, each a
`Mutex<HashMap<thunk addr, Vec<*mut Fiber>>>`). Thunk objects do not grow.

**Park sequence** (in the parking fiber):

1. lock shard; re-read the cell; if it is no longer `BLACKHOLE(other)`,
   unlock and retry the claim loop (closes the enroll-vs-publish race);
2. push self on the list; CAS the has-waiters bit into the cell (if the
   CAS fails the word changed: remove self, unlock, retry);
3. unlock; `fiber.state = PARKING`; yield to the worker.

**Publish sequence** (in the resolver): swap the value in; if the old word
had has-waiters: lock shard, take the list, unlock; wake each fiber outside
the lock.

## Fiber wake handshake

A parked fiber must be resumed exactly once and only after it has fully
switched out. One atomic `state` word on the fiber does it, no lock:

```
fiber before yield:        state = PARKING
worker after switch-out:   prev = state.swap(SUSPENDED)
                           if prev == WOKEN: push fiber on own ready deque
waker:                     loop on state:
                             SUSPENDED -> CAS to RUNNABLE -> push on own ready deque
                             PARKING   -> CAS to WOKEN     (worker pushes it)
                             else      -> done (already runnable/running)
resumer (owner of a ready entry): state = RUNNING, switch in
```

Invariants: a fiber is on a ready deque at most once (only the
SUSPENDED->RUNNABLE or PARKING->WOKEN transition enqueues, and each happens
once per park); a fiber is resumed only from a ready deque entry; the
resumer's switch-in happens after the previous switch-out because the
entry is created after `swap(SUSPENDED)` or after the resumer's own
switch-out returned.

Context switch: a naked function saves callee-saved registers and the
stack pointer (x86-64: rbx rbp r12-r15 mxcsr x87cw; aarch64: x19-x30
d8-d15) on the current stack, stores rsp into the source context, loads
the target's rsp, restores, and returns. A never-run fiber's stack is
seeded so that the first "return" lands in a trampoline that calls the
fiber entry with the fiber pointer in a callee-saved register. No
thread-local storage is read on the switch path.

Stacks: `mmap` 256 MiB `MAP_NORESERVE` plus a `PROT_NONE` guard page at
the low end; pages are demand-paged. The sequential path keeps its 4 GiB
thread stack. Pool fibers beyond the prewarm count give their pages back
with `MADV_DONTNEED` when the worker parks.

## Scheduler

Per worker: `ready` deque of fiber pointers, `urgent` deque of tasks
(fan-out), `spec` deque of tasks (speculation). All three are
fixed-capacity Chase-Lev deques (owner LIFO push/pop, thieves FIFO steal;
push fails when full and the caller falls back to doing the work inline).
No dynamic buffer means no memory reclamation and no thread-local storage
inside the scheduler.

Drain step, in priority order: own ready, steal ready, own urgent, steal
urgent, own spec, steal spec. Ready fibers first because a woken fiber
is usually on the critical path. A task runs by taking a pooled fiber,
installing the task, and resuming it.

Park: a worker with nothing to do spins for a bounded number of scans
(`spin_loop` hints, then `yield_now`), then increments `sleepers`, reads
the wake `epoch`, re-scans once, and `futex_wait(epoch)`. A push that
observes `sleepers > 0` bumps `epoch` and does `futex_wake(1)`. Lost
wake-ups are impossible: the epoch is read before the final scan and
compared by the futex.

Shutdown: when the main fiber returns, `run` stores the result, sets
`shutdown`, wakes everyone and returns. Helpers stop at their next loop
iteration. The process exits; unfinished helper fibers are abandoned
(their thunks stay BLACKHOLE, which nobody reads any more).

Deadlock: fan-out can claim the two halves of a cycle on two fibers, which
then wait on each other while sequential evaluation would have reported
infinite recursion. The last worker to go to sleep (`sleepers == N`, no
pending work, main not done) cancels every parked helper fiber: sets its
`cancel` flag and wakes it. `wait` returns `Err(Cancelled)`, the fiber
unwinds, every thunk it claimed is reset, waiters (including main) wake,
and main re-evaluates the cycle sequentially and reports the error.

## Work sources

1. **Strict-demand fan-out** (`parallel::force_deep`, `deepSeq`,
   `--strict`, `toJSON`): when a list or attrset with >= 4 elements is
   forced deeply, the elements are submitted as range tasks of 16 cells
   on the urgent deque before the demand loop walks them. A range task
   forces each cell to WHNF and then recurses with `force_deep`, which
   fans out again. Cells already forced are skipped. The demand walk then
   finds most cells forced or claimed (park).
2. **Speculation at thunk creation** (`maybe_thunk` hook): a thunk whose
   IR body is substantial is also submitted as a spec task. Gated by
   `par.speculating` (a speculative fiber does not spawn more speculation)
   and by a per-worker backlog cap. Second phase.
3. **Sibling prefetch** on attr lookup miss in attrsets with 16..64
   entries. Third phase.

## Cancellation and bail

Every helper fiber (any fiber except main) checks `par.cancel` at each
`force_thunk` entry. Cancel is set by shutdown suppression, by deadlock
resolution, and by the speculation brake. A cancelled fiber gets
`Err(ErrorKind::Cancelled)` from every force and unwinds. **Cancel is
sticky**: once set, every publish this fiber would do becomes a reset,
so a swallowed error (`Err(_) => false` sites) cannot leak a value built
from a bailed sub-computation into a shared thunk. `Cancelled` is not
catchable by `tryEval`.

## Effects

`builtins.trace` and `builtins.warn` in a helper fiber append to the
fiber journal instead of writing to stderr. At publish, a thunk whose
evaluation grew the journal is reset instead of published, and so is
every enclosing thunk of that fiber (the journal length is snapshotted at
each `force_thunk` entry). The demand path re-evaluates those thunks and
emits the messages itself, in sequential order, exactly once. Values
computed by the helper below the traced thunk stay memoised. Errors are
never cached in either mode, so error values from helpers are transient
by construction.

## Memory model

- `Cell::get` is `Acquire`, `Cell::set` is `Release` (free on x86-64).
  An object is fully written before the word that points at it is stored
  into a cell or pushed onto a deque, and readers see the write after the
  acquire load.
- Interner: 64 shards of `RwLock<HashMap<&'static [u8], Sym>>`, ids
  allocated under a global counter, names in a lazily-mapped array of
  `AtomicPtr` indexed by id (no lock on `name`).
- File table, import cache, `src_to_store`, derivation table: `Mutex`.
  Derivation hash cache: `Mutex<BTreeMap>` locked for the duration of
  `hashDerivationModulo`.
- Import cache: path -> thunk. The first importer inserts a `ThunkApp`
  over an internal primop that reads, compiles and evaluates the file;
  everyone then forces that thunk, so the claim protocol serialises
  concurrent imports of one file and imports of different files proceed
  in parallel.
- Regex cache stays thread-local. The `Rc` is obtained after every force
  in the builtin and dropped before the next one, so it never crosses a
  yield. The accessor is `#[inline(never)]` so the TLS address is
  recomputed on the current thread.
- Thread-local storage rule: a function that reads TLS must be
  `#[inline(never)]` and must not yield. LLVM may cache a TLS base across
  calls inside one function; a fiber that migrated between two accesses
  would touch another thread's slot.
- No thread-local is read on the switch path or inside the scheduler.

## Determinism

The value graph is the same whichever fiber forces a thunk: forcing is
pure except for the effects above, and each thunk is published at most
once per successful evaluation (a reset followed by a re-evaluation
yields the same value). Attr order, list order, and derivation hashing are
unchanged. Differential tests: `--workers 8` versus `--workers 1` on
`scripts/langtests.sh` and on `bench/workloads` (`drvPath` identical).

## Not done

ThreadSanitizer needs nightly; not run. Invariants above are the
argument. No loom model.

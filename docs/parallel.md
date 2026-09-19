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
  attr cells (fan-out). A task runs in a pooled fiber.
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

The thunk's *state* `Cell` is the synchronisation word
(`Word::thunk_state_cell`). A thunk is overwritten in place, so the state
cell doubles as payload storage (docs/memory.md): for a `Thunk` it holds
the env pointer, for a `ThunkApp` the argument, for a `ThunkApp2` the
second argument, and for a `mapAttrs` map word it is a cell in the
`MapDesc` (payload NULL) while the entry's own cell in the result set is
the `code` word. States:

```
payload ──CAS──> BLACKHOLE(claimer, waiters bit) ──store──> EMPTY
   ^                          │
   └────────── reset ─────────┘   (error, cancel, or traced-in-helper)
```

The value sits in the `code` word, written before the state with
Release. `Word::thunk_unforced(state)` tells payload from done;
`thunk_value` and `thunk_finish` read and write the value. Cells that
values are read from (env slots, list and attr cells) never hold a
BLACKHOLE.

- `BLACKHOLE(id)` is `Word::BLACKHOLE | id << 8`. Bit 7 is the
  *has-waiters* flag. The sequential path stores plain `Word::BLACKHOLE`
  (id 0); `Word::is_blackhole` masks bit 7 and matches every variant.
  Payloads are never EMPTY and never a BLACKHOLE (`mk_thunk_app` asserts
  it), so the three states are disjoint.
- **Claim**: load the word first (Acquire); only a payload word gets the
  `compare_exchange(payload, BLACKHOLE(me), Acquire, Acquire)`, retried
  on the new word when it fails. A failed CAS still takes the cache line
  exclusive, and most forces hit a thunk another fiber already evaluated
  (`lib` functions, `pkgs`), so the plain load keeps hot shared lines
  shared. Winner gets the payload back (`Claim::Won(payload)`) and
  evaluates; the `code` word still holds the IR or function, which only
  the winner may read. Loser reads the current word:
  - done: return the value (Acquire load pairs with the Release publish).
  - `BLACKHOLE(me)`: infinite recursion. The claimer id is per fiber, not
    per thread, so it follows a migrated computation.
  - `BLACKHOLE(other)`: park (below).
- **Publish**: store the value in `code`, then `swap(done, Release)` on
  the state. If the old word had the has-waiters bit, drain the waiter
  list for this thunk and wake each fiber.
- **Reset**: same as publish with the payload; waiters wake and retry
  the claim. Used for errors (errors are never cached, as in the
  sequential path) and for cancelled fibers (see Effects).

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
worker after switch-out:   (state is PARKING or already WOKEN)
                           prev = state.swap(SUSPENDED)
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
(fan-out). Both are fixed-capacity Chase-Lev deques (owner LIFO
push/pop, thieves FIFO steal; push fails when full and the caller falls
back to doing the work inline). No dynamic buffer means no memory
reclamation and no thread-local storage inside the scheduler.

Drain step, in priority order: own ready, steal ready, own urgent, steal
urgent. Ready fibers first because a woken fiber is usually on the
critical path. A task runs by taking a pooled fiber, installing the
task, and resuming it.

Two global summary words, `pending_ready` and `pending_tasks`, say
whether any lane holds work. Idle workers poll only those; scanning the
lanes' `top`/`bottom` lines directly made every busy owner's push and
pop pay a coherence miss per idle thief (8 workers ran slower than 4).
Idle workers register as *searchers*; at most `max(1, n/4)` helpers
search at a time (worker 0 is never refused) and a searcher spins
~2 ms (2000 scans of 64 polls, `IETSP_SPIN`) before it parks.

Park: a searcher that finds nothing increments `sleepers`, reads the
wake `epoch`, re-checks the summaries, and `futex_wait(epoch)`. Lost
wake-ups are impossible: the epoch is read before the final check and
compared by the futex.

Wake policy (one wake per handoff, never one per push): a push wakes
one sleeper only when no searcher is awake *and* the work is worth a
wake; a searcher that takes work and was the last searcher wakes one
more sleeper under the same test. Worth a wake: a ready fiber always
(it is usually the critical path); queued tasks only when the queue
holds more than 8 tasks per awake worker (`IETSP_HANDOFF`), or when
the caller is the only worker awake. Waking on every push cost the
critical path a futex syscall per fan-out task and handed cache-warm
work to cold cores: at 16 workers 26k parks per run and 0.9 s of
system time; the handoff policy brought that to ~2k parks. Drivers
whose fibers busy-wait on a queued peer instead of parking (the tests)
call `set_eager_wakes` to get one wake per task back.

`running`, busy time and task counts are per-worker padded counters
(`WorkerStat`), summed for the stats; the shared counters they replace
were written from every core on every fiber resume.

Stack pages of idle pool fibers beyond 16 are returned with
`MADV_DONTNEED` on every 64th park only; trimming on every park cost
more in page faults than it saved.

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

1. **Strict-demand fan-out.** Three task shapes, all on the urgent lane,
   batched, submitted only for ranges that still hold an unclaimed thunk:
   - *deep* (`force_deep`, `deepSeq`, `--strict`): 16 cells per task,
     each forced then walked deeply, fanning out again;
   - *coerce* (`derivationStrict` attrs, strict `toJSON`,
     `concatStringsSep`): 4 cells per task; lists element-wise,
     derivations to `outPath` only, other attrsets attribute-wise. This
     is the shape of the demand that follows, so helpers resolve the
     derivation DAG ahead of the coercion loop;
   - *WHNF* (`filter`, `foldl'`, `concatLists`, `concatMap`, `any`,
     `all`, `sort`, `partition`, `elem`, `listToAttrs`, `zipAttrsWith`,
     `catAttrs`, `groupBy`): 16 cells per task, elements to WHNF.
   Helpers never park inside a range walk: a busy cell is skipped, its
   claimer walks below it. The demand walk (`walk`) does every unclaimed
   cell first and parks on the busy ones afterwards, in index order, so
   the first error in sequential order is still the one reported.
   Measured on nixos-desktop: the WHNF hooks alone are a net loss (main
   parks on cheap elements), coerce alone is a small win, all three
   together give the numbers below. `IETSP_FANOUT` bitmask (1 deep, 2
   coerce, 4 WHNF) toggles them for experiments.

## Cancellation and bail

Every helper fiber (any fiber except main) checks for a bail at each
`force_thunk` entry: the slot's `cancel` flag (deadlock resolution), or
an effect (below). A cancelled fiber gets `Err(ErrorKind::Cancelled)`
from every force and unwinds. **Cancel is sticky**: once set, every
publish this fiber would still do becomes a reset, so a swallowed error
(`Err(_) => false` sites) cannot leak a value built from a bailed
sub-computation into a shared thunk. `Cancelled` is not catchable by
`tryEval`. Thunks the fiber completed before the bail stay published.

## Effects

`builtins.trace` and `builtins.warn` in a helper fiber print nothing:
the helper sets its sticky cancel and abandons the task. Every thunk
still open in that fiber is reset, and because a cancelled force returns
`Err`, no write-back into a list, attr or env cell happens (the first
design, "journal and reset the thunk but keep computing", lost warnings:
`force_cell` wrote the value into the container cell and demand never
re-evaluated). The demand path evaluates the thunk itself and prints,
exactly once. Errors are never cached in either mode, so error values
from helpers are transient by construction. `scripts/partest.nu` checks
trace and warn counts at 1 versus N workers.

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
unchanged. Known divergences: the order of trace lines on stderr can
differ (main walks unclaimed cells first), and a helper starts at call
depth 0, so a program that only fails by `max-call-depth` sequentially
may succeed.

Tests: `cargo test --lib parallel` (fiber, deque, scheduler handshake,
claim/park stress on a shared DAG from 8 workers, deadlock resolution,
interner); `scripts/partest.nu N` (twelve expressions at 1 versus N
workers: shared graph, deep fan-out, cycles in one and two ranges,
errors, `tryEval`, trace/warn exactly once, concurrent import,
derivations, JSON); `IETSP_WORKERS=8 scripts/langtests.nu` (same counts
as sequential); `IETSP_WORKERS=8 scripts/difftest.nu` on nixos-desktop
and nixos-minimal (`ok`).

## Measurements

Why the default is 4 workers: `bench/workloads/nixos-desktop.nix` and
`nixos-minimal.nix`, 16-core EPYC, hyperfine 8 runs, after the second pass
(wake policy, shared tables, flat AST; earlier passes in docs/history.md):

| workload | workers | wall | user | sys |
|---|---|---|---|---|
| nixos-desktop | 1 | 1.67 s | 1.44 s | 0.21 s |
| nixos-desktop | 4 | 1.01 s | 2.37 s | 0.31 s |
| nixos-desktop | 16 | 1.27 s | 5.88 s | 0.74 s |
| nixos-minimal | 1 | 1.32 s | 1.16 s | 0.15 s |
| nixos-minimal | 4 | 0.89 s | 1.95 s | 0.27 s |
| nixos-minimal | 16 | 1.18 s | 4.86 s | 0.59 s |

More workers cost user time and gain no wall: the critical chain sets the
floor. At 8 workers main spends ~0.95 s of 1.22 s parked while helpers walk
that chain (`IETSP_PAR_STATS=1|2`). The chain is the module fixpoint
(`lib/modules.nix` `mergeDefinitions`, no strict consumer to fan out from)
and imports: on nixos-minimal at 4 workers, one helper spends ~0.17 s alone
reading and compiling `hackage-packages.nix` (16.6 MB) while everyone else
waits on it. Only parse and compile speed moves that phase; the IR cache
(docs/memory.md) removes it on warm runs.

## Merge notes for the sequential work

- `Evaluator` derives `Clone`; a new field must be `Clone`, and per-fiber
  state must be cheap to clone (shared tables go behind a handle in
  `parallel::shared`).
- An inline cache on an IR node or a heap object is read and written by
  several threads: use atomics, and expect stale misses.
- A new `thread_local!` must be read through an `#[inline(never)]`
  function that does not force anything.
- A new strict list or attrset consumer can call
  `parallel::fanout_list` / `fanout_attrs_coerce` after forcing the
  container (one line; a no-op with one worker).
- A new place that writes a forced value into a cell must do so only on
  `Ok` (all current sites do), or a cancelled helper could leak a value.
- `Files` (shared) owns the global `Pos` base offsets: `add` hands out
  the next base under the lock, `file_of` searches under it. The source
  text is not kept.

## Not done

ThreadSanitizer needs nightly; not run. Invariants above are the
argument. No loom model. Sibling prefetch not implemented. The aarch64
context switch is written but untested.

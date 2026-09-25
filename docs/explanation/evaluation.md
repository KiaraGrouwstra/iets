# How ietsp evaluates

ietsp is a tree-walking evaluator over a compiled intermediate
representation. It is lazy, like Nix, and it forces thunks on several
threads at once. This page explains why those two properties can hold
together, and why the memory layout is the thing that decides how large a
run gets.

## The pipeline

```
source bytes
  -> syntax::lexer / syntax::parser   -> ast::Ast (per file, arena of Expr)
  -> compile                          -> ir::Ir (scopes resolved, attrpaths merged)
  -> eval                             -> runtime::Word (lazy graph, forced on demand)
  -> print / json / drvPath
```

The parser keeps raw bindings. Attribute-path merging and scope resolution
happen in the compile step, so the evaluator never searches a scope by
name: every variable is already a `Local { depth, index }`, or a `WithVar`
when no lexical binder exists. Variable-length node payloads live in side
tables addressed by a range, so one file costs nine allocations instead of
one per node.

## Laziness, and what a thunk costs

Every argument, attribute value, list element and `let` binding that is
not cheap becomes a thunk. A cheap expression — a literal, a variable —
reuses the existing word or cell, so a variable that holds a thunk shares
that thunk pointer instead of allocating a second one.

Forcing overwrites the thunk object in place. A thunk is two words: `code`
holds the IR or the function and becomes the value; `state` holds the
environment or the argument and becomes empty. Cells that still point at
the thunk are rewritten with the value as they are read. That is graph
reduction, and it is why a shared subexpression is evaluated once.

`mapAttrs` is the case where even two words were too many. On the outpaths
workload there were 22 million `mapAttrs` thunks and 93 % of them were
never forced. A `mapAttrs` entry now has no thunk object at all: a
`THUNK_MAP` word carries a descriptor pointer and an entry index, and one
`MapDesc` per call holds an 8-byte state cell per entry. Sets over 65,536
entries fall back to an ordinary two-argument thunk. That change alone took
the arena from 8,936 MB to 7,981 MB, with the run time inside the noise.

## The value model

A `Word` is one 8-byte word with a 3-bit tag in the low bits.

| tag | meaning |
|---|---|
| `INT` | 61-bit immediate integer; a larger integer boxes to a heap object |
| `PTR` | pointer to a heap object with a one-byte kind header |
| `THUNK`, `THUNK_APP`, `THUNK_APP2` | pointer to a 16- or 24-byte thunk object |
| `THUNK_MAP` | a `mapAttrs` entry: descriptor pointer plus index, no object |
| `LAMBDA` | pointer to a two-word closure |
| `BLACKHOLE` | thunk under evaluation |
| `NULL`, `TRUE`, `FALSE` | immediates |

A `Cell` is an `AtomicU64` holding a `Word`. Environment slots, list
elements and attribute values are all cells. That single decision is what
lets the sequential path and the parallel path share one shape: there is no
"parallel value" and no "sequential value", only a cell that happens to be
read by one thread or by four.

Attributes are sorted by symbol id, as they are in Nix. Name order — what
`attrNames`, printing, JSON and a derivation environment need — is computed
on demand.

## `//` builds a union, not a copy

When the left side has at least eight entries, `a // b` is not a merged
copy. It is a union object: a tail with the flat leaves of both sides, at
most eight, lowest priority first. A lookup probes the leaves from the
last. An iteration merges them as it goes. A consumer that needs one
contiguous slice asks for a flat copy, which is made once and cached.

The reason is measurement, not taste. On outpaths, `//` copied 5.12 GB, of
which 3.94 GB was left sides, and 80 % of the results were never iterated
at all. The union took the arena from 13,253 MB to 8,936 MB. An earlier
version that kept recursive `(a, b)` pointers instead of inline leaves cost
3.8 % of wall time; inline leaves cost nothing.

## Strings shared by content

The arena keeps a table of string bytes by 64-bit content hash, the
context-free string object over them, and a set of context slices. A string
without context shares the whole object; a string with context shares the
bytes. Interpolation joins its pieces in the arena and gives them back when
the content already exists.

On outpaths, 5.93 million of 7.31 million strings repeated bytes that were
already there — 662 MB. Sharing them took the arena from 7,981 MB to
7,106 MB, for +3 % wall at one worker and −7 % at four. Keying the table by
the bytes instead of by their hash cost 6 % and 12 %, so the hash stayed.

## Why there is no garbage collector

There is one bump arena per worker and nothing is ever freed. Every object
lives until the process exits. This is why object layout, not allocation
speed, decides peak memory, and it is why the four packing changes above
were worth their complexity.

Four changes, measured from commit `e6b8971` to `155cbdf`:

| | before | after | Nix for scale |
|---|---|---|---|
| desktop arena | 937 MB | 629 MB | |
| desktop peak RSS, 1 worker | 988 MB | 659 MB | 690 MB (`nix-instantiate`) |
| outpaths arena | 13,253 MB | 7,106 MB | |
| outpaths peak RSS, 1 worker | 13,495 MB | 7,219 MB | 4.8 GB per nix-eval-jobs worker, restarted at that size |
| outpaths wall, 1 / 4 workers | 32.7 / 15.2 s | 32.5 / 13.5 s | |

What is left is thunks, frames and attribute sets the program can still
reach. No layout change reduces that. Only a collector or a restart bounds
it, which is why `eval-jobs` restarts its worker processes at
`--max-memory-size` instead.

A census of the outpaths run after those four changes named six more ideas
and priced them all out: attribute positions in a side table (a 16-byte
attribute pads back to 16 without a struct-of-arrays rewrite), compiling
`hackage-packages.nix` lazily (~15 MB, but the AST stays alive), flattening
`let` frames (under 56 MB), a 24-byte string header (43 MB), one shared
closure for lambdas that capture nothing (16 MB), and one shared empty
attribute set (22 MB). Scoped release without a collector was ruled out
too: failed `tryEval` scopes held 1.28 MB and not one was free of escapes,
and per-unit scopes in eval-jobs are 0.9 % of the arena, the rest being the
package set the next unit reuses.

## The fiber scheduler

Parallel evaluation is implicit. Worker threads force thunks ahead of and
beside the demand path. Four units make it up:

- a **fiber** is a stackful coroutine with its own stack and its own
  evaluator. It is the unit of blocking, and it migrates: a fiber that
  parks on worker A can resume on worker B;
- a **worker** is an OS thread running a drain loop. It owns a bump arena,
  a fiber pool and work-stealing deques;
- a **task** is work not yet started — force a range of list cells or
  attribute cells. It runs in a pooled fiber;
- the **main fiber** runs the command's own closure. When it returns,
  evaluation is done.

Each fiber's evaluator is a clone of the root one. Per-fiber state is
private. Shared state — the interner, the file table, the import cache, the
derivation table — sits behind handles that clone by reference. A fiber's
arena field is swapped with the worker's arena on every resume and swapped
back on every yield, so N workers own N arenas and no fiber ever allocates
from an arena another thread is using. Pointers are global, and cross-arena
references are fine, because no arena is ever freed.

### The claim protocol

The thunk's *state* cell is the synchronisation word. Its three states are
disjoint: a payload, a blackhole, or empty.

```
payload ──CAS──> BLACKHOLE(claimer, waiters bit) ──store──> EMPTY
   ^                          │
   └────────── reset ─────────┘   (error, cancel, or traced-in-helper)
```

A force loads the word first and only attempts the compare-exchange on a
payload word. That ordering is deliberate: a failed compare-exchange still
takes the cache line exclusive, and most forces hit a thunk another fiber
has already evaluated — everything in `lib`, everything in `pkgs` — so the
plain load keeps hot shared lines shared.

The winner gets the payload back and evaluates. A loser reads the current
word. If it is done, it returns the value. If it is a blackhole holding its
*own* claimer id, that is infinite recursion; the id is per fiber, not per
thread, so it follows a computation that migrated. If another fiber holds
it, the loser parks on a side table sharded by thunk address, so thunk
objects never grow.

### Why more workers stop helping

The default is four. The reason is the critical chain, not contention:

| workload | workers | wall | user | sys |
|---|---|---|---|---|
| nixos-desktop | 1 | 1.67 s | 1.44 s | 0.21 s |
| nixos-desktop | 4 | 1.01 s | 2.37 s | 0.31 s |
| nixos-desktop | 16 | 1.27 s | 5.88 s | 0.74 s |
| nixos-minimal | 1 | 1.32 s | 1.16 s | 0.15 s |
| nixos-minimal | 4 | 0.89 s | 1.95 s | 0.27 s |
| nixos-minimal | 16 | 1.18 s | 4.86 s | 0.59 s |

At eight workers the main fiber spends about 0.95 s of 1.22 s parked while
helpers walk one chain. The chain is the module fixpoint in
`lib/modules.nix`, which has no strict consumer to fan out from, plus
imports: on nixos-minimal at four workers one helper spends about 0.17 s
alone reading and compiling the 16.6 MB `hackage-packages.nix` while
everyone else waits. Only parse and compile speed move that phase, which is
what the IR cache does on a warm run (see
[caching](caching.md)).

The wake policy is where the scheduler's own cost showed. Waking a sleeper
on every push cost the critical path a futex call per fan-out task and
handed cache-warm work to cold cores: 26,000 parks per run at sixteen
workers and 0.9 s of system time. Waking only on a handoff — a ready fiber
always, queued tasks only past a threshold — brought that to about 2,000.

## Why the answer is identical

Three properties together:

1. **Forcing is pure.** The value graph is the same whichever fiber forces
   a thunk.
2. **A thunk is published at most once per successful evaluation.** A reset
   followed by a re-evaluation yields the same value, because the input did
   not change.
3. **Attribute order, list order and derivation hashing do not depend on
   scheduling.** They are decided by symbol ids and by the ATerm, both of
   which are computed from the program.

Two effects would break purity, and both are handled by giving them back to
the demand path. `builtins.trace` and `builtins.warn` in a helper fiber
print nothing: the helper sets a sticky cancel flag and abandons the task.
Every thunk still open in that fiber is reset, and because a cancelled
force returns an error, no value is written back into any list, attribute
or environment cell. The demand path then evaluates that thunk itself and
prints, exactly once. An earlier design that journalled the warning and
kept computing lost warnings, because the write-back happened and demand
never re-evaluated.

Cancel is sticky for the same reason. Once set, every publish the fiber
would still do becomes a reset, so a swallowed error cannot leak a value
built from a bailed sub-computation into a shared thunk.

Two divergences are known and accepted. The order of trace lines on stderr
can differ, because the demand walk does every unclaimed cell first and
parks on busy ones afterwards. And a helper starts at call depth zero, so a
program that fails only by exceeding the maximum call depth sequentially may
succeed in parallel.

### Deadlock, and how it resolves

Fan-out can claim the two halves of a cycle on two fibers, which then wait
on each other. Sequential evaluation would have reported infinite recursion
instead. The last worker to go to sleep — when every worker sleeps, nothing
is pending and main is not done — cancels every parked helper fiber. Each
one unwinds, every thunk it claimed is reset, its waiters wake, and the main
fiber re-evaluates the cycle sequentially and reports the error the
sequential path would have reported.

## What is not proven

ThreadSanitizer needs a nightly compiler and has not been run. There is no
model checker. The invariants above are the argument, and the tests are the
evidence: a claim-and-park stress test on a shared graph from eight workers,
the deadlock resolution, the scheduler handshake, twelve expressions
compared at one against N workers, the Nix language suite at eight workers,
and the drvPath comparison against real `nix-instantiate` at eight workers.
The aarch64 context switch is written and untested.

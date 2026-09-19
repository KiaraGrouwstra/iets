# Memory

ietsp has one bump arena per worker and no garbage collector: every object
lives until the process exits. So the layout of each object decides the
peak RSS. This document describes the layouts that exist and gives the
numbers that justify them. Rejected ideas are in docs/history.md.

Workloads (`NP` is the pinned nixpkgs):

- desktop: `ietsp eval --strict --no-cache --workers 1 -I nixpkgs=$NP bench/workloads/nixos-desktop.nix`
- outpaths: chunk 0 of 8 of `ci/eval/outpaths.nix` for x86_64-linux,
  `ietsp eval-jobs --no-instantiate --workers 1 --chunks 8 --chunk 0`

Measurement: `IETSP_STATS=1` prints allocation counts and bytes per object
kind, RSS and glibc's `malloc_stats`. `IETSP_STATS=2` on a binary built
with `--features stats2` adds the census (histograms, duplicates, objects
never read); the hooks are compiled out otherwise. `bench/perf.nu` prints
wall, user, sys and peak RSS for one binary; `bench/ab.nu` compares two.

## Where it stands

Four packing changes, measured from commit `e6b8971` to `155cbdf`:

| | before | after | Nix for scale |
|---|---|---|---|
| desktop arena | 937 MB | 629 MB | |
| desktop peak RSS, 1 worker | 988 MB | 659 MB | 690 MB (`nix-instantiate`) |
| outpaths arena | 13253 MB | 7106 MB | |
| outpaths peak RSS, 1 worker | 13495 MB | 7219 MB | 4.8 GB per nix-eval-jobs worker, restarted at that size |
| outpaths wall, 1 / 4 workers | 32.7 / 15.2 s | 32.5 / 13.5 s | |

What remains is thunks, frames and attrsets that the program reaches. Only
a collector or a restart strategy shrinks that: eval-jobs keeps
`--max-memory-size` for it (docs/eval-jobs.md).

## `//` builds a union, not a copy

When the left side has at least 8 entries, `a // b` is an `AttrsObj` of
kind `AttrsUnion`: a `UnionTail` with a `flat` cell and the flat leaves
of both sides, at most 8, lowest priority first. `get` probes the leaves
from the last, in a loop; past 8 leaves the left side is flattened first.
An iteration merges the leaves on the fly (`AttrsObj::iter`). A consumer
that needs one slice (fan-out by index, the eval-jobs walk) calls
`Arena::flat`, which copies once and caches the copy in `flat`. Smaller
left sides merge flat.

Why: on outpaths `//` copied 5.12 GB, 3.94 GB of it left sides, and 80 %
of the results were never iterated. The union took the arena from 13253 to
8936 MB. A first version with recursive `(a, b)` pointers cost 3.8 % wall;
inline leaves do not.

## Thunks overwritten in place

A thunk is two words: `code` (the IR or the function, overwritten with the
value) and `state` (the env or the argument, then BLACKHOLE, then EMPTY).
`ThunkApp2` keeps its first argument as a third word. The claim protocol
runs on the state cell (docs/parallel.md). A `mapAttrs` entry has no thunk
object: a `THUNK_MAP` word (descriptor pointer and entry index) sits in the
result's own cell, and one `MapDesc { f, src, out }` per call holds an
8-byte state cell per entry. Sets over 65536 entries fall back to
`ThunkApp2`.

Why: 80 M thunks and 22 M `mapAttrs` thunks on outpaths, 93 % of the
latter never forced. Arena 8936 to 7981 MB, time within noise.

## Strings and contexts shared by content

The arena keeps a table of string bytes by 64-bit content hash, plus the
context-free string object over them, and a set of context slices.
`mk_string`, interpolation and the builtins that build bytes look the
content up first: a string without context shares the object, a string
with context shares the bytes. Interpolation joins its pieces in the arena
and gives them back (`Arena::mark`/`release`) when the content exists.
The tables are per arena.

Why: 5.93 M of 7.31 M strings on outpaths repeated earlier bytes (662 MB).
Arena 7981 to 7106 MB for +3 % wall at one worker and -7 % at four.
Keying the table by `&[u8]` instead of the hash cost 6 % and 12 %.

## ATerm text kept only when something reads it

`DrvState::keep_aterm` is off for `eval-jobs --no-instantiate` and for
`eval` with neither a cache nor `--dump-drvs`. Instantiation, the tracing
cache, `build` and `print-dev-env` read the text; everything else handles
`DrvInfo.aterm = None`, the state of a derivation read from `/nix/store`.

Why: 331 MB of heap on outpaths; peak RSS 7680 to 7219 MB.

## IR cache

`src/compile/cache.rs` writes each imported file's compiled IR after its
first compile and loads it on later runs. Entry:
`${IETSP_CACHE_DIR:-~/.cache/ietsp}/ir/<blake3 of build id, path, system,
store dir, source>`, a varint pre-order stream with a per-entry symbol
table. Three things make IR process-specific, and the loader undoes each:
`Sym` ids (names re-interned), `Pos` (stored relative to the file,
rebased), and constant words (literals by content, globals and
`builtins.x` by name). Binding frames are sorted by `Sym` id, which
differs between processes: the loader re-sorts them and rewrites every
`Local` index through a permutation stack.

Only `import` uses the cache; `scopedImport`, `-E`, and files with a `~/`
path compile from source. The compile-time lint levels are part of the key
and the entry carries the lint warnings the compile produced
(docs/lints.md). `--no-ir-cache` or
`IETSP_CACHE=0` turns it off; `--no-cache` leaves it on.
`IETSP_IR_STATS=1` prints a line per file. Locking, touching, garbage
collection and the version file are shared with the tracing cache:
docs/caches.md.

Why: at one worker, parsing and compiling the 3,500 files of desktop was
~28 % of the run. All files load in 229 ms against 696 ms to compile;
`hackage-packages.nix` alone in 71-77 ms against 190-218 ms. A/B
(`bench/ab.nu`, 7 runs, warm cache): desktop w1 -15.8 %, minimal w1
-19.2 %, outpaths w1 -2.2 %, desktop w4 within noise, RSS unchanged.
Files under 1 KiB (`IETSP_IR_MIN`) are not cached: below that the load
costs what the compile costs.

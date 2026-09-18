# Memory

ietsp has one bump arena and no garbage collector: every object lives until
the process exits. This document is the census of that arena on two
workloads, the packing changes made from it, the running perf table for
each change, and what was assessed and rejected.

Workloads (`NP` is the pinned nixpkgs):

- desktop: `ietsp eval --strict --no-cache --workers 1 -I nixpkgs=$NP bench/workloads/nixos-desktop.nix`
- outpaths: chunk 0 of 8 of `ci/eval/outpaths.nix` for x86_64-linux,
  `ietsp eval-jobs --no-instantiate --workers 1 --chunks 8 --chunk 0`

Measurement: `IETSP_STATS=1` prints allocation counts and bytes per object
kind plus RSS and glibc's `malloc_stats`. `IETSP_STATS=2` on a binary built
with `--features stats2` adds the census below (histograms, duplicates,
objects never read); the hooks are compiled out otherwise. `bench/perf.nu`
prints the perf table rows for one binary.

## Census before any change (commit e6b8971)

Arena bytes by kind. desktop: 937 MB arena, 1012 MB RSS. outpaths: 13253 MB
arena, 13816 MB RSS.

| kind | desktop count | desktop MiB | outpaths count | outpaths MiB | outpaths share |
|---|---|---|---|---|---|
| attrs from `//` | 330802 | 237.3 | 5599282 | 5123.2 | 38.7 % |
| thunk (24 B) | 6410573 | 146.7 | 80025257 | 1831.6 | 13.8 % |
| env | 6138567 | 115.0 | 59031809 | 1262.5 | 9.5 % |
| string bytes | 432799 | 25.3 | 5393371 | 745.2 | 5.6 % |
| thunk_app2 (`mapAttrs`, 32 B) | 1784117 | 54.4 | 22365208 | 682.5 | 5.2 % |
| attrs literal | 1262435 | 71.9 | 11193244 | 645.8 | 4.9 % |
| lambda (16 B) | 3030727 | 46.2 | 20789681 | 317.2 | 2.4 % |
| attrs from `mapAttrs` | 259504 | 27.3 | 1123400 | 295.9 | 2.2 % |
| string objects (40 B) | 559567 | 21.3 | 7310915 | 278.9 | 2.1 % |
| attrs other builtins | 77850 | 6.8 | 2332132 | 277.1 | 2.1 % |
| IR | 3502 files | 101.2 | 33459 files | 266.2 | 2.0 % |
| attrs from `removeAttrs` | 76828 | 5.8 | 1314323 | 228.7 | 1.7 % |
| list literal | 900181 | 11.6 | 16103049 | 165.0 | 1.2 % |
| list other builtins | 232434 | 5.7 | 4997052 | 100.5 | 0.8 % |
| thunk_app (`map`, `genList`, 24 B) | 481046 | 11.0 | 4208291 | 96.3 | 0.7 % |
| list `concatLists`/`++` | 132019 | 3.7 | 1270441 | 68.1 | 0.5 % |
| attrs `zipAttrsWith` | 26611 | 2.1 | 66536 | 54.4 | 0.4 % |
| context slices | 111873 | 2.0 | 3188260 | 51.4 | 0.4 % |
| list `map` | 198542 | 4.5 | 1622905 | 41.3 | 0.3 % |
| attrs `listToAttrs` | 18065 | 0.8 | 849599 | 32.9 | 0.2 % |
| attrs `intersectAttrs` | 9948 | 0.4 | 461329 | 18.1 | 0.1 % |
| primop_app | 61293 | 2.6 | 401536 | 16.6 | 0.1 % |
| path | 19143 | 2.1 | 204491 | 11.2 | 0.1 % |
| list `genList` | 51091 | 1.0 | 158626 | 4.2 | 0.0 % |
| float, bigint | 84 | 0.0 | 97 | 0.0 | 0.0 % |

Non-arena memory (outpaths): derivation table 79812 entries holding
331 MB of ATerm text; line tables 21 MB; file paths 3 MB; interner 205065
symbols (3 MB of names, ~6.5 MB of map); import cache 33458 entries;
regex cache 100 entries; glibc heap 56 MB on desktop (mostly the
compiler's scratch and the derivation table). The AST is dropped after each
file is compiled. RSS minus arena is 75 MB on desktop and 560 MB on
outpaths (the ATerms). Fiber stacks: none at one worker; 256 MB reserved
and 64 KB kept resident per pooled fiber otherwise.

### Histograms and dead objects (outpaths; desktop in parentheses)

Thunks: 80.0 M created, 45.3 M forced: 35 M thunks (840 MB) are never
forced (desktop: 6.4 M created, 3.9 M forced). `mapAttrs` thunks: 22.4 M
created, 1.6 M forced: 93 % dead, 665 MB. `map`/`genList` thunks: 98 %
forced.

Envs by slot count: 1 slot 52.0 M (832 MB), 2 slots 2.8 M, 3-4 1.3 M,
5-8 1.5 M, 9-16 0.4 M, 17-32 0.6 M (103 MB), 33-64 0.4 M (116 MB).
Never read: 8.0 M (132 MB), almost all 1-slot frames of lambdas that
ignore their argument. Never captured by a thunk, closure or child env:
25.5 M 1-slot frames (415 MB): the body used the argument directly and
nothing kept the frame.

Attrsets by size: literals are small (2.7 M empty, 1.2 M of one entry,
4.6 M of two); `//` results are large (2.0 M of 33-64 entries, 0.9 M of
9-16, 0.8 M of 17-32, 0.9 M of 5-8). `//` shapes: average left 46.1
entries, right 15.5; in 2.65 M of 5.60 M cases the result is more than
four times the right side; the left side is itself a `//` result in 2.59 M
cases (chains) and the right side in 1.47 M. Bytes spent copying left
sides: 3.94 GB of the 5.12 GB. Use of `//` results, iteration by `//`
itself excluded: 2.17 M (1.81 GB) never selected from nor iterated, 1.0 M
selected once only, 1.15 M (20 %) iterated by a consumer
(`derivationStrict`, `removeAttrs`, `mapAttrs`, `attrNames`, printing).

Strings by length: 5.4 M under 128 bytes carry 199 MB; 455 k over 256
bytes carry 608 MB. Duplicates by content: 5.93 M of 7.31 M strings, 662
MB of bytes (desktop: 308 k of 560 k, 16.5 MB). Never viewed as a string:
1.66 M of 7.31 M (mostly literals in code that never ran). Contexts:
0.91 M of 1.09 M slices duplicate an existing one (47 MB of 51 MB).

Lists never read: 3.6 M of 24.2 M (63 MB). Closures of lambdas that
capture nothing: 1.05 M of 20.8 M (16 MB). `unsafeGetAttrPos` calls:
192181 (2234 on desktop). IR: one file, `hackage-packages.nix` (16.6 MB
of source), compiles to 41.4 MB of IR; the next largest is 10.5 MB.

## Changes

Each change has one commit; the message carries the numbers. The running
perf table is at the end.

### 1. `//` builds a union, not a copy

`a // b` used to copy both sides into a new sorted array: 5.12 GB on
outpaths, 3.94 GB of it copies of the left side, and 80 % of the results
are never iterated. Now, when the left side has at least 8 entries, the
result is a 32-byte `AttrsUnion { a, b, flat }`: `get` probes `b` then
`a` (recursively for chains), an iteration merges the flat leaves on the
fly (`AttrsObj::iter`, a k-way merge over at most 8 leaves), and consumers
that need one contiguous slice (fan-out by index, the eval-jobs walk,
`attrs()`) call `Arena::flat`, which makes the copy once and caches it in
`flat`. A union may hold at most 8 leaves; past that the left side is
flattened first, so a lookup miss costs at most 8 probes. `len` is
computed on demand and cached. Small left sides (under 8 entries) still
merge flat: cheaper than the union plus a probe.

Result: outpaths arena 13253 → 8936 MB (−4317 MB, −32.6 %), peak RSS
13495 → 9398 MB; `//` bytes 5123 → 1006 MiB (5.8 M unions plus 820 MiB
of flat copies for the iterated 20 %). desktop arena 937 → 741 MB
(−21 %), RSS 988 → 796 MB. Output byte-identical. Time: outpaths 32.73 →
31.92 s at 1 worker (user +4 %, sys −26 %: fewer pages to populate),
15.21 → 14.61 s at 4; desktop 1.85 → 1.72 s and 1.45 → 1.15 s. A first
version kept `(a, b)` pointers and probed recursively; it cost +3.8 %
wall on outpaths at 1 worker, so the leaves are stored inline and probed
in a loop.

### 2. Thunks overwritten in place; `mapAttrs` entries without a thunk object

A thunk was `{ result, env, ir }` (24 B), a `map` thunk `{ result, func,
arg }` (24 B), a `mapAttrs` thunk `{ result, func, name, value }` (32 B):
80 M, 4 M and 22 M of them on outpaths, 2.6 GB, and 35 M of the plain
thunks plus 93 % of the `mapAttrs` thunks are never forced. Now a thunk
is two words: `code` (the IR or function, overwritten with the value) and
`state` (the env or argument, then BLACKHOLE, then EMPTY); `ThunkApp2`
keeps its first argument as a third word. The env and IR pointers do not
fit one word together (48 + 48 bits; IR ids would need a dedicated
contiguous IR arena), so the in-place overwrite is what makes 16 bytes
possible. The claim protocol moved from "EMPTY → BLACKHOLE → value" to
"payload → BLACKHOLE → EMPTY" on the state cell (docs/parallel.md);
payloads are never EMPTY or a BLACKHOLE, so the states stay disjoint.

`mapAttrs` gets no thunk object at all: a `THUNK_MAP` word (tag 111,
descriptor pointer in bits 3..48, entry index in bits 48..64) sits in the
result's own attr cell, and one `MapDesc { f, src, out }` per call holds
one 8-byte state cell per entry. Forcing reads the name from the result's
sym and the value from the source's cell at the same index. A first cut
kept the state in the attr cell itself; it crashed at 4 workers because
readers of `attr.cell` saw a BLACKHOLE as a value, hence the state array
(8 B per entry instead of 0, still 24 B less than the object). Sets over
65536 entries fall back to `ThunkApp2`.

Result: outpaths arena 8936 → 7981 MB (−955 MB, −10.7 %), peak RSS 9398
→ 8437 MB: thunks 1832 → 1221 MiB, `map` thunks 96 → 64 MiB, `mapAttrs`
thunks 683 → 169 MiB of state cells plus 81 MiB of `ThunkApp2` for
`zipAttrsWith`. desktop arena 741 → 657 MB, RSS 796 → 700 MB. Time within
noise: outpaths 31.92 → 32.10 s at 1 worker, 14.61 → 14.45 at 4; desktop
1.72 → 1.71 and 1.15 → 1.00. Output byte-identical.

### 3. Strings and contexts shared by content

5.93 M of the 7.31 M strings on outpaths repeat the bytes of an earlier
one (662 MB of 745 MB), and 0.91 M of 1.09 M context slices repeat too.
The arena now keeps `strings: FxHashMap<&[u8], Word>` (bytes by content,
plus the context-free string object over them once one exists) and
`ctxs: FxHashSet<&[Ctx]>`. `mk_string`, `mk_string_ctx`, interpolation
(`eval_concat`) and the builtins that build a `Vec<u8>` (`substring`,
`replaceStrings`, `concatStringsSep`, `toJSON`, `toXML`, `readFile`,
`baseNameOf`, ...) look the content up first; a string with an empty
context shares the object, a string with context shares the bytes.
Interpolation joins its pieces in the arena and gives the bytes back
(`Arena::mark`/`release`) when the content already exists, so no scratch
buffer is needed. The tables are per arena (per fiber in parallel runs);
sharing across fibers is not required for correctness. The table is
keyed by the 64-bit content hash (probes compare integers, the bytes are
read only on a hit, a new string is hashed once); a first version keyed
by `&[u8]` cost 6 % wall on outpaths at 1 worker and 12 % at 4.

Result: outpaths arena 7981 → 7106 MB (−875 MB, −11 %), peak RSS 8437 →
7680 MB: string objects 279 → 108 MiB, bytes 745 → 169 MiB, contexts 51
→ 8 MiB; the tables cost ~60 MB of heap. desktop RSS 700 → 676 MB. Time:
outpaths 32.10 → 33.13 s at 1 worker (+3.2 %; user +6 %, sys −10 %) and
14.45 → 13.46 s at 4 (−7 %); desktop 1.71 → 1.80 s and 1.00 → 1.05 s,
within the ±5 % run-to-run noise of this box. Kept: 9 % of RSS for 3 %
of wall at one worker, and a gain at four.

### 4. ATerm text kept only when something reads it

`derivationStrict` stored the ATerm of every derivation in the derivation
table (79812 entries, 331 MB of heap on outpaths) although only
instantiation, the tracing cache, `--dump-drvs`, `build` and
`print-dev-env` read it. `DrvState::keep_aterm` (default on) is switched
off for `eval-jobs --no-instantiate` and for `eval` runs with neither a
cache nor `--dump-drvs`. `DrvInfo.aterm = None` was already the state of
a derivation read back from `/nix/store`, so every reader handles it.

Result: outpaths peak RSS 7680 → 7219 MB (glibc heap in use 825 → 401
MB); the arena is unchanged. Time within noise: outpaths 33.13 → 32.47 s
at 1 worker, 13.46 → 13.47 at 4. The desktop rows of this commit were
taken while the box had turned slower (the commit-2 binary re-measured
in the same session gave 1.88 / 1.37 s against its 1.71 / 1.00 earlier);
side by side the two binaries are within 4 %, and the desktop path keeps
no ATerm under `--no-cache`, RSS 676 → 659 MB.

### 5. Compiled-IR cache on disk

At one worker, parsing and compiling the 3,500 files of nixos-desktop is
~28 % of the run (docs/parallel.md; `hackage-packages.nix` alone is
0.19 s). `src/compile/cache.rs` writes each imported file's IR after its
first compile and loads it on later runs. Entry:
`${IETSP_CACHE_DIR:-~/.cache/ietsp}/ir/<blake3 of build id, path,
system, store dir, source>`; a varint pre-order stream with a per-entry
symbol table. Three things make IR process-specific and the entry undoes
each: `Sym` ids (names re-interned on load), `Pos` (stored relative to the
file, rebased on the file's global base), and constant words (literals by
content; globals and `builtins.x` by name, looked up in the live tables
on load, so `__nixPath` follows the run). Binding frames (`let`, `rec`,
formals) are slot-ordered by `Sym` id, which differs between processes:
the loader re-sorts them by the new ids and rewrites every `Local` index
through a permutation stack that mirrors the compiler's scope pushes
(frames already in order, the common case, push no permutation).

Only `import` uses the cache (`scopedImport`, `-E`, and files with a
`~/` path literal or a lint diagnostic compile from source). Files under
1 KiB are not cached: below that the load (open, read, decode) costs
what the compile costs. `--no-ir-cache` or `IETSP_CACHE=0` disables it;
`--no-cache` (the tracing cache) leaves it on. `IETSP_IR_STATS=1` prints
a line per file, `IETSP_IR_MIN` overrides the size floor.

Per file on desktop (one cold and one warm run, 3,501 files):
`hackage-packages.nix` loads in 71-77 ms against 190-218 ms to parse and
compile (symbol table 3.6 ms, nodes 57 ms, the rest is the key hash,
the read and the line table); all files together load in 229 ms against
696 ms. The loader's profile is the compiler's minus the parser: string
interning, the line table's `memchr`, symbol interning, plus blake3 of
the source (7 %). A cold run (every entry written, 23 MB) costs the
compile plus 238 ms of serialize-and-write; that is the first run only.

A/B (`bench/ab.nu`, hyperfine `--warmup 1 --runs 7`, interleaved, warm
IR cache, 2026-09-18):

| workload | wall before | wall after | change | user before | user after | sys before | sys after | RSS before | RSS after |
|---|---|---|---|---|---|---|---|---|---|
| nixos-desktop w1 | 2.011 | 1.693 | -15.8% | 1.6 | 1.3 | 0.39 | 0.39 | 658 | 654 |
| nixos-desktop w4 | 1.556 | 1.623 | 4.3% | 2.74 | 2.67 | 0.86 | 0.85 | 799 | 812 |
| nixos-minimal w1 | 1.549 | 1.252 | -19.2% | 1.26 | 1.0 | 0.28 | 0.24 | 529 | 522 |
| outpaths 0/8 w1 | 33.469 | 32.729 | -2.2% | 28.98 | 28.0 | 4.48 | 4.45 | 7218 | 7215 |

nixos-desktop w4 is inside this box's noise: three interleaved measurements gave
-6.8 % (7 runs, before = 217e383), +4.3 % (7 runs) and, at 11 runs, median
+1.2 % with mean -1.2 % (1232-1493 ms against 1220-1543 ms, user 2.62 -> 2.55 s).

Kept: -15.8 % wall on desktop at one worker, -19.2 % on minimal, -2.2 % on outpaths at one
worker; RSS unchanged.

## Running perf table

`bench/perf.nu` rows: nixos-desktop is the median-wall run of 5; outpaths
chunk 0/8 is one run. Same 16-core box, idle.

| build | workload | workers | wall s | user s | sys s | peak RSS MB |
|---|---|---|---|---|---|---|
| e6b8971 before | nixos-desktop | 1 | 1.85 | 1.42 | 0.40 | 988 |
| e6b8971 before | nixos-desktop | 4 | 1.45 | 2.54 | 0.80 | 1077 |
| e6b8971 before | outpaths chunk 0/8 | 1 | 32.73 | 25.70 | 6.58 | 13495 |
| e6b8971 before | outpaths chunk 0/8 | 4 | 15.21 | 32.72 | 8.70 | 13733 |
| 1 union `//` | nixos-desktop | 1 | 1.72 | 1.49 | 0.21 | 796 |
| 1 union `//` | nixos-desktop | 4 | 1.15 | 2.54 | 0.44 | 885 |
| 1 union `//` | outpaths chunk 0/8 | 1 | 31.92 | 26.74 | 4.85 | 9398 |
| 1 union `//` | outpaths chunk 0/8 | 4 | 14.61 | 34.90 | 7.44 | 9801 |
| 2 thunks in place | nixos-desktop | 1 | 1.71 | 1.49 | 0.20 | 700 |
| 2 thunks in place | nixos-desktop | 4 | 1.00 | 2.37 | 0.32 | 793 |
| 2 thunks in place | outpaths chunk 0/8 | 1 | 32.10 | 27.01 | 4.67 | 8437 |
| 2 thunks in place | outpaths chunk 0/8 | 4 | 14.45 | 36.37 | 6.86 | 8811 |
| 3 strings by content | nixos-desktop | 1 | 1.80 | 1.59 | 0.19 | 676 |
| 3 strings by content | nixos-desktop | 4 | 1.05 | 2.49 | 0.34 | 833 |
| 3 strings by content | outpaths chunk 0/8 | 1 | 33.13 | 28.58 | 4.21 | 7680 |
| 3 strings by content | outpaths chunk 0/8 | 4 | 13.46 | 34.75 | 6.16 | 8138 |
| 4 no ATerm text | nixos-desktop | 1 | 2.08 | 1.60 | 0.44 | 659 |
| 4 no ATerm text | nixos-desktop | 4 | 1.42 | 2.63 | 0.70 | 808 |
| 4 no ATerm text | outpaths chunk 0/8 | 1 | 32.47 | 27.90 | 4.21 | 7219 |
| 4 no ATerm text | outpaths chunk 0/8 | 4 | 13.47 | 35.70 | 6.13 | 7620 |
| 2 re-measured with 4 | nixos-desktop | 1 | 1.88 | 1.48 | 0.38 | 700 |
| 2 re-measured with 4 | nixos-desktop | 4 | 1.37 | 2.66 | 0.63 | 825 |

The desktop rows of change 4 and the re-measured change-2 rows come from
a later session in which every binary ran about 10 % slower (sys time
doubled); compare rows within a session.

### Single-thread ideas, 2026-09-18

`bench/ab.nu BEFORE AFTER`: both binaries in one hyperfine invocation
per workload (`--warmup 1 --runs 7`, runs interleaved), median wall,
mean user and sys, peak RSS of one extra run. Keep rule: wall improves
more than 2 % on desktop w1 or outpaths w1 with no regression above 1 %
elsewhere. Same 16-core box, idle, warm IR cache.

Idea 1, compiled-IR cache (kept; before = fb21901):

| workload | wall before | wall after | change | user before | user after | sys before | sys after | RSS before | RSS after |
|---|---|---|---|---|---|---|---|---|---|
| nixos-desktop w1 | 2.011 | 1.693 | -15.8% | 1.6 | 1.3 | 0.39 | 0.39 | 658 | 654 |
| nixos-desktop w4 | 1.556 | 1.623 | 4.3% | 2.74 | 2.67 | 0.86 | 0.85 | 799 | 812 |
| nixos-minimal w1 | 1.549 | 1.252 | -19.2% | 1.26 | 1.0 | 0.28 | 0.24 | 529 | 522 |
| outpaths 0/8 w1 | 33.469 | 32.729 | -2.2% | 28.98 | 28.0 | 4.48 | 4.45 | 7218 | 7215 |

nixos-desktop w4 is inside this box's noise: three interleaved measurements gave
-6.8 % (7 runs, before = 217e383), +4.3 % (7 runs) and, at 11 runs, median
+1.2 % with mean -1.2 % (1232-1493 ms against 1220-1543 ms, user 2.62 -> 2.55 s).


Idea 2, transient frames on a fiber-local stack (tried, rejected;
before = 02d7621). The census counted 25.5 M one-slot frames on outpaths
that nothing captured. A compile-time analysis (`Compiler::marks`: a
`let` or lambda is transient when its body compiles no lazy position
that thunks, no lambda, no `with`, no `rec` and no `inherit (e)` env)
proved 20.3 M of the 59.0 M frames on the chunk (desktop 1.06 M of
6.1 M), and those frames were pushed on a per-fiber chunked stack and
popped on return: envs in the arena 1262 → 947 MiB, outpaths peak RSS
7215 → 6895 MB, output byte-identical at 1 and 4 workers. The wall did
not follow. Env allocation is an inlined bump and the kernel is 1.1 % of
the one-worker profile, so the only win was locality:

| workload | wall before | wall after | change | user before | user after | sys before | sys after | RSS before | RSS after |
|---|---|---|---|---|---|---|---|---|---|
| nixos-desktop w1 | 1.523 | 1.561 | 2.5% | 1.28 | 1.3 | 0.22 | 0.26 | 654 | 654 |
| nixos-desktop w4 | 1.322 | 1.319 | -0.2% | 2.54 | 2.53 | 0.58 | 0.62 | 829 | 793 |
| nixos-minimal w1 | 1.148 | 1.146 | -0.1% | 0.99 | 0.99 | 0.15 | 0.15 | 522 | 521 |
| outpaths 0/8 w1 | 33.049 | 32.479 | -1.7% | 28.6 | 28.18 | 4.27 | 4.19 | 7215 | 6895 |

Reverted: −1.7 % on outpaths is under the 2 % bar and desktop w1 lost
2.5 %. The 320 MB of RSS would be the reason to bring it back as a
memory change; the diff is small (a `marks` counter in the compiler, a
`transient` flag on `LambdaIr` and `LetIr`, `eval::frames::FrameStack`).


Idea 3, inline caches on `Select` (tried, rejected; before = 1f0a477).
`AttrsObj::get` is 3.4-4.1 % of the one-worker profile, mostly the
binary search. Every `Select1` node got a `hint: AtomicU32` (it fits the
32-byte node) and a `SelectIr` one per path element; `get_hint` returns
the entry at the hinted index when its `sym` matches and searches and
stores the index otherwise. Keying on the index rather than the set
pointer makes every set of the same shape hit. Hit rate: desktop 2.56 M
hits / 0.55 M misses (82 %), outpaths chunk 8.71 M / 4.08 M (68 %).
Output byte-identical, partest 12/12. The wall went the other way:

| workload | wall before | wall after | change | user before | user after | sys before | sys after | RSS before | RSS after |
|---|---|---|---|---|---|---|---|---|---|
| nixos-desktop w1 | 1.479 | 1.512 | 2.2% | 1.29 | 1.32 | 0.2 | 0.19 | 654 | 654 |
| nixos-desktop w4 | 1.012 | 1.018 | 0.6% | 2.42 | 2.46 | 0.32 | 0.33 | 811 | 861 |
| nixos-minimal w1 | 1.135 | 1.201 | 5.8% | 0.99 | 1.03 | 0.13 | 0.15 | 522 | 522 |
| outpaths 0/8 w1 | 31.944 | 32.698 | 2.4% | 27.39 | 28.09 | 4.26 | 4.11 | 7215 | 7215 |

Reverted. A hit saves a binary search over a few cache-hot lines (sets
under 9 entries are a linear scan anyway) and costs a load of the IR
node's hint, a bounds check and a compare; a miss adds a store to a
shared IR node on top of the search, and at 68 % hits on outpaths that
store lands on lines other selects read. A two-entry cache would raise
the hit rate but not remove the store. Not pursued.


Idea 4, one escaped ATerm per derivation (tried, rejected; before =
521ca66). `derivationStrict` serializes every derivation three times
(masked hash-modulo input, text, unmasked hash-modulo) and walks the
input hashes twice, cloning each cached `DrvHash`; the profile has
`print_string` at 2.9 %, sha256 at 2.8 % and the allocator at ~5 %.
`store::drv::instantiate` escaped the derivation once into segments
(`Parts`), replayed the masked form straight into a sha256 hasher, built
the text once, hashed the unmasked form from the same segments, returned
cache borrows instead of clones, and `print_string` scanned with a
256-byte table. Output byte-identical, 133 tests including the 30 `.drv`
fixtures. User time fell 1-3 % on every workload; wall did not:

| workload | wall before | wall after | change | user before | user after | sys before | sys after | RSS before | RSS after |
|---|---|---|---|---|---|---|---|---|---|
| nixos-desktop w1 | 1.524 | 1.529 | 0.3% | 1.33 | 1.31 | 0.21 | 0.21 | 654 | 654 |
| nixos-desktop w4 | 1.055 | 1.021 | -3.3% | 2.47 | 2.4 | 0.33 | 0.33 | 822 | 817 |
| nixos-minimal w1 | 1.106 | 1.114 | 0.7% | 0.98 | 0.96 | 0.14 | 0.15 | 522 | 522 |
| outpaths 0/8 w1 | 31.887 | 31.517 | -1.2% | 27.58 | 27.23 | 4.08 | 5.11 | 7219 | 7219 |

Reverted: −1.2 % on outpaths is under the 2 % bar. The whole derivation
path is ~6 % of a one-worker run and the sha256 work (two hashes of the
ATerm per derivation, both required) is half of that, so the 2 % bar
was out of reach for any change to the serialization alone. The diff
(`+345/−60` in `src/store/drv.rs`, one call site in
`src/builtins/derivation.rs`) is a clean 1-3 % of user time if a later
pass wants it.


Where it stands after the four ideas: one kept (the IR cache), three
rejected with their numbers above. Against the starting point 217e383
on this box, desktop w1 1.92 → 1.70 s (−11.5 %, first session) and
1.52 s in the later, quieter sessions; outpaths chunk 0/8 w1 34.0 →
32.6 s (−4.3 %) and 31.9 s later; RSS unchanged. `bench/ab.nu` is the
runner for the next round.

## Assessed and not done

Numbers are from the outpaths census unless stated; percentages are of
the arena after change 3 (7.1 GB).

- **Attr positions in a side table.** An `Attr` is `{ sym: u32, pos: u32,
  cell: AtomicU64 }`, 16 bytes; dropping `pos` gives a 12-byte struct
  that alignment pads back to 16. Saving the 4 bytes needs a
  struct-of-arrays set (`syms[]`, `cells[]`, and `pos[]` only when
  known) and touches every `Attr` consumer. Literal sets could take their
  positions from `BindIr` for free (646 MB → −160 MB, 2.3 %); `//`
  results hold no entries any more; builtin results would still need an
  array. `unsafeGetAttrPos` runs 192181 times on the chunk
  (`mkDerivation`'s `meta.position`), so positions cannot be dropped.
  Not worth the churn.
- **IR.** 266 MB (3.7 %), 32 bytes per node, slices exact, AST dropped
  after each file, line tables 21 MB, interner ~10 MB. One file
  (`hackage-packages.nix`, 16.6 MB of source) is 41 MB of IR and is
  almost all unused; compiling its bindings on first force would keep
  the AST (24 bytes per node plus side tables, ~60 % of the IR) for the
  whole run, so the gain is ~15 MB per such file. Not done.
- **Env frames.** 1.26 GB (18 %): 52 M one-slot frames. Flattening `let`
  frames into the enclosing frame saves only the 8-byte header per
  `let` (all multi-slot frames together are 430 MB, so under 56 MB).
  Small envs inside the closure object does not apply: frames are
  per call, closures per creation. 7.9 M one-slot frames are never
  read (126 MB) and 25.5 M (415 MB, 5.8 %) are read but never captured
  by a thunk, closure or child frame: those could live on a per-fiber
  frame stack and be popped on return if the compiler proves the body
  creates nothing that holds the frame. That is the largest remaining
  packing item and is a compiler analysis plus a second allocator; not
  done here; tried as idea 2 (2026-09-18): 320 MB of RSS, no wall gain.
- **Lists.** `map` allocates a result list plus a 16-byte `ThunkApp` per
  element; 98 % of those thunks are forced, so a lazier form saves
  nothing. Lists never read: 3.6 M of 24 M (63 MB, 0.9 %).
- **Dead thunks.** 35 M of 80 M thunks are never forced (560 MB at 16
  bytes). They are laziness itself; only a smaller thunk helps, done.
- **String header.** `StringObj` is 40 bytes (kind, fat pointer to bytes,
  fat pointer to context); thin pointers with `u32` lengths in the header
  would make it 24 bytes: 2.7 M objects after dedup, 43 MB (0.6 %). Not
  done. Sharing bytes on `+` with a prefix already in the arena (rope or
  slice) is superseded by content sharing.
- **Closed lambdas.** 1.05 M of 20.8 M closures capture nothing (16 MB,
  0.2 %) and could be allocated once per `LambdaIr`. Not worth a cell in
  the IR.
- **Empty attrsets.** 2.7 M empty literal sets, 22 MB (0.3 %), could be
  one shared object. Not done.
- **Arena chunks.** 32 MiB chunks populated with `MADV_POPULATE_WRITE`;
  mapped minus used is one chunk (32-46 MB) at one worker. At four
  workers RSS is 370 MB above one worker: four per-worker arenas with
  their tails, pooled fiber stacks (64 KB resident each) and duplicated
  helper work. Not a lever.
- **Derivation table.** 79812 entries, ATerm text 331 MB plus the output
  and reference sets. Change 4 drops the text when nothing reads it.

## Freeing memory without a collector

Two scoped-release candidates were assessed with the `stats2` escape
census: a scope records the thunks created inside it, and every force of
a thunk created before the scope counts as an escape, because the forced
value (built inside the scope) is written into that older thunk, and
through it into older cells.

- **`tryEval` that failed.** outpaths: 2900 outermost `tryEval` calls,
  1781 failed, 1.28 MB allocated inside all failed scopes together, and
  not one failed scope without an escape (desktop: 25 calls, none
  failed). `check-meta` forces the package's `meta` and `name` thunks
  from inside the scope, so the values live on in the package set. Both
  the payoff (0.02 % of the arena) and the safety condition rule it out.
- **Per-unit scopes in eval-jobs.** 9244 units on the chunk; the walk's
  own allocation inside `job()` (coercing `drvPath`, the output paths,
  the JSON line) is 65 MB in total, 0.9 % of the arena. The other 99 %
  is the build graph forced through the package set's thunks, which the
  set keeps: every unit escapes by construction. Freeing it means
  un-memoizing the package set, which is what nix-eval-jobs's worker
  restarts do at 4 GB (docs/eval-jobs.md); ietsp keeps `--max-memory-size`
  for that.

What a real collector would reclaim, from the census after the changes:
the 35 M thunks that are never forced but stay reachable from cells (560
MB), the 8 M frames whose slot nothing read (126 MB) and the 25 M frames
nothing captured (415 MB), the flat copies unions make for one iteration,
the source-file IR after its last thunk is forced, and above all the
forced build graphs of finished eval-jobs units (the 7 GB that grows with
the number of jobs per process). Only the last item changes the picture:
a collector or a restart strategy is what bounds a long eval-jobs run;
everything else in this document is packing.

## Where it stands

After the four changes (commit 155cbdf), same measurement as the first
table:

| | before | after | Nix for scale |
|---|---|---|---|
| desktop arena | 937 MB | 629 MB | |
| desktop peak RSS, 1 worker | 988 MB | 659 MB | 690 MB (`nix-instantiate`) |
| outpaths arena | 13253 MB | 7106 MB | |
| outpaths peak RSS, 1 worker | 13495 MB | 7219 MB | 4.8 GB per nix-eval-jobs worker, restarted at that size |
| outpaths peak RSS, 4 workers | 13733 MB | 7620 MB | |
| outpaths wall, 1 / 4 workers | 32.7 / 15.2 s | 32.5 / 13.5 s | |

outpaths arena by kind after the changes (MiB): env 1263, thunk 1221
(80 M at 16 B), `//` 1175 (5.8 M unions plus the flat copies consumers
asked for: `mapAttrs` flattens its source, 170 MiB of those), attrs
literal 646, lambda 317, attrs from `mapAttrs` 296, attrs other builtins
277, IR 249, attrs `removeAttrs` 229, string bytes 169, `mapAttrs` state
cells 169, list literal 165, string objects 108, list other 101,
`ThunkApp2` 81, list `++` 68, `map` thunks 64, `zipAttrsWith` 54, list
`map` 41, `listToAttrs` 33, `intersectAttrs` 18, primop_app 17, path 11,
contexts 8. glibc heap in use 401 MB (derivation table without ATerms,
hash caches, string tables, line tables).

The next packing item by size is the frame stack for the 25 M frames
nothing captures (415 MB, 5.8 %), then the 8 M frames nothing reads (126
MB), then the string header (43 MB). Beyond that the arena is thunks,
frames and attrsets that the program reaches, and only a collector or a
restart strategy shrinks it.

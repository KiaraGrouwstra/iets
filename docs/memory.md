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

# History

What was tried, measured and removed, newest first. The code and the
full tables are in git at the commit each entry names. The numbers that
justify a current default live in the doc of that feature, not here.

## 2026-09-19: second slimming pass

The last build flow outside the dispatcher went. `nix-instantiate`,
`nix-shell` and `print-dev-env` had kept their own client flow
(`remote::worker::build::build_on`, 392 lines with its own `Timings`)
because folding them into `build::Dispatcher` risked classic stdout. They
now drive the worker backend (`build::backend::Worker`) through
`build::upload::PlanUpload`, and `examples/build.rs` uses the dispatcher;
classic-e2e stayed at 59/59 byte for byte. The e2e scripts share one
`scripts/lib.nu` (the farm, the isolation from the host config, the
scratch daemon). The history docs folded into this file.

## 2026-09-19: first slimming pass (`3945e70`, `4e882f9`)

Method: every `pub` became `pub(crate)`, the real external surface
(`main.rs`, `examples/`, `tests/`) was restored one compiler round at a
time, and rustc's `dead_code` lint then named 48 unreachable items. Cut:
those items (`store/context.rs`, a second string-context parser; ten
nix.conf getters nothing read; provenance and parallel helpers), creation
time speculation (below), and three one-implementation wrappers
(`ViewPrimop`, `Shared<T>`, `EvalDriver(SyncEv)`). Kept on purpose:
`flake::unpack`, the test oracle for `flake::spool`; the `is_empty` twins
that clippy's `len_without_is_empty` requires; `Kind::Env`, whose removal
shifts every heap discriminant. Proposed and not cut: one flag table for
the six argument parsers (about 120 lines of `match` against a 100-line
dispatcher, and the by-hand usage strings at risk), and one varint codec
for the three on-disk formats (about 40 net lines across three formats).
The `bzip2` and `lzma-rust2` dependencies stay: dropping them drops
`.tar.bz2` and `.tar.xz` flake inputs, a behaviour change. `src/` went from
50,315 to 49,397 lines.

## 2026-09-19: creation-time speculation removed (`4e882f9`)

Helpers evaluated thunks at creation time, behind `IETSP_SPEC`, off by
default. nixos-desktop, 15 runs: 1.401 s off, 1.544 s at `IETSP_SPEC=1`,
1.405 s at `=3`; user time 2.58 s against 2.98 s. It burned CPU on work
nobody demanded, and with no collector the results stayed resident. It
also needed a third deque lane, a backlog cap and a creation budget.

## 2026-09-18: build scheduling levers over the worker protocol (`6b93209`, removed in `38f4096`)

Six flags on `ietsp build`: a pool of N connections, upload batches of B
objects on M connections, per-derivation or K-at-a-time
`BuildPathsWithResults`, submitting while the upload runs, critical-path
order, and skipping substitutable paths. Measured on synthetic DAGs
(`bench/graph.nix`: `g8x6`, `g32x4`, `chain30`), hello and a
nixos-desktop upload, over a latency shim (0 and 20 ms), a unix socket and
a real host 0.11 s away. None paid. Per-derivation ops were 7.3x slower on
`g8x6` and 10x on `g32x4`: each op builds a daemon `Worker` and the
connection is serial. Upload batches of 256 added 0.25 s and of 32 added
0.4 s; a second uploader added 0.3 s and 9 % daemon CPU. The daemon
registers paths one SQLite transaction at a time (0.3-0.5 ms per path),
and no client-side cut changes that. Extra connections paid only as
extra `max-jobs` (g32x4, -20 %), which `SetOptions` already sets. The
five-op flow stayed; the flags, the shim and `scripts/sched-bench.nu`
went.

## 2026-09-18: single-thread ideas (`bench/ab.nu`)

Keep rule: wall better by more than 2 % on desktop w1 or outpaths w1, no
regression above 1 % elsewhere. Kept: the compiled-IR cache
(docs/memory.md). Rejected: transient frames on a fiber-local stack (20 M
of 59 M frames proved uncaptured, 320 MB less RSS on outpaths, but -1.7 %
wall there and +2.5 % on desktop w1); an inline cache on `Select` (82 %
hits on desktop, yet +2.2 % to +5.8 % wall: a miss stores into a shared IR
node); one escaped ATerm per derivation (user time -1 to -3 %, wall
-1.2 % on outpaths, under the bar; the derivation path is only ~6 % of a
run).

## 2026-09-18: memory, assessed and not done

From the outpaths census after the four packing changes (arena 7.1 GB):
attr positions in a side table (16-byte `Attr` pads back to 16 without a
struct-of-arrays rewrite); compiling `hackage-packages.nix` lazily (~15 MB,
but the AST stays alive); flattening `let` frames (under 56 MB); a
24-byte string header (43 MB); one shared closure for lambdas that capture
nothing (16 MB); one shared empty attrset (22 MB). Scoped release without
a collector was also ruled out: failed `tryEval` scopes held 1.28 MB and
none was free of escapes; per-unit scopes in eval-jobs are 0.9 % of the
arena, the rest is the package set the next unit reuses. Only a collector
or a process restart bounds a long eval-jobs run.

## 2026-09-18: gRPC builder protocol and amber object model removed (`a48eb78`)

A builder daemon of our own (`ietsp-builderd`, `proto/builder.proto`) and
a content-defined-chunking object store (`src/amber`) were removed; the
`grpc://` transport that exists today is a client of nix-grpc-store, added
the same day (`89342fd`).

## 2026-09-17: proposal, an eval cache layer on the binary cache

Not built. The idea: publish "attribute A of source S evaluates to P" next
to the NARs, keyed by the source's narHash, the system and the attribute
path, signed with the cache key. Measured for `ci/eval/outpaths.nix`
(80k jobs): the index is 4.5 MB with zstd, 0.27 MB as a three-week delta;
a lookup competes with a 1.0 s ietsp evaluation of a NixOS system and a
0.03 s tracing-cache hit. First milestone would have been
`eval-jobs --publish DIR` and an `--eval-cache URL` consumer in `build`.

## 2026-09-16: parallel evaluation, second pass

Tried and dropped, each inside noise or worse: an arena prefault thread
(w16 +20 %), no stack trimming plus prefaulting new stacks, a lock-free
interner cache in front of the shards (`intern` self time doubled), AST
buffer reuse, coerce batches of 8, and glibc malloc tunables
(`arena_max=2` was far worse).

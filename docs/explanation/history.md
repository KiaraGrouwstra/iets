# What was tried and removed

This is the record of ideas that were built, measured and then taken out
again, newest first. The code and the full tables are in git at the commit
each entry names. The numbers that justify a *current* default live in the
page for that feature, not here.

A page of failed experiments is worth keeping for two reasons. It stops the
same idea being tried a third time, and it shows what the measuring bar
was.

## 2026-09-27: the machine settings file

iets read `/etc/iets/iets.toml` (or `$IETS_CONF_DIR/iets.toml`) below
the user file, and the NixOS module had an `iets.settings` option that
wrote it. This copied the shape of `/etc/nix/nix.conf`. The file is
removed, with the option and the variable.

iets is a client. The one setting that the file held in practice was
`builder`, with a `token-file` in the home directory of one user. Each
account on the host got that builder, and root read the token of that
user. The setting is the user's, so it is in the user's file,
`~/.config/iets/iets.toml`. The sources are now four: the user file,
`$IETS_CONFIG`, `--iets-option` and the `IETS_*` variables.

An existing `/etc/iets/iets.toml` is not read and is not an error.

## 2026-09-27: the lints that read only the syntax

The registry had 41 lints. 18 of them were reports of the parser and of
the compiler about how code looks: `with`, `with-non-literal`,
`with-nested`, `shadow-builtin`, `shadow-binding`, `unused-binding`,
`unused-formal`, `rec-unused`, `legacy-let`, `deprecated-builtin`,
`overrides`, `unsafe-builtin`, `current-system`, `if-bool-literal`,
`eq-bool`, `manual-inherit`, `empty-let` and `ellipsis-without-bind`. They
are removed, with the families `scope` and `style`.

The reason is the scope rule: iets reports what only an evaluator can
know, and it has no opinion about how code looks
(`docs/explanation/lint-scope.md`). Style is a decision of the project.
nixpkgs has its own style for each type of file. statix and deadnix read
the syntax already, so a copy of their rules in iets gave no new data.

The removal took 523 lines out of `src/`. The compiler does not record
the bindings and the uses of a frame any more. On nixos-desktop with one
worker, a run that compiles each file went from 12.201 G to 12.183 G
instructions. A run with a warm compiled-file cache did not change
(9.393 G).

In place of the lints, iets gives the data. `iets parse --json` gives
the syntax tree with the byte span of each node, and a finding is a
record with a span and an edit. A rule is an external program.

## 2026-09-27: the fiber template guard

`parallel::guard` was a debug-only check. It made a fingerprint of the
settings of the fiber template. After the run, it compared the fingerprint
with the settings of the main fiber. A setting that was written after the
clone made a test fail.

The guard found a late write only when a test did that write. It did nothing
in a release build. The three failures that it came from (`--dump-drvs`, the
IR cache, the search path) had the same cause: the evaluator had public
setting fields, and `api::configure` wrote them after construction.

`eval::EvalConfig` replaces the guard, `api::configure` and
`Run::configured`. The settings go to a builder, and the setting fields of
the evaluator are private. A late write does not compile
(docs/explanation/evaluation.md).

## 2026-09-26: `fast-build`, a second build command

`iets fast-build` and the classic name `nix-fast-build` were removed. It
was 920 lines of another tool's command line: twelve flags it existed to
refuse, its own meaning for `-f`, its own `--option` handling and its own
reject type, beside 510 lines of `iets build`. With them went the result
records behind it (`build/results.rs`: the JSON and JUnit result files,
`--stream-json-lines`, the Actions step summary), the stream that fed it
(`jobs/stream.rs`) and `--retries` in the dispatcher.

What it could do that `build` could not is now `build`'s: a set of
derivations is built whole, every root to its own verdict, and `--json`
prints the outcomes. That took about 90 lines. Skipping what is built
needed none: the store already does it.

The overlap of evaluation and building did not come along. It needed a
second pipeline: a dispatch thread, a batcher and an incremental
uploader. On this repository's 18 checks the whole tree evaluates in
0.8 s with the parallel walk and 1.1 s with `nix-build`'s sequential one,
against builds that take minutes; and the tracing cache, which the
streaming walk could not use, makes the second run 0.03 s.

## 2026-09-26: `iets.conf`

iets's own settings lived in `iets.conf`, in nix.conf's line syntax, while
the lints lived in a TOML `iets.toml`: two formats for one program. The
settings are TOML now, in an `iets.toml` per machine and per user
(`docs/reference/settings.md`). `iets.conf` is not read. Nothing checks for a
leftover one: the NixOS module writes the new file and removes the old one at
the same switch. `include` and `extra-<name>` went with the format.

## 2026-09-25: the ATerm buffer the derivation pipeline kept allocating

The same profile put 9.3 % of a warm run in glibc `malloc` and `free`
although the evaluator allocates from an arena, and 46 % of that arrived
through `RawVecInner::finish_grow`. It was the derivation pipeline: every
derivation is unparsed three times, each into a fresh 4 KiB `String` that
then grows, and inside each unparse every store path, hash and method name
was built as its own `String` first.

The ATerm now goes into one buffer per thread that keeps its capacity —
taken out of its slot while in use, because `hash_derivation_modulo`
recurses — and `print_store_path`, `print_method_algo`, `base16_encode_into`
and `Hash::write_string_with` append to it directly. `StorePath::new`,
`make_store_path`, `make_type` and the fixed-output payload size their
buffer once instead of growing it, and `make_type` stopped printing each
reference into a string of its own.

| warm, instructions | before | after | |
|---|---:|---:|---:|
| nixos-desktop, 1 worker | 9.74 G | 9.39 G | −3.6 % |
| nixos-desktop, 4 workers | 10.54 G | 10.19 G | −3.4 % |
| nixos-minimal, 1 worker | 7.51 G | 7.22 G | −3.9 % |
| nixos-minimal, 4 workers | 8.12 G | 7.84 G | −3.5 % |

drvPaths identical on both workloads and still equal to nix 2.34.8; the
drv fixtures still round-trip byte for byte. The allocator bucket went from
780 M instructions to 516 M and `realloc` fell out of the profile, so the
work went rather than moved. What is left of it is `BTreeSet<StorePath>`
clones and the `String` per store path the reference sets are made of.

## 2026-09-25: the union merge became a loser tree

The first profile of a warm-cache run put `AttrIter::next` at 7.1 %, second
only to `eval` itself and 2.3× the cost of a lookup. The multi-leaf merge
scanned every leaf head twice per attribute — once to pick the minimum, once
to advance it — so eight leaves cost 16 head checks. A census of one
nixos-desktop run found where that landed: 8.4 million merge steps, of which
5.1 million came from iterations over six, seven or eight leaves, a few
hundred iterations of 5,000 to 12,000 attributes each.

A loser tree over the heads replaced the scan: one root path per attribute,
three comparisons at eight leaves. Equal syms are ordered by leaf so that
the winner surfaces first and the duplicates behind it are dropped as they
come. `Arena::flat` also merged twice, once through `len()` to size the copy
and once to fill it — 1.6 million of those 8.4 million steps; it now sizes
the copy at the sum of the leaf lengths, fills it in one pass and gives the
tail back to the bump pointer.

| warm, instructions | before | after | |
|---|---:|---:|---:|
| nixos-desktop, 1 worker | 10.03 G | 9.74 G | −2.9 % |
| nixos-desktop, 4 workers | 10.86 G | 10.54 G | −3.0 % |
| nixos-minimal, 1 worker | 7.75 G | 7.51 G | −3.0 % |
| nixos-minimal, 4 workers | 8.40 G | 8.12 G | −3.2 % |

drvPaths identical on both workloads and still equal to nix 2.34.8. A fresh
profile confirms the bucket shrank rather than moved: `AttrIter::next` 7.05 %
→ 4.49 % of a total that is itself 2.9 % smaller, 707 M instructions → 437 M,
and `Arena::flat` fell out of the 0.4 % list.

Two things the key encoding has to respect, both found by a randomised test
over every leaf count: a real head must never tie with the sentinel that
fills the tree while it is built (so keys start at 1, not 0), and the
tie-break has to prefer the *last* leaf or `//` returns the wrong side.

## 2026-09-25: the IR cache only the main fiber had

`parallel::try_run` clones the evaluator as the fiber template, and
`api::evaluate` installed the IR cache after that clone, inside the main
fiber. Every helper fiber therefore ran without one: on nixos-desktop with a
warm cache it loaded 2804 entries at one worker and 60 at four, and each
helper parsed and compiled its own imports again.

| nixos-desktop, warm, instructions | before | after |
|---|---:|---:|
| 1 worker | 10.03 G | 10.03 G |
| 4 workers | 13.05 G | 10.88 G |

−16.6 % at four workers, wall 0.59 s → 0.47 s, drvPaths identical. The
parallel overhead that was quoted as +30 % is +8 %; the rest was this. A cold
four-worker run also wrote the cache it was meant to fill: 62 entries before,
2807 after. The search path had the same shape, so `<nixpkgs>` on a helper
fiber resolved against an empty `__nixPath`. The fix installed both before
`try_run`, and `api/run.rs` holds the regression test: the fiber
template carries the configuration, and a warm run reads one entry per file
whatever the worker count.

## 2026-09-20: `BuildPaths` and the non-farm gRPC path

nix-grpc-store dropped `BuildPaths` and the `farm` flag in its store info
reply: every node now builds through its scheduler, so a standalone daemon
schedules onto itself. iets followed — one schedule stream per batch, a
want per derivation, one build call to the assigned worker. The client-side
salting that worked around an unavailable worker went with it; a bounced
build goes back to the scheduler instead. Before this change, every build
against the farm failed with an empty `UNIMPLEMENTED`.

## 2026-09-19: the second slimming pass

The last build flow outside the dispatcher went. `nix-instantiate`,
`nix-shell` and `print-dev-env` had kept a client flow of their own —
392 lines with its own timings — because folding them into the dispatcher
risked classic stdout. They now drive the worker backend through the shared
upload, and the classic byte comparison stayed at 59 of 59.

Cruft that went with it: a compile cache committed under `bench/` (5,287
files, 88 MB, regenerated by a script), one machine's benchmark results, a
workload generator and two workloads nothing ran, a demo script, and the
test fixtures of the removed object store. Kept on purpose: the parse cases
of the vendored Nix language suite, which no script runs yet, because
`iets parse` dumps its own AST format and not Nix's.

## 2026-09-19: the first slimming pass

The method is the interesting part. Every `pub` became `pub(crate)`, the
real external surface — the binary, the examples, the tests — was restored
one compiler round at a time, and rustc's dead-code lint then named 48
unreachable items.

Cut: those items (a second string-context parser, ten `nix.conf` getters
nothing read, provenance and parallel helpers), creation-time speculation
(below), and three wrappers that had one implementation each.

Kept on purpose: the test oracle for the spool reader; the `is_empty` twins
clippy requires; one heap kind whose removal shifts every discriminant.

Proposed and *not* cut, which is the useful half: one flag table for the six
argument parsers (about 120 lines of `match` against a 100-line dispatcher,
and the hand-written usage strings at risk), and one varint codec for the
three on-disk formats (about 40 net lines across three formats). Two
compression dependencies stay, because dropping them drops `.tar.bz2` and
`.tar.xz` flake inputs, which is a behaviour change and not a cleanup.

`src/` went from 50,315 to 49,397 lines.

## 2026-09-19: creation-time speculation

Helpers evaluated thunks at creation time, behind a flag, off by default.
Fifteen runs of nixos-desktop: 1.401 s off, 1.544 s at level 1, 1.405 s at
level 3; user time 2.58 s against 2.98 s.

It burned CPU on work nobody demanded, and with no collector the results
stayed resident. It also needed a third deque lane, a backlog cap and a
creation budget. Removed.

## 2026-09-18: build scheduling levers over the worker protocol

Six flags on `iets build`: a pool of N connections, upload batches of B
objects on M connections, per-derivation or K-at-a-time build operations,
submitting while the upload runs, critical-path order, and skipping
substitutable paths. Measured on synthetic graphs of several widths and
depths, on hello and on a nixos-desktop upload, over a latency shim at 0
and 20 ms, over a unix socket, and against a real host 0.11 s away.

**None paid.** Per-derivation operations were 7.3× slower on one graph and
10× on another, because each one builds a daemon worker and the connection
is serial. Upload batches of 256 added 0.25 s and of 32 added 0.4 s. A
second uploader added 0.3 s and 9 % daemon CPU.

The reason is one line: the daemon registers paths one SQLite transaction
at a time, at 0.3-0.5 ms per path, and **no client-side cut changes that.**
Extra connections paid only as extra `max-jobs`, which the existing options
message already sets. The five-operation flow stayed; the flags, the shim
and the benchmark script went.

## 2026-09-18: single-thread ideas

The keep rule: wall time better by more than 2 % on one of the two
one-worker workloads, with no regression above 1 % elsewhere.

Kept: the compiled-IR cache.

Rejected:

- **Transient frames on a fiber-local stack.** 20 M of 59 M frames proved
  uncaptured and it saved 320 MB of RSS on outpaths — but −1.7 % wall there
  and +2.5 % on desktop.
- **An inline cache on attribute selection.** 82 % hits on desktop, and yet
  +2.2 % to +5.8 % wall, because a miss stores into a shared IR node.
- **One escaped ATerm per derivation.** User time −1 to −3 %, wall −1.2 % on
  outpaths: under the bar, because the derivation path is only about 6 % of
  a run.

## 2026-09-18: memory ideas that were priced and not built

From the outpaths census after the four packing changes, with the arena at
7.1 GB: attribute positions in a side table (a 16-byte attribute pads back
to 16 without a struct-of-arrays rewrite), compiling `hackage-packages.nix`
lazily (~15 MB, but the AST stays alive), flattening `let` frames (under
56 MB), a 24-byte string header (43 MB), one shared closure for lambdas
that capture nothing (16 MB), one shared empty attribute set (22 MB).

Scoped release without a collector was ruled out too: failed `tryEval`
scopes held 1.28 MB and not one was free of escapes, and per-unit scopes in
eval-jobs are 0.9 % of the arena — the rest is the package set the next unit
reuses. Only a collector or a process restart bounds a long run.

## 2026-09-18: a builder daemon of our own

`iets-builderd` with its own protocol, and a content-defined-chunking
object store, were both removed. The `grpc://` transport that exists today
is a *client* of nix-grpc-store, added the same day. That is the point:
somebody else's daemon, and iets is a client of it.

## 2026-09-17: an eval cache layer on the binary cache

Proposed, measured, not built. The idea: publish "attribute A of source S
evaluates to P" next to the NARs, keyed by the source's NAR hash, the
system and the attribute path, signed with the cache key.

Measured for the 80,000 jobs of the outpaths workload: the index is 4.5 MB
with zstd, and 0.27 MB as a three-week delta. Against that, a lookup
competes with a 1.0 s local evaluation of a NixOS system and a 0.03 s
tracing-cache hit. The first milestone would have been a publisher in
`eval-jobs` and a consumer flag in `build`.

## 2026-09-16: parallel evaluation, the second pass

Tried and dropped, each inside the noise or worse: an arena prefault thread
(+20 % at sixteen workers), no stack trimming plus prefaulting new stacks, a
lock-free interner cache in front of the shards (which doubled the
interner's own self time), AST buffer reuse, coercion batches of eight, and
glibc malloc tunables — restricting the arena count was far worse.

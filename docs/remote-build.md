# Remote builds

`ietsp build` evaluates in memory, computes the closure of the root
derivations, and ships what the store lacks. Two transports: the Nix worker
protocol (`src/libstore/worker-protocol.hh`, Nix 2.34.8) to any `nix-daemon`,
and `grpc://` to a nix-grpc-store service such as Numtide's build farm. The
client owns no local store: nothing on either path reads `/nix/store` unless
an input drv came from there. What more scheduling over the worker protocol
would buy, measured and rejected: docs/history.md.

## Why not `nix-build`

The worker protocol is a serial RPC stream designed for a local daemon.
`nix-instantiate` writes every source and every drv into the local store
first (`IsValidPath` then `AddToStore` per path, `AddTempRoot` per drv), and
the build hook copies the closure to the remote builder per built drv. For
hello (616 drvs, 160 sources) that is about 1,700 daemon round trips before
the build starts. ietsp sends the same closure in one op.

## Client flow

`ietsp build [--builder URL] [--timings] [--no-build] [--dry-run [--json]] [--fetch-to DIR] [--out NAME]...
[-I prefix=path]... [--arg name expr] [--argstr name str] [-A attr]
(-E EXPR | FILE)` evaluates like `ietsp eval`, forces the result to
derivations (an attrset with `drvPath`, a string with derivation context,
or a list of those; `!out!drv` context also names the wanted output), and
builds each root.

- Every derivation the evaluation created is in `Evaluator::drv.drvs`
  (`DrvInfo { outputs, references, aterm }`); `derivationStrict` keeps the
  ATerm it hashed, so the plan never re-serializes a drv. An input drv that
  is not in the table came from a pre-existing `/nix/store/*.drv` reached
  through string context or `import`; the plan reads that file and
  `--timings` reports the count as `read from /nix/store`.
- Every source the evaluation added has an origin in `Evaluator::known`
  (`store::source::SourceOrigin`, all of them via `known.origins()`): a
  `SrcPath` plus the NAR hash and size. `./path` and `builtins.path` record
  the original local tree plus the set of entries a `filterSource` filter
  accepted, so the client re-dumps the same NAR; a fetched input records its
  input-store entry; `builtins.toFile` records the contents in memory and
  the references.
  After hashing, the client checks that the NAR still yields the planned
  store path and fails if the tree changed since evaluation.
- Sources with no recorded origin (under a drv read from the store, or a
  path pinned with `builtins.storePath`) go to a batch locator: one `nix
  path-info --json` call. A path the local store has as a recursive sha256
  source or a text file is read from `/nix/store`. Anything else, an
  input-addressed build output or a path that is not valid locally, is left
  to the daemon: Nix treats such an `inputSrcs` entry the same way, valid or
  substitutable on the builder.

`plan.rs`: `plan(roots, Inputs { drvs, sources })` walks the drv closure
once, iteratively, with a visited set (O(n)), emits drvs in post-order and
the source paths in registration order, and keeps the eval-time origin of
each source. It reads and hashes nothing: the daemon is asked first, and
`Plan::resolve(missing, locate)` then locates and hashes only the sources
the daemon lacks, on a thread pool. A `toFile` text is listed after the
sources it references, so the daemon sees every reference before the path
that needs it.

Legacy mode: `ietsp build [--source STOREPATH=LOCAL]... DRV` where `DRV` is
a `.drv` store path or a copy named `<hash>-<name>.drv`. The root comes from
that file, its closure from `/nix/store`, and `--source` names the original
local tree of an input source. Same planner, empty eval tables.

## Transport

`--builder daemon | unix:///path | ssh-ng://[user@]host[?remote-program=CMD] |
grpc://host:port[?…]`. Without the flag the URL is the `builder` setting in
`ietsp.conf`, and nix.conf's `store` when that is unset (`docs/reference/settings.md`).
`grpc://` is its own protocol and has its own section
below; the rest of this section is the worker protocol.
`daemon` is `/nix/var/nix/daemon-socket/socket`; `ssh-ng://` runs `ssh host
nix-daemon --stdio` and uses its stdio, as `ssh-store.cc` does, and reads
`NIX_SSHOPTS`. Code: `src/remote/worker/` (`wire.rs` framing and the
`FramedSink`, and `write_object`, one `AddMultipleToStore` entry; `mod.rs`
transport, handshake, the `STDERR_*` loop, the ops; `src/remote/event.rs`
what the daemon reports). The flow below is `build::Dispatcher` over the
worker backend (`build::backend::Worker`) with `build::upload::PlanUpload`;
see "The dispatcher".
No Nix library, no store; the protocol is written from the C++ source.

Handshake as `initConnection`: `WORKER_MAGIC_1`, our version 1.38, the
daemon's version, a feature-set exchange at >= 1.38 (we offer none), the
obsolete CPU-affinity and reserve-space words, then the daemon's `nix_version`
and its trust flag, then one `STDERR_LAST`. Daemons below 1.35 are refused.
`SetOptions` sends what `RemoteStore::setOptions` sends: keep-failed 0,
keep-going 0, try-fallback 0, verbosity `lvlInfo`, max-jobs = the CPU count
(`max-jobs = auto`; 0 would forbid local builds), max-silent-time 0,
build verbosity `lvlVomit` (so builder output arrives as
`resBuildLogLine` results attributed to their build activity, not as plain
log lines), cores 0, substitutes on, and an empty override map.

One op per phase:

1. `QueryValidPaths` on every planned path (sources, text paths, drvs) in one
   call, substitute flag off. Then, off the wire, `Plan::resolve` on the
   missing sources: eval-time origins and `--source` overrides first, one
   `nix path-info --json` batch for the rest.
   (`Connection::substitute_paths` is the same op with the flag on, which asks
   the daemon to substitute before it answers. No build phase uses it; the
   flake fetcher does, on the eval store, for a locked input that is missing
   from it: `docs/flakes.md`.)
2. `BuildPathsWithResults` on the missing sources the client cannot produce
   (plain store paths, the `DerivedPath::Opaque` case that the daemon turns
   into a substitution goal), before any upload: `registerValidPaths`
   (local-store.cc) rejects a path whose reference is not valid, so the
   daemon must hold these before the drvs that name them. Skipped when there
   are none. A failed substitution ends the run with the daemon's message.
   This is what `nix build` does through `copyPaths` plus `buildPaths` for an
   `inputSrcs` entry it does not own.
3. `AddMultipleToStore` of the missing ones in one framed stream, in plan
   order (sources and text paths first, then each drv after its inputs). Each
   entry is a `ValidPathInfo` in the 1.16 layout the daemon fixes for this op
   followed by the NAR: no deriver, NAR hash and size from our own dump,
   registration time 0, `ultimate = false`, no signatures, and the content
   address the daemon recomputes and checks: `fixed:r:sha256:<nar hash>` for
   a source tree, `text:sha256:<flat hash>` plus references for a
   `builtins.toFile` path, and `text:sha256:<sha256(aterm)>` with
   references = inputSrcs + inputDrvs for a `.drv` (a drv is a text path;
   `writeDerivation` registers it the same way). A CA path is accepted from
   an untrusted client because it needs no signature.
4. `BuildPathsWithResults` in normal mode on `<root drv>!<out1,out2>` (`!*`
   without `--out`), the legacy `DerivedPath` string that protocol >= 1.30
   reads, one entry per root in the same op. A failed status is that root's
   verdict. `--no-build` and `--dry-run` stop
   before this step; the substitution in step 2 still runs, or the drvs
   could not register.
5. `QueryDerivationOutputMap` per root that built: its output paths.
6. `NarFromPath` per wanted output with `--fetch-to DIR`, restored with
   `store::nar::restore_path`.

`--timings` prints one line per phase: `plan` (the closure walk), `connect`,
`query`, `resolve` (locate plus hash of the missing sources, with the count
read from `/nix/store` when there are any), `substitute` (only when paths
were left to the daemon), `add`, `build`, `outputs`, `fetch`, `worker`.

`STDERR_NEXT` lines are unattributed `Log` events; `STDERR_START_ACTIVITY` of
type `actBuild` (105) is `Started` and maps the activity id to its drv;
`STDERR_RESULT` of type `resBuildLogLine` (101) or `resPostBuildLogLine`
(107) is a `Log` attributed through that map; `STDERR_ERROR` is decoded as
`readError` (message plus traces). `--timings` counts worker ops and round
trips (ops plus the three handshake exchanges) and bytes written to the
socket. Under `internal-json` or with a `json-log-path` every activity
message is also forwarded to the log (`docs/reference/errors.md`).

## The dispatcher

Every ietsp command that realises a derivation goes through
`crate::build::Dispatcher`, over every transport: `ietsp build`, classic
`nix-build` and `nix build`, and `fast-build`. `nix-instantiate`,
`nix-shell` and `print-dev-env` use the same two halves without the
renderer: `build::upload::PlanUpload` over `build::backend::Worker`.

One dispatcher, two ways of feeding it:

- `build::upload::PlanUpload` ships a `remote::plan::Plan` computed once,
  for the commands that know every root before they build anything.
- `build::upload::StreamUpload` is incremental, for the eval-jobs walk of
  `fast-build` (docs/fast-build.md). Per batch it walks only the part of the
  closure it has not registered yet, then one `QueryValidPaths` and one
  `AddMultipleToStore`. It duplicates `remote::plan::plan`'s walk on
  purpose: `plan` cannot prune at a derivation already known valid, and
  re-planning a whole closure per batch is quadratic on a large `checks`
  set.

The dispatcher owns the batching, the provenance, the event stream and the
records; a `build::backend::Backend` owns the wire. Two implementations:

| | worker protocol (`daemon`, `unix://`, `ssh-ng://`) | `grpc://` |
|---|---|---|
| a ready set | one `BuildPathsWithResults` | a `Want` on the `Schedule` stream and one `BuildDerivation` per derivation of the set's closure, `max-builds` in flight |
| build parallelism | the daemon's, `-j` through `SetOptions` | the client's, in flight, placed by the scheduler |
| log lines | attributed by the daemon's activity ids | by the call, which knows its derivation |
| output paths | `QueryDerivationOutputMap` | statically, from the derivations |
| out-links | the local daemon, the closure copied back first | the local daemon substitutes from its caches; the rest by `FetchNars` |

Why one op per ready set on the worker protocol: the protocol is serial per
connection, and every `BuildPathsWithResults` constructs a daemon `Worker`
(build/entry-points.cc). A per-derivation op idles the daemon between
derivations: measured 7.3x slower on a 8-wide, 6-deep graph and 10x on a
32-wide, 4-deep one (docs/history.md). A farm has no such constraint: one
call per derivation is the only shape it offers, and fanning out drops the
barrier where every derivation of a batch waits for the slowest. The
fan-out is `build::fanout`, a dependency-ordered runner with a cap on jobs
in flight, unit-tested without a farm.

Stdout of the classic names is byte-identical to the real nix
(`scripts/classic-e2e.nu`); all progress goes to stderr, drawn by
`build::render` (docs/fast-build.md, "Rendering").

## `grpc://`: nix-grpc-store and Numtide's build farm

[nix-grpc-store](https://github.com/Mic92/nix-grpc-store) is a gRPC service
in front of a Nix store. Numtide's build farm is one: `grpc://farm.numtide.com:50051`,
a set of interchangeable workers behind an envoy balancer, with the outputs
published to `https://cache.numtide.com`. ietsp speaks it natively, so
`--builder grpc://…` and `--store grpc://…` need no nix, no plugin and no
local store. Code: `src/remote/grpc/` (`mod.rs` the URL, TLS, auth and the
retry policy; `drvwire.rs` the worker-protocol payloads the messages carry;
`build.rs` the calls; `sched.rs` the `Schedule` stream; `backend.rs` the
transport as one of the dispatcher's backends). The service definition is
vendored verbatim as
`proto/nix_remote.proto` (rev `4f6d239e`, the one numtide/org deploys) and
compiled by `build.rs`.

### URL

`grpc://host:port?k=v&…`, with nix-grpc-store's own parameter names and
defaults (`src/client/store.hh`):

| parameter | default | what it does |
|---|---|---|
| `system` | the `system` setting | `x-nix-system` on every call: which worker cluster the balancer routes to |
| `token-file` | none | file with a Keycloak/OIDC bearer token, re-read for every call. Needs TLS |
| `client-cert`, `client-key` | none | PEM client identity for mTLS. Both or neither |
| `ca-cert` | platform roots | PEM CA that verifies the server. The farm's balancer has a public ACME certificate, so this is rarely needed |
| `insecure` | `false` | plaintext instead of TLS. Local testing only |
| `connect-timeout` | 30 | seconds the first call waits for the service |
| `reschedule-retries` | 8 | how often a build a worker bounced goes back to the scheduler |
| `restart-grace` | 120 | seconds to wait for a scheduler that went away, or for a worker that can take an `Unplaceable` derivation |
| `max-builds` | 64 | concurrent `BuildDerivation` calls; nix.conf `max-jobs` does not apply, fast-build's `-j` lowers it |
| `nar-connections` | 4 | accepted for URL compatibility; this client fetches over its one channel |

### Auth and headers

`authorization: Bearer <token>` from `token-file`, read fresh for each call so
a rotating projected volume or a CI refresher works (`channel.hh:TokenFileCredentials`).
`x-nix-system` rides on every call, so an upload and the build that follows
land on the same cluster. A `BuildDerivation` adds `x-nix-worker` (the
address the scheduler assigned, which the balancer routes to) and
`x-nix-features` (the derivation's `requiredSystemFeatures`, comma separated),
and overrides `x-nix-system` with the derivation's own `platform`
(`build.cc:routingFor`).

### Flow

The `grpc://` backend, in order. `StoreInfo` once, for the trust flag. Then:

1. `QueryValidPaths` with every planned path's base name, in one call, then
   `Plan::resolve` on the missing sources — the same split the worker path
   makes.
2. The sources the client cannot produce go to the store: a worker
   substitutes an input when the build asks for it (daemon/main.cc:
   `gatherInputs`), so ietsp only checks that some substituter has them,
   which is what `QueryValidPaths` with `substitute` answers.
3. `AddMultipleToStore`: one client-streamed zstd stream whose decompressed
   bytes are exactly the worker-protocol `AddMultipleToStore` framing the
   `unix://` path already writes — the same `write_object`, count first, then
   each `ValidPathInfo` at 1.16 followed by its NAR. The flags travel alone in
   the first message; the data follows in 256 KiB chunks, as the C++ sink does,
   and the stream is ended (not flushed) once at the end.
4. Per derivation, at most `max-builds` in flight, a derivation only once
   every input derivation of it is done: a `Want` on the batch's one
   `Scheduler.Schedule` stream (`sched.rs`, build.cc:`GrpcStore::Run`), with
   the output keys, the inputs, the features and the platform. The scheduler
   answers `Cached` (the verdict is `AlreadyValid`), `Assigned` with a worker
   address and an `assign_id`, or `Unplaceable`, which is advisory: the
   `Want` stays queued and fails after `restart-grace`. On `Assigned`,
   `BuildDerivation` to that worker carries the `BasicDerivation` wire form
   (`derivation/aterm.cc:write`) at `protocol = 1 << 8 | 37`,
   `build_mode = 0` and the `assign_id`. `log_line` and `phase` become `Log`
   events; `BuildDerivationDone.result` is a `BuildResult` at 1.37 and
   becomes the derivation's verdict, a `Failed` event on failure.
   Derivations whose outputs `QueryValidPaths` already reports valid are
   skipped, so they cost no `Want`.
5. `--fetch-to DIR`: the farm never copies an output into a local store, so
   `FetchNars` streams the NARs back — one zstd stream across all paths,
   flushed per frame, each frame tagged with its `path_index` and the last one
   of a path marked `eof`. Each NAR is spooled beside its destination and
   restored with `store::nar::restore_path`.

The worker-protocol tunnel (`Connect`) is never opened. A farm endpoint
refuses it with `UNIMPLEMENTED`, which is why `nix` needs `--eval-store auto`
there; ietsp always evaluates in its own process, so it needs nothing.

### What a `write` token can and cannot do

`write` (the role an OIDC token usually gets) may request builds and import
**signed** paths. The daemon forces `CheckSigs` on for that role no matter how
trusted the proxy's uid is (`src/daemon/main.cc`). Everything ietsp uploads is
content-addressed — a source tree is `fixed:r:sha256:<nar hash>`, a
`builtins.toFile` text and a `.drv` are `text:sha256:…` — so the daemon
recomputes the address and accepts it without a signature. Sending an
*unsigned, input-addressed* path (an output built locally) needs the `trusted`
role, which is what a CI host's client certificate gets. ietsp never needs to:
it uploads only sources and derivations.

Outputs land in the farm's binary cache, not in the local store. `--fetch-to`
restores them into a directory. `nix build --store grpc://… --eval-store auto`
(what the `nix-farm` wrapper runs) needs the `result` symlink to resolve, so
the backend's out-link step (`grpc::build::register`) first asks the local
daemon to substitute the outputs (`QueryValidPaths` with `substitute`), which
fetches them from the farm's cache when `nix.conf` lists it. A worker's own
path infos carry only the worker's key, not the cache key, so this is the
path a farm takes. What the local daemon cannot substitute is read back with
`QueryPathInfos`, fetched with `FetchNars`, and registered through one
worker-protocol `AddMultipleToStore` with each path's signatures, which the
daemon checks as `nix copy --from` does. The local `nix.conf` must therefore
list and trust the cache (`substituters`, `trusted-public-keys`), or the user
must be trusted.

### Measured (`scripts/grpc-e2e.nu`, release build)

`nix-grpc-daemon` in plaintext standalone mode (it schedules onto itself) in
front of a scratch chroot store on this machine (`--proxy-socket` a
`nix-daemon` on that store, `--proxy-store local?root=DIR`), so these are the
transport's own figures, not a farm's.

| case | connect | QueryValidPaths | AddMultipleToStore | build | RPCs |
|---|---|---|---|---|---|
| custom drv + 1 source tree, `--fetch-to` | 0.009 s | 2 paths, 2 missing, 0.020 s | 2 paths, 721 B, 0.009 s | 0.63 s | 6 |
| hello cold, `--no-build` | 0.000 s | 776 paths, 776 missing, 0.006 s | 776 paths, 490 KB, 0.26 s | skipped | 3 |
| hello warm, `--no-build` | 0.000 s | 776 paths, 0 missing, 0.006 s | skipped | skipped | 2 |

The cold hello upload is 490 KB on the wire against 2.66 MB over the worker
protocol: the same bytes, zstd level 1 in one stream across all 776 paths.
`connect` is the `StoreInfo` round trip, which the daemon answers from a
cached backend connection.

Case 4 of the script runs classic `nix build --store grpc://…?insecure=1
--eval-store auto` and compares stdout with the real nix and its
nix-grpc-store plugin, which matched here. The script prefers the `nix` on
`PATH`, falls back to `/run/current-system/sw/bin/nix` (the dev shell's nix is
often a different libnixstore than the installed plugin was built against),
and, failing both, compares against the same store over the worker protocol
instead and says so. Case 5 checks the out-link path end to end: no local
substituter has the output, so it is fetched back with `FetchNars` and handed
to the local daemon, which refuses it for lacking a signature — correct,
because a scratch daemon builds unsigned outputs. On a real farm the local
daemon substitutes the output from the cache instead.

Case 6 runs `ietsp fast-build --store grpc://…` on `scripts/lib.nu`'s
`fastbuild-flake` and compares the whole result file — every record's type,
attribute, success and skipped flag — with the same flake built over the
worker protocol on the same store; case 7 checks that a multi-root
`ietsp build` over `grpc://` names every attribute on stderr.

Against `farm.numtide.com` with a Keycloak `write` token (2026-09-20): a
`runCommand` with 572 cached inputs went `Want`, `Assigned`, `BuildDerivation`
on bld1 in 2.6 s, 5 RPCs; a classic `nix-build` out-link then came from
cache.numtide.com through the local daemon. The fan-out's own scheduling —
the cap on builds in flight, the dependency order, a failed derivation
blocking its dependants, and the drv-tagged merge into one event stream — is
`build::fanout`, tested without a farm.

### Retries

`UNAVAILABLE` is what gRPC folds every connect failure into. Unary calls
retry with the `GrpcStore::retrying` policy: 500 ms doubling to 4 s, until
`connect-timeout` has passed. The `Schedule` stream reconnects when the
scheduler says `Restarting`, or goes away with `UNAVAILABLE`, `UNKNOWN` or
`INTERNAL`, for up to `restart-grace`, and sends every unanswered `Want`
again. A `BuildDerivation` that the worker bounces (`ABORTED`,
`FAILED_PRECONDITION`, `UNAVAILABLE`, `UNKNOWN`, `INTERNAL`) goes back to the
scheduler, up to `reschedule-retries` times (build.cc:`buildAssigned`).
Unlike build.cc, each ask after the first waits: 1 s doubling to 30 s. A
worker that crashed stays placeable until the scheduler notices, and every
bounce off it is instant.
`NOT_FOUND` means the worker has not got the `.drv` closure: ietsp uploads
that closure (`QueryValidPaths` then `AddMultipleToStore`, with the same
routing headers) and asks once more.

## Measured (`scripts/daemon-e2e.nu`, release build)

"Local daemon" is this machine's `nix-daemon` on its socket; the hello
closure is GC-rooted there, so every path is valid and a cold run is
impossible. "Scratch daemon" is `nix-daemon --store local?root=DIR` on its own
socket: an empty chroot store, so the cold numbers are real uploads and a
build is not attempted (`--no-build`). `unix://` and `daemon` are the same
code; `ssh-ng://` is the same code over a child's stdio, tested with an `ssh`
shim in `PATH` that runs `nix-daemon --stdio` locally (case 6), not against
a real host (no host with a reachable `nix-daemon` accepted the key).

| case | eval | plan | connect | QueryValidPaths | AddMultipleToStore | build (realise) | ops / round trips | bytes sent |
|---|---|---|---|---|---|---|---|---|
| custom drv + 1 source tree, deleted from the store, local daemon, `--fetch-to` | 0.005 s | 0.001 s | 0.077 s | 2 paths, 2 missing, 0.000 s | 2 paths, 2.4 KB, 0.001 s | 0.87 s | 6 / 9 | 2.9 KB |
| hello, local daemon (all valid) | 0.138 s | 0.006 s | 0.074 s | 776 paths, 0 missing, 0.008 s | skipped | 0.001 s | 4 / 7 | 62 KB |
| hello, scratch daemon, cold, `--no-build` | 0.159 s | 0.008 s | 0.020 s | 776 paths, 776 missing, 0.008 s | 776 paths, 2.6 MB, 0.247 s | skipped | 3 / 6 | 2.66 MB |
| hello, scratch daemon, warm, `--no-build` | 0.146 s | 0.007 s | 0.008 s | 776 paths, 0 missing, 0.011 s | skipped | skipped | 2 / 5 | 62 KB |
| nixos-desktop, scratch daemon, cold, `--no-build` | 1.48 s | 0.094 s | 0.015 s | 4,184 paths, all missing, 0.022 s | 4,184 paths, 40.4 MB, 1.89 s | skipped | 3 / 6 | 40.8 MB |
| nixos-desktop, scratch daemon, warm, `--no-build` | 1.65 s | 0.112 s | 0.055 s | 4,184 paths, 0 missing, 0.034 s | skipped | skipped | 2 / 5 | 333 KB |
| nixos-desktop, local daemon, `--no-build` | 1.21 s | 0.121 s | 0.081 s | 4,184 paths, 413 missing, 0.201 s | 413 paths, 23.2 MB, 1.67 s | skipped | 3 / 6 | 23.5 MB |

The 62 KB of a warm hello run is the `QueryValidPaths` request: 776 full
store paths as padded strings, and the 776 answers back. The custom drv's
0.87 s "build" is the sandbox build itself; the 0.87-1.1 s spread across
runs is the daemon, not the client. The 413 paths the local daemon lacked
for nixos-desktop are large nixpkgs subtrees that no local evaluation had
copied yet (the NixOS manual sources, python packages); the run registered
them, exactly as `nix-instantiate` would have.

Byte identity, checked by the script: after the upload, the `.drv` file in
the store is `cmp`-identical to the one `nix-instantiate` wrote before it
was deleted; `nix-store -q --hash`, `nix derivation show`, and `nix path-info
--json` (`ca`, `narHash`, `narSize`, `references`) match for the drv and the
source; in the scratch store the hello drv has the same hash as in the local
store and every input source has the same `ca`.

The upload is daemon-bound: about 0.5 ms per path for the sequential
`addToStore` (restore, hash, check, lock, one SQLite transaction). No
client-side cut of the upload changes that (docs/history.md).

## Gaps

- `ssh-ng://` is tested only through a local `ssh` shim; no real host has
  been tried. The shim path found one bug already (the child's stdin must
  close before waiting for it), so a real host is worth one run. No
  `--ssh-option` passthrough.
- The client does not poll stderr while streaming `AddMultipleToStore`, so an
  error in the middle of a large upload surfaces after the whole stream is
  sent; the daemon's `FramedSource` drains the rest, so nothing deadlocks.
- `ietsp build` sends `max-jobs` = the CPU count and has no flag for it,
  `keep-going`, or substituters.
- Protocol < 1.35 is refused, and no features are negotiated.
- Output paths are asked only for the roots; a dependency that built has no
  line of its own beyond the renderer's verdict.
- Connecting to the system daemon costs about 75 ms here against 10-20 ms
  on the scratch daemon; the system daemon forks a child per connection and
  loads two plugins (`plugin-files` in `nix.conf`), which is the likely
  cause but was not profiled.
- Flat fixed-output sources (`builtins.path { recursive = false; }`) have
  no recorded origin; the store locator does not send them, so the daemon
  must substitute them like an input-addressed output.
- `nix path-info --json` on this Nix prints a deprecation warning for the
  missing `--json-format`; it reaches stderr on every run that locates a
  source through the store.

`grpc://` adds:

- `Want.cp_hint_ms` is 0: a `Want` goes out only when a build slot is free,
  so the scheduler never orders a backlog by critical path.
- `nar-connections` is parsed and ignored: `FetchNars` runs over the one
  channel, so a large closure does not fan out over several connections the
  way the C++ `NarFetcher` does.
- A `BuildDerivation` cancelled by ^C is not cancelled server-side; the C++
  plugin calls `TryCancel` on interrupt.
- No `QueryMissing` use: the client decides what to upload from its own
  `QueryValidPaths` answer, and which derivations to build from
  a second `QueryValidPaths` over the output paths.
- Floating content-addressed and dynamic derivations are rejected, as they
  are by the C++ farm path.
- `x-nix-features` is the derivation's `requiredSystemFeatures` only.
  `DerivationOptions::getRequiredSystemFeatures` also adds `ca-derivations`
  for a content-addressed output and `recursive-nix` for `__recursive-nix`,
  neither of which ietsp supports anyway.

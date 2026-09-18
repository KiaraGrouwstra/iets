# Remote builds over the Nix worker protocol

`ietsp build` evaluates in memory, computes the closure of the root
derivations, and ships what the daemon lacks over the Nix worker protocol
(`src/libstore/worker-protocol.hh`, Nix 2.34.8). The client owns no local
store: nothing on this path reads `/nix/store` unless an input drv came from
there. What more scheduling over the protocol would buy, measured and
rejected: `docs/scheduling.md`.

## Why not `nix-build`

The worker protocol is a serial RPC stream designed for a local daemon.
`nix-instantiate` writes every source and every drv into the local store
first (`IsValidPath` then `AddToStore` per path, `AddTempRoot` per drv), and
the build hook copies the closure to the remote builder per built drv. For
hello (616 drvs, 160 sources) that is about 1,700 daemon round trips before
the build starts. ietsp sends the same closure in one op.

## Client flow

`ietsp build [--builder URL] [--timings] [--no-build] [--fetch-to DIR] [--out NAME]...
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
- Every source the evaluation added is in `Evaluator::sources`
  (`store::source::SourceOrigin`, accessor `source_origins()`): `./path`
  and `builtins.path` record the original local tree plus the set of
  entries a `filterSource` filter accepted, so the client re-dumps the same
  NAR; `builtins.toFile` records the contents and references in memory.
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

`--builder daemon | unix:///path | ssh-ng://[user@]host[?remote-program=CMD]`.
`daemon` is `/nix/var/nix/daemon-socket/socket`; `ssh-ng://` runs `ssh host
nix-daemon --stdio` and uses its stdio, as `ssh-store.cc` does, and reads
`NIX_SSHOPTS`. Code: `src/remote/worker/` (`wire.rs` framing and the
`FramedSink`; `mod.rs` transport, handshake, the `STDERR_*` loop, the ops;
`build.rs` the client flow; `src/remote/event.rs` what the flow reports).
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
   reads. A failure status becomes a `Failed` event. `--no-build` stops
   before this step; the substitution in step 2 still runs, or the drvs
   could not register.
5. `QueryDerivationOutputMap` on the root: `Built` events per output.
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
socket.

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
`addToStore` (restore, hash, check, lock, one SQLite transaction), see
`docs/scheduling.md`.

## Gaps

- `ssh-ng://` is tested only through a local `ssh` shim; no real host has
  been tried. The shim path found one bug already (the child's stdin must
  close before waiting for it), so a real host is worth one run. No
  `--ssh-option` passthrough.
- The client does not poll stderr while streaming `AddMultipleToStore`, so an
  error in the middle of a large upload surfaces after the whole stream is
  sent; the daemon's `FramedSource` drains the rest, so nothing deadlocks.
- `max-jobs` is the CPU count; there is no flag for it, `keep-going`, or
  substituters.
- Protocol < 1.35 is refused, and no features are negotiated.
- `QueryDerivationOutputMap` only on the root; no `Built` events for
  dependencies.
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

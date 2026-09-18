# Build scheduling over the worker protocol

> The levers below were measured, found not to pay, and removed on
> 2026-09-18 (commit bc64b0d); the tables stay as the record. `ietsp build`
> keeps the plain five-op flow. The code, the shim, the bench script and the
> raw results are in the history up to 6b93209.

What `ietsp build` could gain, over a plain `nix-daemon`, by cutting the
upload and the builds into more than one op and more than one connection.
Every lever was a flag on `ietsp build`, off by default; the numbers below
say which ones pay. `scripts/sched-bench.nu` ran the matrix.

Levers (`src/remote/worker/sched.rs`, removed):

| flag | what it changes |
|---|---|
| `--connections N` | a pool of N connections, one thread each; over ssh, N `ssh` processes (channels of one master with ControlMaster) |
| `--upload-connections M`, `--upload-batch B` | `AddMultipleToStore` ops of at most B objects on up to M connections at once |
| `--per-drv`, `--build-batch K` | `BuildPathsWithResults` per ready derivation (or per K ready ones; 0 = all ready) instead of one op on the root |
| `--overlap` | submit ready derivations while the upload is still running on the uploading connections |
| `--critical-path` | uploads and submissions ordered by longest path to the root (a drv weighs 1, a fixed-output drv and a source 0) instead of plan order |
| `--skip-substitutable` | do not upload paths the daemon can substitute when every referrer is skipped too |

The flow, before and after the study: one connection, `QueryValidPaths`, one
`AddMultipleToStore` of every missing object in topological order, one
`BuildPathsWithResults` on the root, `QueryDerivationOutputMap`. Five ops.

## Setup

Everything on one machine (16 cores) against Nix 2.34.8 (`nix develop`), the
Nix whose source is cited below. The scratch daemon is
`nix-daemon --store 'local?root=ROOT/store' --option builders '' --option
substituters '' --stdio`, so it builds locally in its own sandbox and never
forwards to the real daemon's gRPC builder. It runs under `ietsp-shim`
(`src/bin/ietsp-shim.rs`), a relay that holds every chunk in each direction
for `IETSP_SCHED_LATENCY_MS` (one round trip costs twice that; `pv -L` and
`socat` rate limits change bandwidth, not latency) and, when the daemon exits,
reads the daemon's own `utime+stime` from its zombie `/proc/<pid>/stat`: the
daemon's CPU, not the builders it reaped.

Three transports:

- `shim0`, `shim20`: `ssh-ng://shim` with an `ssh` shim in `PATH` that ignores
  host and command and runs the daemon on stdio here, 0 or 20 ms per
  direction. Nix's own ssh-ng store needs two things from that shim
  (libstore/ssh.cc): it passes `-oLocalCommand=echo started` and reads that
  line before it speaks the protocol (ssh.cc:115-116, 210-217), and it probes
  a master with `ssh -O check`. Real `ssh` does both by itself. Nix also
  SIGKILLs its ssh child at teardown (libutil/unix/processes.cc:Pid::kill), so
  the shim runs `ietsp-shim` as a child, not with `exec`, or the CPU line is lost.
- `unix`: `unix://ROOT/unix/sock`, a `socat UNIX-LISTEN:...,fork` listener that
  runs the same daemon per connection. `nix-daemon`'s own listener forks one
  child per connection too (src/nix/unix/daemon.cc:334), so the process model
  is the same; socat only lets the shim meter each daemon.
- `host`, `host-cm`: `ssh-ng://root@bld1.ztm.io?remote-program=/tmp/ietsp-sched/daemon-stdio`,
  a real host 0.11 s away per full ssh round trip (connect, auth, exec), with
  and without `NIX_SSHOPTS="-o ControlMaster=auto ..."`. `ietsp` reads
  `NIX_SSHOPTS` and the `remote-program` query parameter as Nix does.
  `ietsp-shim` is shipped with its loader and libraries under
  `/tmp/ietsp-sched/lib` and run through `ld-linux --library-path`, so the
  host's store is never touched. See "bld1 leg" for why it uploads only.

Cold: the scratch store is wiped (for hello: restored from a seed store, see
below) and `nix-store --init` creates the schema. Warm: the same command again.
Three runs each; the tables show medians. `cold s` is `sched: ... total`,
from before the connect to the root's outputs; `first build s` is the first
`Started` event (`STDERR_START_ACTIVITY` of type `actBuild`), from the same
origin; `upload s` is the `add:` line; `bytes sent` is every byte the client
wrote on all connections; `ops` and `conns` come from one cold run; `daemon
CPU s` is the sum over the daemon processes of that run.

## Workloads

- `g8x6`, `g32x4`, `chain30`: `bench/graph.nix { width, depth, cost }`, a
  layered DAG of `/bin/sh` derivations; each node reads its two inputs of the
  layer below and spins `cost` = 40000 shell iterations (about 100 ms, 150 ms
  with the sandbox), a root depends on the whole top layer. 49, 129 and 31
  derivations, no sources. Everything is absent from the scratch store, so
  the cold time is scheduling plus a 6-, 4- or 30-deep chain of ~150 ms
  builds.
- `hello`: `(import <nixpkgs> {}).hello.overrideAttrs (_: { name =
  "hello-ietsp-2.12.3"; })` from `-I nixpkgs=/nix/store/ah3pxz4x...-source`.
  Plain `hello-2.12.3` is on cache.nixos.org and the daemon substituted the
  output itself (781 paths, no build); the renamed one has the same inputs
  and no cached output, so its inputs substitute and it builds. One true
  cold run with `--option substituters https://cache.nixos.org` uploaded 776
  paths (2.6 MB) in 0.24 s, then substituted the inputs and built hello in
  33.9 s (total 34.2 s, daemon CPU 2.6 s, 424 MB store). For the matrix the
  61 input outputs (418 MB) were copied out of that store into
  `ROOT/hello-seed/store` with `nix copy --no-check-sigs`; a cold run
  restores it with `cp -a` (0.4 s) and the daemon keeps cache.nixos.org as
  substituter, so every run uploads the 762 missing closure paths (14 of the
  776 are sources the input outputs reference) and builds hello (~32 s).
- `nixos-desktop`: `bench/workloads/nixos-desktop.nix` (a NixOS toplevel)
  with `--no-build`: 3533 derivations and 651 sources, 4184 paths, 40 MB. The
  upload levers only.

Baselines per workload: `ietsp` (today's flow) and `nix`: `nix copy --to URL
--derivation DRV` of the drv closure, then `nix build --max-jobs 16 --store
URL --eval-store auto --no-link 'DRV^*'` through the same daemon
(`--max-jobs 16` because the client's `max-jobs` becomes the daemon
`Worker`'s limit for that connection, daemon.cc:240, and this machine's
nix.conf says 2). Both steps are timed; `upload s` is the copy.

## Results: local

What the tables say (medians of 3; the tables follow):

- **The root op is the right default.** On every graph and every transport
  today's flow (`ietsp`) is fastest or within noise, with one exception
  below. It is also the cheapest for the daemon: 0.38 s CPU on g8x6
  against 0.5-0.7 s for any batched variant, because every
  `BuildPathsWithResults` builds a `Worker`.
- **Per-derivation ops serialise what the daemon runs in parallel.**
  `--per-drv` (k = 1) is 7.3× slower on g8x6, 10× on g32x4, 1.4× on the
  chain. The batch size sweet spot is "every ready derivation" (k = 0,
  layer-synchronous), and even that costs one op per layer: +0.6 s on
  g8x6 (6 layers), +0.35 s on g32x4, +1.3 s on chain30 (31 ops). The
  per-op overhead is about 42 ms at 0 ms latency ((4.35 − 3.04) / 31 on
  the chain) and about 90 ms at 20 ms per direction: a `Worker` per op
  plus one round trip in which no build runs. k = 16 equals k = 0 when no
  layer is wider than 16; k = 4 is 2.3-3.2× slower than the root op.
- **Connections alone change nothing** (`conn2-batch-all`, `conn4-batch-all`
  equal `batch-all`): the ready set of a layer goes into one op on one
  connection. What more connections buy is more `Worker`s, each with its
  own `max-jobs`: on g32x4, whose layers are wider than the 16 slots of
  one worker, `overlap4` (4 connections, upload batches of 32, every
  ready derivation per op) is 1.51 s against 1.88 s (0 ms), 2.04 s
  against 2.29 s (20 ms), 1.42 s against 2.09 s (unix). The upload is
  0.05 s there, so it is not the overlap that pays but the 64 build slots.
  The same effect is available for free by sending a larger `max-jobs` in
  `SetOptions` (daemon.cc:240); not measured here.
- **`--critical-path` changes nothing on these graphs**: every ready set
  is a whole layer and every op takes the whole ready set, so order never
  matters. It can only matter when ready derivations exceed the slots and
  ops are smaller than the ready set, which is the case the previous point
  says not to create.
- **Time to first build** is 0.17-0.28 s for the root op (connect, query,
  upload, then the daemon's first goal) and 0.11-0.15 s with batched
  uploads and overlap. Nothing to win on these uploads; on hello (762
  paths) the first build starts at 0.44 s either way.
- **Latency.** 20 ms per direction adds 0.3 s to the root op: 8 round
  trips (3 for the handshake, `SetOptions`, `QueryValidPaths`,
  `AddMultipleToStore`, the build, `QueryDerivationOutputMap`). Every
  per-op lever adds one round trip per op on top: `per-drv` goes from
  7.3 s to 9.8 s on g8x6, `ub32` on nixos-desktop from 2.5 s to 9.9 s
  (133 ops). Plain Nix goes from 2.9 s to 5.2 s on g8x6 and from 7.3 s to
  13.1 s on g32x4: its client makes about one round trip per derivation
  (`-vvv` shows `QueryPathInfo`, op 26, per output path from the client's
  `queryMissing` before the build).
- **Plain Nix** is 1.1-1.3× slower than `ietsp` over the unix socket
  (`nix copy` 0.18 s plus `nix build` 1.1 s on g8x6) and 2.9-3.9× slower
  over ssh-ng at 0 ms: Nix's ssh-ng path spends 1.5-5 s more than its unix
  path on the same daemon before latency is added; `ietsp` pays 0.05 s
  for the shim process. Its daemon CPU is 1.5-2.7× the root op's.
- **Uploads (nixos-desktop, 4184 paths, 40 MB).** The daemon is the
  bottleneck: 2.1 s of daemon CPU for a 2.2 s upload, about 0.5 ms per
  path for the sequential `addToStore` (restore, hash, check, lock,
  transaction). Batches of 256 cost nothing at 0 ms and +0.8 s at 20 ms
  (16 more round trips); batches of 32 cost +0.1 s and +7.3 s. A second
  uploading connection makes it slower (2.67 s against 2.35 s) and costs
  20% more daemon CPU: the two `LocalStore`s contend on the SQLite lock
  and `big-lock`. `--skip-substitutable` skips nothing: none of the drvs
  and sources of hello or nixos-desktop is on cache.nixos.org, and it
  costs one op. `nix copy` takes 3.0 s for the same closure.
- **hello.** The upload is 0.28 s of a 33.9 s run; every upload lever is
  within noise of the root op. Per-derivation submission is one op after
  the validity walk (only hello itself is unbuilt once its inputs are in
  the store), so it equals the root op too. Before the walk was made to
  follow wanted outputs, per-derivation mode submitted 170 ops and made
  the daemon substitute the whole build-time closure (19 s of daemon
  CPU): the daemon's goals visit only what the requested outputs need.

### local shim, 0 ms per direction

**g8x6**

| config | cold s | first build s | warm s | upload s | build s | bytes sent | ops | conns | daemon CPU s |
|---|---|---|---|---|---|---|---|---|---|
| ietsp | 0.97 | 0.17 | 0.07 | 0.03 | 0.88 | 0.06 MB | 5 | 1 | 0.38 |
| nix | 2.86 | - | 0.3 | 0.18 | 2.65 | - | 0 | 1 | 0.6 |
| per-drv | 7.29 | 0.13 | 0.07 | 0.02 | 7.21 | 0.06 MB | 53 | 1 | 0.68 |
| batch4 | 2.29 | 0.16 | 0.07 | 0.02 | 2.19 | 0.06 MB | 17 | 1 | 0.51 |
| batch16 | 1.6 | 0.17 | 0.06 | 0.02 | 1.5 | 0.06 MB | 11 | 1 | 0.51 |
| batch-all | 1.59 | 0.15 | 0.07 | 0.01 | 1.51 | 0.06 MB | 11 | 1 | 0.5 |
| conn2-batch-all | 1.59 | 0.16 | 0.07 | 0.02 | 1.51 | 0.06 MB | 12 | 2 | 0.49 |
| conn4-batch-all | 1.59 | 0.17 | 0.07 | 0.02 | 1.49 | 0.06 MB | 14 | 4 | 0.6 |
| conn4-per-drv | 1.86 | 0.12 | 0.07 | 0.02 | 1.76 | 0.07 MB | 56 | 4 | 0.67 |
| overlap4 | 1.6 | 0.14 | 0.07 | 0.02 | 1.52 | 0.06 MB | 15 | 4 | 0.57 |
| critical | 1.59 | 0.16 | 0.07 | 0.03 | 1.51 | 0.06 MB | 11 | 1 | 0.48 |
| critical-conn4 | 1.57 | 0.13 | 0.07 | 0.02 | 1.51 | 0.06 MB | 14 | 4 | 0.52 |
| all | 1.54 | 0.11 | 0.07 | 0.02 | 1.48 | 0.06 MB | 14 | 4 | 0.5 |

**g32x4**

| config | cold s | first build s | warm s | upload s | build s | bytes sent | ops | conns | daemon CPU s |
|---|---|---|---|---|---|---|---|---|---|
| ietsp | 1.88 | 0.28 | 0.07 | 0.05 | 1.77 | 0.15 MB | 5 | 1 | 0.83 |
| nix | 7.33 | - | 0.33 | 0.22 | 7.12 | - | 0 | 1 | 2.23 |
| per-drv | 18.86 | 0.18 | 0.08 | 0.05 | 18.74 | 0.17 MB | 133 | 1 | 1.86 |
| batch4 | 6.08 | 0.18 | 0.06 | 0.04 | 5.97 | 0.17 MB | 37 | 1 | 1.34 |
| batch16 | 2.74 | 0.17 | 0.08 | 0.04 | 2.66 | 0.17 MB | 14 | 1 | 1 |
| batch-all | 2.22 | 0.21 | 0.07 | 0.04 | 2.12 | 0.17 MB | 9 | 1 | 0.95 |
| conn2-batch-all | 2.3 | 0.18 | 0.07 | 0.04 | 2.2 | 0.17 MB | 10 | 2 | 1.03 |
| conn4-batch-all | 2.2 | 0.19 | 0.07 | 0.04 | 2.09 | 0.17 MB | 12 | 4 | 1.06 |
| conn4-per-drv | 4.53 | 0.15 | 0.07 | 0.04 | 4.43 | 0.17 MB | 136 | 4 | 1.76 |
| overlap4 | 1.51 | 0.14 | 0.07 | 0.04 | 1.43 | 0.17 MB | 25 | 4 | 0.92 |
| critical | 2.28 | 0.21 | 0.07 | 0.05 | 2.15 | 0.17 MB | 9 | 1 | 1.04 |
| critical-conn4 | 2.27 | 0.18 | 0.06 | 0.03 | 2.2 | 0.17 MB | 12 | 4 | 1.09 |
| all | 2.27 | 0.18 | 0.07 | 0.05 | 2.15 | 0.17 MB | 12 | 4 | 1.14 |

**chain30**

| config | cold s | first build s | warm s | upload s | build s | bytes sent | ops | conns | daemon CPU s |
|---|---|---|---|---|---|---|---|---|---|
| ietsp | 3.04 | 0.16 | 0.07 | 0.02 | 2.95 | 0.03 MB | 5 | 1 | 0.31 |
| nix | 3.33 | - | 0.28 | 0.16 | 3.17 | - | 0 | 1 | 0.34 |
| per-drv | 4.35 | 0.16 | 0.07 | 0.02 | 4.26 | 0.04 MB | 35 | 1 | 0.43 |
| batch4 | 4.46 | 0.13 | 0.05 | 0.02 | 4.37 | 0.04 MB | 35 | 1 | 0.42 |
| batch16 | 4.48 | 0.14 | 0.07 | 0.02 | 4.4 | 0.04 MB | 35 | 1 | 0.41 |
| batch-all | 4.5 | 0.14 | 0.07 | 0.02 | 4.42 | 0.04 MB | 35 | 1 | 0.42 |
| conn2-batch-all | 4.58 | 0.15 | 0.07 | 0.02 | 4.49 | 0.04 MB | 36 | 2 | 0.46 |
| conn4-batch-all | 4.5 | 0.13 | 0.07 | 0.01 | 4.43 | 0.04 MB | 38 | 4 | 0.49 |
| conn4-per-drv | 4.45 | 0.13 | 0.07 | 0.01 | 4.36 | 0.04 MB | 38 | 4 | 0.5 |
| overlap4 | 4.38 | 0.12 | 0.07 | 0.01 | 4.31 | 0.04 MB | 38 | 4 | 0.53 |
| critical | 4.35 | 0.14 | 0.07 | 0.01 | 4.28 | 0.04 MB | 35 | 1 | 0.42 |
| critical-conn4 | 4.53 | 0.16 | 0.07 | 0.02 | 4.44 | 0.04 MB | 38 | 4 | 0.53 |
| all | 4.37 | 0.14 | 0.06 | 0.02 | 4.29 | 0.04 MB | 38 | 4 | 0.53 |

**hello**

| config | cold s | first build s | warm s | upload s | build s | bytes sent | ops | conns | daemon CPU s |
|---|---|---|---|---|---|---|---|---|---|
| ietsp | 33.85 | 0.44 | 0.11 | 0.28 | 33.5 | 2.62 MB | 5 | 1 | 0.45 |
| nix | 34.37 | - | 0.56 | 0.52 | 33.85 | - | 0 | 1 | 0.55 |
| skip-sub | 33.94 | 0.45 | 0.12 | 0.26 | 33.59 | 2.69 MB | 7 | 1 | 0.46 |
| ub256 | 33.79 | 0.4 | 0.12 | 0.27 | 33.43 | 2.62 MB | 7 | 1 | 0.44 |
| ub256-2c | 33.85 | 0.56 | 0.12 | 0.41 | 33.35 | 2.62 MB | 10 | 2 | 0.51 |
| batch-all | 33.29 | 0.45 | 0.12 | 0.27 | 32.92 | 2.68 MB | 5 | 1 | 0.44 |
| conn4-batch-all | 33.05 | 0.44 | 0.13 | 0.28 | 32.68 | 2.68 MB | 8 | 4 | 0.56 |
| overlap4 | 32.94 | 0.45 | 0.1 | 0.27 | 32.6 | 2.68 MB | 31 | 4 | 0.55 |
| all | 33.51 | 0.44 | 0.12 | 0.27 | 33.13 | 2.68 MB | 10 | 4 | 0.57 |

**nixos-desktop**

| config | cold s | first build s | warm s | upload s | build s | bytes sent | ops | conns | daemon CPU s |
|---|---|---|---|---|---|---|---|---|---|
| ietsp | 2.35 | - | 0.16 | 2.18 | 0 | 40.76 MB | 3 | 1 | 2.11 |
| nix | 2.97 | - | 0.66 | 2.97 | 0 | - | 0 | 1 | 2.08 |
| skip-sub | 2.29 | - | 0.17 | 2.1 | 0 | 41.13 MB | 4 | 1 | 2.08 |
| ub256 | 2.36 | - | 0.18 | 2.19 | 0 | 40.76 MB | 19 | 1 | 2.12 |
| ub32 | 2.48 | - | 0.17 | 2.33 | 0 | 40.77 MB | 133 | 1 | 2.18 |
| ub256-2c | 2.67 | - | 0.18 | 2.51 | 0 | 40.77 MB | 23 | 2 | 2.53 |

### local shim, 20 ms per direction

**g8x6**

| config | cold s | first build s | warm s | upload s | build s | bytes sent | ops | conns | daemon CPU s |
|---|---|---|---|---|---|---|---|---|---|
| ietsp | 1.29 | 0.47 | 0.34 | 0.08 | 0.93 | 0.06 MB | 5 | 1 | 0.41 |
| nix | 5.23 | - | 0.72 | 0.38 | 4.86 | - | 0 | 1 | 0.6 |
| per-drv | 9.8 | 0.42 | 0.29 | 0.07 | 9.47 | 0.06 MB | 53 | 1 | 0.71 |
| batch4 | 3.37 | 0.44 | 0.29 | 0.07 | 3.01 | 0.06 MB | 17 | 1 | 0.58 |
| batch16 | 2.2 | 0.42 | 0.29 | 0.07 | 1.86 | 0.06 MB | 11 | 1 | 0.5 |
| batch-all | 2.25 | 0.42 | 0.3 | 0.07 | 1.88 | 0.06 MB | 11 | 1 | 0.51 |
| conn2-batch-all | 2.23 | 0.44 | 0.29 | 0.07 | 1.89 | 0.06 MB | 12 | 2 | 0.57 |
| conn4-batch-all | 2.29 | 0.44 | 0.3 | 0.07 | 1.93 | 0.06 MB | 14 | 4 | 0.65 |
| conn4-per-drv | 2.76 | 0.43 | 0.29 | 0.08 | 2.39 | 0.07 MB | 56 | 4 | 0.81 |
| overlap4 | 2.29 | 0.43 | 0.3 | 0.12 | 1.94 | 0.06 MB | 15 | 4 | 0.6 |
| critical | 2.22 | 0.44 | 0.3 | 0.07 | 1.87 | 0.06 MB | 11 | 1 | 0.54 |
| critical-conn4 | 2.29 | 0.44 | 0.29 | 0.07 | 1.93 | 0.06 MB | 14 | 4 | 0.66 |
| all | 2.23 | 0.44 | 0.29 | 0.07 | 1.86 | 0.06 MB | 14 | 4 | 0.64 |

**g32x4**

| config | cold s | first build s | warm s | upload s | build s | bytes sent | ops | conns | daemon CPU s |
|---|---|---|---|---|---|---|---|---|---|
| ietsp | 2.29 | 0.51 | 0.34 | 0.1 | 1.9 | 0.15 MB | 5 | 1 | 0.91 |
| nix | 13.12 | - | 0.77 | 0.4 | 12.72 | - | 0 | 1 | 2.29 |
| per-drv | 25.04 | 0.45 | 0.3 | 0.1 | 24.65 | 0.17 MB | 133 | 1 | 1.93 |
| batch4 | 7.96 | 0.47 | 0.3 | 0.1 | 7.56 | 0.17 MB | 37 | 1 | 1.44 |
| batch16 | 3.71 | 0.48 | 0.3 | 0.1 | 3.32 | 0.17 MB | 14 | 1 | 1.25 |
| batch-all | 2.9 | 0.48 | 0.3 | 0.11 | 2.5 | 0.17 MB | 9 | 1 | 1.11 |
| conn2-batch-all | 2.95 | 0.5 | 0.3 | 0.1 | 2.57 | 0.17 MB | 10 | 2 | 1.27 |
| conn4-batch-all | 3.03 | 0.5 | 0.29 | 0.1 | 2.63 | 0.17 MB | 12 | 4 | 1.34 |
| conn4-per-drv | 6.28 | 0.46 | 0.28 | 0.1 | 5.87 | 0.17 MB | 136 | 4 | 1.85 |
| overlap4 | 2.04 | 0.44 | 0.3 | 0.28 | 1.68 | 0.17 MB | 27 | 4 | 1.05 |
| critical | 2.85 | 0.47 | 0.3 | 0.1 | 2.48 | 0.17 MB | 9 | 1 | 1.09 |
| critical-conn4 | 2.86 | 0.48 | 0.3 | 0.11 | 2.46 | 0.17 MB | 12 | 4 | 1.26 |
| all | 2.9 | 0.48 | 0.3 | 0.1 | 2.52 | 0.17 MB | 12 | 4 | 1.21 |

**chain30**

| config | cold s | first build s | warm s | upload s | build s | bytes sent | ops | conns | daemon CPU s |
|---|---|---|---|---|---|---|---|---|---|
| ietsp | 3.3 | 0.42 | 0.34 | 0.06 | 2.95 | 0.03 MB | 5 | 1 | 0.3 |
| nix | 4.99 | - | 0.74 | 0.33 | 4.68 | - | 0 | 1 | 0.37 |
| per-drv | 6.14 | 0.42 | 0.29 | 0.06 | 5.8 | 0.04 MB | 35 | 1 | 0.43 |
| batch4 | 6.06 | 0.41 | 0.29 | 0.06 | 5.71 | 0.04 MB | 35 | 1 | 0.43 |
| batch16 | 6.24 | 0.42 | 0.29 | 0.06 | 5.88 | 0.04 MB | 35 | 1 | 0.45 |
| batch-all | 6.27 | 0.42 | 0.29 | 0.06 | 5.92 | 0.04 MB | 35 | 1 | 0.44 |
| conn2-batch-all | 6.16 | 0.42 | 0.3 | 0.06 | 5.81 | 0.04 MB | 36 | 2 | 0.47 |
| conn4-batch-all | 6.22 | 0.42 | 0.3 | 0.06 | 5.86 | 0.04 MB | 38 | 4 | 0.57 |
| conn4-per-drv | 6.18 | 0.42 | 0.29 | 0.07 | 5.83 | 0.04 MB | 38 | 4 | 0.56 |
| overlap4 | 6.05 | 0.4 | 0.29 | 0.06 | 5.72 | 0.04 MB | 38 | 4 | 0.54 |
| critical | 6.16 | 0.43 | 0.29 | 0.06 | 5.8 | 0.04 MB | 35 | 1 | 0.44 |
| critical-conn4 | 6.05 | 0.42 | 0.29 | 0.06 | 5.71 | 0.04 MB | 38 | 4 | 0.53 |
| all | 6.24 | 0.42 | 0.29 | 0.06 | 5.89 | 0.04 MB | 38 | 4 | 0.56 |

**hello**

| config | cold s | first build s | warm s | upload s | build s | bytes sent | ops | conns | daemon CPU s |
|---|---|---|---|---|---|---|---|---|---|
| ietsp | 33.94 | 0.7 | 0.38 | 0.32 | 33.33 | 2.62 MB | 5 | 1 | 0.45 |
| nix | 34.5 | - | 1.03 | 0.74 | 33.76 | - | 0 | 1 | 0.57 |
| skip-sub | 34.2 | 0.83 | 0.47 | 0.33 | 33.46 | 2.69 MB | 7 | 1 | 0.5 |
| ub256 | 33.89 | 0.81 | 0.4 | 0.45 | 33.16 | 2.62 MB | 7 | 1 | 0.5 |
| ub256-2c | 34.06 | 0.85 | 0.41 | 0.48 | 33.29 | 2.62 MB | 10 | 2 | 0.55 |
| batch-all | 33.36 | 0.72 | 0.37 | 0.32 | 32.74 | 2.68 MB | 5 | 1 | 0.46 |
| conn4-batch-all | 33.67 | 0.74 | 0.34 | 0.33 | 33.02 | 2.68 MB | 8 | 4 | 0.59 |
| overlap4 | 35.03 | 1.92 | 0.35 | 1.52 | 33.08 | 2.68 MB | 31 | 4 | 0.84 |
| all | 33.51 | 0.8 | 0.35 | 0.4 | 32.79 | 2.68 MB | 10 | 4 | 0.58 |

**nixos-desktop**

| config | cold s | first build s | warm s | upload s | build s | bytes sent | ops | conns | daemon CPU s |
|---|---|---|---|---|---|---|---|---|---|
| ietsp | 2.54 | - | 0.37 | 2.17 | 0 | 40.76 MB | 3 | 1 | 2.11 |
| nix | 3.09 | - | 0.76 | 3.09 | 0 | - | 0 | 1 | 2.11 |
| skip-sub | 2.58 | - | 0.41 | 2.15 | 0 | 41.13 MB | 4 | 1 | 2.12 |
| ub256 | 3.3 | - | 0.37 | 2.94 | 0 | 40.76 MB | 19 | 1 | 2.18 |
| ub32 | 9.87 | - | 0.37 | 9.54 | 0 | 40.77 MB | 133 | 1 | 4.05 |
| ub256-2c | 3.36 | - | 0.42 | 3.01 | 0 | 40.77 MB | 25 | 2 | 2.63 |

### unix socket, socat listener

**g8x6**

| config | cold s | first build s | warm s | upload s | build s | bytes sent | ops | conns | daemon CPU s |
|---|---|---|---|---|---|---|---|---|---|
| ietsp | 0.99 | 0.18 | 0.08 | 0.02 | 0.9 | 0.06 MB | 5 | 1 | 0.43 |
| nix | 1.29 | - | 0.29 | 0.18 | 1.1 | - | 0 | 1 | 0.47 |
| per-drv | 7.21 | 0.15 | 0.07 | 0.03 | 7.13 | 0.06 MB | 53 | 1 | 0.69 |
| batch4 | 2.35 | 0.16 | 0.05 | 0.02 | 2.29 | 0.06 MB | 17 | 1 | 0.52 |
| batch16 | 1.6 | 0.16 | 0.06 | 0.02 | 1.51 | 0.06 MB | 11 | 1 | 0.49 |
| batch-all | 1.61 | 0.14 | 0.07 | 0.01 | 1.54 | 0.06 MB | 11 | 1 | 0.48 |
| conn2-batch-all | 1.62 | 0.17 | 0.07 | 0.03 | 1.52 | 0.06 MB | 12 | 2 | 0.52 |
| conn4-batch-all | 1.6 | 0.15 | 0.07 | 0.02 | 1.51 | 0.06 MB | 14 | 4 | 0.61 |
| conn4-per-drv | 1.83 | 0.11 | 0.07 | 0.02 | 1.77 | 0.07 MB | 56 | 4 | 0.76 |
| overlap4 | 1.53 | 0.12 | 0.06 | 0.02 | 1.45 | 0.06 MB | 15 | 4 | 0.54 |
| critical | 1.55 | 0.15 | 0.05 | 0.02 | 1.48 | 0.06 MB | 11 | 1 | 0.44 |
| critical-conn4 | 1.55 | 0.12 | 0.07 | 0.02 | 1.48 | 0.06 MB | 14 | 4 | 0.55 |
| all | 1.64 | 0.16 | 0.07 | 0.04 | 1.54 | 0.06 MB | 14 | 4 | 0.08 |

**g32x4**

| config | cold s | first build s | warm s | upload s | build s | bytes sent | ops | conns | daemon CPU s |
|---|---|---|---|---|---|---|---|---|---|
| ietsp | 2.09 | 0.26 | 0.07 | 0.05 | 1.98 | 0.15 MB | 5 | 1 | 0.98 |
| nix | 2.37 | - | 0.32 | 0.19 | 2.15 | - | 0 | 1 | 1.09 |
| per-drv | 18.85 | 0.19 | 0.07 | 0.05 | 18.74 | 0.17 MB | 133 | 1 | 1.84 |
| batch4 | 6.01 | 0.2 | 0.07 | 0.06 | 5.89 | 0.17 MB | 37 | 1 | 1.35 |
| batch16 | 2.88 | 0.21 | 0.06 | 0.05 | 2.76 | 0.17 MB | 14 | 1 | 1.13 |
| batch-all | 2.23 | 0.2 | 0.07 | 0.05 | 2.12 | 0.17 MB | 9 | 1 | 0.98 |
| conn2-batch-all | 2.28 | 0.16 | 0.06 | 0.04 | 2.19 | 0.17 MB | 10 | 2 | 1.11 |
| conn4-batch-all | 2.23 | 0.18 | 0.08 | 0.04 | 2.15 | 0.17 MB | 12 | 4 | 1.1 |
| conn4-per-drv | 4.44 | 0.16 | 0.05 | 0.04 | 4.33 | 0.17 MB | 136 | 4 | 1.67 |
| overlap4 | 1.42 | 0.15 | 0.07 | 0.05 | 1.33 | 0.17 MB | 25 | 4 | 0.92 |
| critical | 2.34 | 0.21 | 0.08 | 0.04 | 2.24 | 0.17 MB | 9 | 1 | 1.09 |
| critical-conn4 | 2.3 | 0.18 | 0.07 | 0.05 | 2.21 | 0.17 MB | 12 | 4 | 1.18 |
| all | 2.36 | 0.21 | 0.08 | 0.05 | 2.25 | 0.17 MB | 12 | 4 | 1.15 |

**chain30**

| config | cold s | first build s | warm s | upload s | build s | bytes sent | ops | conns | daemon CPU s |
|---|---|---|---|---|---|---|---|---|---|
| ietsp | 3.14 | 0.17 | 0.07 | 0.02 | 3.06 | 0.03 MB | 5 | 1 | 0.31 |
| nix | 3.3 | - | 0.26 | 0.17 | 3.13 | - | 0 | 1 | 0.37 |
| per-drv | 4.36 | 0.14 | 0.06 | 0.01 | 4.28 | 0.04 MB | 35 | 1 | 0.42 |
| batch4 | 4.42 | 0.15 | 0.07 | 0.02 | 4.34 | 0.04 MB | 35 | 1 | 0.41 |
| batch16 | 4.45 | 0.15 | 0.06 | 0.02 | 4.36 | 0.04 MB | 35 | 1 | 0.43 |
| batch-all | 4.41 | 0.14 | 0.05 | 0.02 | 4.34 | 0.04 MB | 35 | 1 | 0.42 |
| conn2-batch-all | 4.36 | 0.14 | 0.07 | 0.02 | 4.28 | 0.04 MB | 36 | 2 | 0.47 |
| conn4-batch-all | 4.41 | 0.12 | 0.07 | 0.01 | 4.37 | 0.04 MB | 38 | 4 | 0.51 |
| conn4-per-drv | 4.43 | 0.14 | 0.06 | 0.01 | 4.37 | 0.04 MB | 38 | 4 | 0.51 |
| overlap4 | 4.48 | 0.15 | 0.07 | 0.02 | 4.4 | 0.04 MB | 38 | 4 | 0.52 |
| critical | 4.29 | 0.13 | 0.07 | 0.01 | 4.21 | 0.04 MB | 35 | 1 | 0.41 |
| critical-conn4 | 4.4 | 0.15 | 0.07 | 0.02 | 4.3 | 0.04 MB | 38 | 4 | 0.57 |
| all | 4.36 | 0.14 | 0.07 | 0.02 | 4.31 | 0.04 MB | 38 | 4 | 0.52 |

**hello**

| config | cold s | first build s | warm s | upload s | build s | bytes sent | ops | conns | daemon CPU s |
|---|---|---|---|---|---|---|---|---|---|
| batch-all | 33.02 | 0.44 | 0.13 | 0.29 | 32.64 | 2.68 MB | 5 | 1 | 0.44 |
| conn4-batch-all | 33.72 | 0.41 | 0.13 | 0.26 | 33.38 | 2.68 MB | 8 | 4 | 0.53 |
| overlap4 | 33.82 | 0.5 | 0.11 | 0.35 | 33.39 | 2.68 MB | 31 | 4 | 0.61 |
| all | 33.4 | 0.42 | 0.12 | 0.27 | 33.06 | 2.68 MB | 10 | 4 | 0.53 |
| ietsp | 33.09 | 0.43 | 0.12 | 0.27 | 32.74 | 2.62 MB | 5 | 1 | 0.45 |
| nix | 32.55 | - | 0.53 | 0.55 | 32 | - | 0 | 1 | 0.49 |
| skip-sub | 33 | 0.45 | 0.12 | 0.28 | 32.61 | 2.69 MB | 7 | 1 | 0.46 |
| ub256 | 33.43 | 0.44 | 0.1 | 0.27 | 33.09 | 2.62 MB | 7 | 1 | 0.44 |
| ub256-2c | 32.98 | 0.52 | 0.12 | 0.38 | 32.48 | 2.62 MB | 10 | 2 | 0.49 |

**nixos-desktop**

| config | cold s | first build s | warm s | upload s | build s | bytes sent | ops | conns | daemon CPU s |
|---|---|---|---|---|---|---|---|---|---|
| ietsp | 2.28 | - | 0.17 | 2.11 | 0 | 40.76 MB | 3 | 1 | 2.05 |
| nix | 2.77 | - | 0.61 | 2.77 | 0 | - | 0 | 1 | 2.07 |
| skip-sub | 2.27 | - | 0.19 | 2.11 | 0 | 41.13 MB | 4 | 1 | 2.08 |
| ub256 | 2.5 | - | 0.18 | 2.32 | 0 | 40.76 MB | 19 | 1 | 2.22 |
| ub32 | 2.54 | - | 0.18 | 2.35 | 0 | 40.77 MB | 133 | 1 | 2.16 |
| ub256-2c | 2.76 | - | 0.18 | 2.58 | 0 | 40.77 MB | 22 | 2 | 2.44 |


## Results: bld1

### root@bld1.ztm.io, no ControlMaster

**g8x6**

| config | cold s | first build s | warm s | upload s | build s | bytes sent | ops | conns | daemon CPU s |
|---|---|---|---|---|---|---|---|---|---|
| ietsp | 0.18 | - | 0.17 | 0.01 | 0 | 0.06 MB | 3 | 1 | 0.04 |
| nix | 0.25 | - | 0.23 | 0.25 | 0 | - | 0 | 1 | 0.04 |
| skip-sub | 0.17 | - | 0.17 | 0.01 | 0 | 0.06 MB | 4 | 1 | 0.04 |
| ub256 | 0.17 | - | 0.17 | 0.01 | 0 | 0.06 MB | 3 | 1 | 0.05 |
| ub32 | 0.18 | - | 0.17 | 0.01 | 0 | 0.06 MB | 4 | 1 | 0.04 |
| ub256-2c | 0.16 | - | 0.17 | 0.01 | 0 | 0.06 MB | 4 | 2 | 0.07 |

**g32x4**

| config | cold s | first build s | warm s | upload s | build s | bytes sent | ops | conns | daemon CPU s |
|---|---|---|---|---|---|---|---|---|---|
| ietsp | 0.19 | - | 0.17 | 0.03 | 0 | 0.15 MB | 3 | 1 | 0.06 |
| nix | 0.29 | - | 0.26 | 0.29 | 0 | - | 0 | 1 | 0.06 |
| skip-sub | 0.19 | - | 0.17 | 0.03 | 0 | 0.16 MB | 4 | 1 | 0.05 |
| ub256 | 0.19 | - | 0.16 | 0.03 | 0 | 0.15 MB | 3 | 1 | 0.06 |
| ub32 | 0.19 | - | 0.16 | 0.03 | 0 | 0.15 MB | 7 | 1 | 0.06 |
| ub256-2c | 0.2 | - | 0.16 | 0.03 | 0 | 0.15 MB | 4 | 2 | 0.08 |

**chain30**

| config | cold s | first build s | warm s | upload s | build s | bytes sent | ops | conns | daemon CPU s |
|---|---|---|---|---|---|---|---|---|---|
| ietsp | 0.18 | - | 0.16 | 0.01 | 0 | 0.03 MB | 3 | 1 | 0.04 |
| nix | 0.27 | - | 0.24 | 0.27 | 0 | - | 0 | 1 | 0.04 |
| skip-sub | 0.17 | - | 0.16 | 0.01 | 0 | 0.03 MB | 4 | 1 | 0.04 |
| ub256 | 0.17 | - | 0.17 | 0.01 | 0 | 0.03 MB | 3 | 1 | 0.04 |
| ub32 | 0.17 | - | 0.16 | 0.01 | 0 | 0.03 MB | 3 | 1 | 0.04 |
| ub256-2c | 0.16 | - | 0.17 | 0.01 | 0 | 0.03 MB | 4 | 2 | 0.08 |

**nixos-desktop**

| config | cold s | first build s | warm s | upload s | build s | bytes sent | ops | conns | daemon CPU s |
|---|---|---|---|---|---|---|---|---|---|
| ietsp | 1.58 | - | 0.28 | 1.31 | 0 | 40.76 MB | 3 | 1 | 1.27 |
| nix | 2.11 | - | 0.66 | 2.11 | 0 | - | 0 | 1 | 1.26 |
| skip-sub | 1.6 | - | 0.28 | 1.31 | 0 | 41.13 MB | 4 | 1 | 1.27 |
| ub256 | 1.83 | - | 0.27 | 1.56 | 0 | 40.76 MB | 19 | 1 | 1.26 |
| ub32 | 1.99 | - | 0.28 | 1.72 | 0 | 40.77 MB | 133 | 1 | 1.27 |
| ub256-2c | 1.9 | - | 0.27 | 1.64 | 0 | 40.77 MB | 23 | 2 | 1.38 |

### root@bld1.ztm.io, ControlMaster

**g8x6**

| config | cold s | first build s | warm s | upload s | build s | bytes sent | ops | conns | daemon CPU s |
|---|---|---|---|---|---|---|---|---|---|
| ietsp | 0.07 | - | 0.06 | 0.01 | 0 | 0.06 MB | 3 | 1 | 0.04 |
| nix | 0.15 | - | 0.14 | 0.15 | 0 | - | 0 | 1 | 0.04 |
| skip-sub | 0.07 | - | 0.06 | 0.01 | 0 | 0.06 MB | 4 | 1 | 0.05 |
| ub256 | 0.07 | - | 0.06 | 0.01 | 0 | 0.06 MB | 3 | 1 | 0.04 |
| ub32 | 0.07 | - | 0.07 | 0.01 | 0 | 0.06 MB | 4 | 1 | 0.04 |
| ub256-2c | 0.07 | - | 0.06 | 0.01 | 0 | 0.06 MB | 4 | 2 | 0.07 |

**g32x4**

| config | cold s | first build s | warm s | upload s | build s | bytes sent | ops | conns | daemon CPU s |
|---|---|---|---|---|---|---|---|---|---|
| ietsp | 0.09 | - | 0.07 | 0.03 | 0 | 0.15 MB | 3 | 1 | 0.06 |
| nix | 0.2 | - | 0.15 | 0.2 | 0 | - | 0 | 1 | 0.06 |
| skip-sub | 0.09 | - | 0.07 | 0.03 | 0 | 0.16 MB | 4 | 1 | 0.06 |
| ub256 | 0.09 | - | 0.06 | 0.03 | 0 | 0.15 MB | 3 | 1 | 0.06 |
| ub32 | 0.09 | - | 0.06 | 0.03 | 0 | 0.15 MB | 7 | 1 | 0.06 |
| ub256-2c | 0.09 | - | 0.07 | 0.03 | 0 | 0.15 MB | 4 | 2 | 0.09 |

**chain30**

| config | cold s | first build s | warm s | upload s | build s | bytes sent | ops | conns | daemon CPU s |
|---|---|---|---|---|---|---|---|---|---|
| ietsp | 0.07 | - | 0.06 | 0.01 | 0 | 0.03 MB | 3 | 1 | 0.04 |
| nix | 0.16 | - | 0.15 | 0.16 | 0 | - | 0 | 1 | 0.04 |
| skip-sub | 0.07 | - | 0.06 | 0.01 | 0 | 0.03 MB | 4 | 1 | 0.04 |
| ub256 | 0.07 | - | 0.06 | 0.01 | 0 | 0.03 MB | 3 | 1 | 0.04 |
| ub32 | 0.07 | - | 0.06 | 0.01 | 0 | 0.03 MB | 3 | 1 | 0.04 |
| ub256-2c | 0.07 | - | 0.06 | 0.01 | 0 | 0.03 MB | 4 | 2 | 0.07 |

**nixos-desktop**

| config | cold s | first build s | warm s | upload s | build s | bytes sent | ops | conns | daemon CPU s |
|---|---|---|---|---|---|---|---|---|---|
| ietsp | 1.49 | - | 0.17 | 1.32 | 0 | 40.76 MB | 3 | 1 | 1.26 |
| nix | 2.08 | - | 0.73 | 2.08 | 0 | - | 0 | 1 | 1.27 |
| skip-sub | 1.5 | - | 0.17 | 1.32 | 0 | 41.13 MB | 4 | 1 | 1.29 |
| ub256 | 1.69 | - | 0.17 | 1.53 | 0 | 40.76 MB | 19 | 1 | 1.26 |
| ub32 | 1.86 | - | 0.17 | 1.7 | 0 | 40.77 MB | 133 | 1 | 1.27 |
| ub256-2c | 1.71 | - | 0.19 | 1.55 | 0 | 40.77 MB | 23 | 2 | 1.36 |


## What limits each lever (Nix 2.34, `src/libstore`)

- **One `Worker` per `BuildPathsWithResults`** (build/entry-points.cc:48-52):
  every op constructs a `Worker`, makes the goals, parses the drvs, runs the
  loop, and tears it down. Per-derivation ops pay that per node, and while
  one op runs its connection is busy: no other op can be in flight on it.
  With one connection, `--per-drv` serialises what the daemon would run in
  parallel.
- **`max-jobs` is per `Worker`, set by the client** (daemon.cc:234-244 applies
  `clientSettings.maxBuildJobs` for the connection, trusted or not;
  build/worker.cc:299-309 `waitForBuildSlot` compares `nrLocalBuilds` with
  it). N connections are N workers with N × `max-jobs` slots. `ietsp` sends
  the CPU count.
- **Output locks and the 5 s poll** (build/derivation-building-goal.cc:401-413;
  `build-poll-interval` default 5, worker-settings.hh:316): two workers that
  want the same derivation do not cooperate; the second waits on the lock
  files and retries every `pollInterval` seconds. The scheduler never submits
  a derivation before its inputs are built and never the same one twice, so
  this path is never entered; a scheduler that submitted overlapping goals
  on several connections would pay it.
- **Registration needs every reference valid** (local-store.cc:896-950,
  `registerValidPaths` resolves each reference with `queryValidPathId`,
  which throws for an absent path). A `.drv` references its input drvs and
  sources, so an upload batch must be a topological prefix: an object may
  only follow objects that are valid or earlier in the same batch. This is
  why a second uploading connection only gets objects independent of the
  batch in flight, and why `--skip-substitutable` may skip a path only when
  every referrer is skipped too.
- **`AddMultipleToStore` is sequential, one transaction per path**
  (store-api.cc:223-238 loops over the framed source; local-store.cc:985-1096
  `addToStore` takes a temp root, a lock file, restores the NAR, hashes it,
  checks the content address, then `registerValidPath` in its own SQLite
  transaction). The daemon-side cost is per path, not per op; with two
  connections the two `LocalStore`s share one SQLite file and one
  `big-lock`, and SQLite's busy handler sleeps up to 100 ms between retries
  (sqlite.cc:279-292).
- **A fresh store cannot take N first connections at once**: four daemons
  creating the schema of an empty store together made one die with "SQLite
  database is busy" before the busy handler was installed. The bench runs
  `nix-store --init` first; a real remote store has a schema already.
- **`QueryValidPaths` is one SQLite lookup per path** (local-store.cc:804-811)
  and `QueryMissing` treats an absent drv as `unknown` (misc.cc), so nothing
  can be learned about a closure before its drvs are registered.
- **Substitution of a missing drv fetches its closure**: for an absent input
  drv, `DerivationTrampolineGoal::init` makes an Opaque goal that substitutes
  the drv file (build/derivation-trampoline-goal.cc:72-97), and a
  `PathSubstitutionGoal` fetches the references first. Skipping a
  substitutable sub-closure moves its transfer from the client to the
  substituter; it does not remove it.
- **Nix's ssh-ng client** adds, per connection, a `started` line to wait for
  and a master probe (ssh.cc) before the handshake; over the shim these cost
  nothing, over a real link each is a process start plus a round trip.

## bld1 leg

`--host root@bld1.ztm.io` (32 cores, Nix 2.35pre, 0.11 s per ssh round trip)
runs only uploads. A `local?root=/tmp/ietsp-sched/store` store there cannot
build from an ssh session: bld1 has no `nixbld` users (`auto-allocate-uids`),
the builder then needs a bind mount inside a user namespace
(unix/build/linux-derivation-builder.cc:298,433), and on bld1 that mount
fails with EPERM for any path backed by `/tmp` (it succeeds for `/root` and
`/var/empty`, and it succeeds under `/tmp` on this machine with the same
kernel 6.18.45, so it is a bld1 policy). With build users forced on, the
build directory must not have a world-writable ancestor
(unix/build/derivation-builder.cc:723-756), and `/tmp` is one. A scratch
root outside `/tmp` would remove both obstacles; that was outside the rules
for this run. The build levers on a real link are therefore not measured;
the upload and round-trip levers are.

What the bld1 tables say (uploads; every workload with `--no-build`):

- **The link is fast and the connection is the cost.** A cold graph upload
  (49-129 paths, 0.03-0.15 MB) takes 0.17-0.19 s without ControlMaster and
  0.07-0.09 s with it; warm runs are 0.17 s and 0.06 s. The 0.11 s is the
  ssh connect, authentication and remote exec; what is left, 0.06 s for
  about 6 round trips of the flow, puts the protocol round trip near
  10 ms. `nix copy` of the same closures takes 0.25-0.29 s and 0.15-0.20 s.
- **nixos-desktop (4184 paths, 40 MB): 1.58 s, 1.49 s with ControlMaster,
  against 2.35 s locally.** The daemon on bld1 registers the closure in
  1.27 s of CPU against 2.1 s here (faster cores and disk); the upload is
  daemon-bound on either machine. `nix copy` takes 2.11 s / 2.08 s.
- **Upload batching only costs**: batches of 256 add 0.25 s (16 more round
  trips of the framed op), batches of 32 add 0.4 s. A second uploading
  connection adds 0.3 s and 9% daemon CPU, as locally. With ControlMaster
  the second connection is a channel, not a second ssh, and the numbers
  are the same: the contention is in the daemon, not in ssh.
- **ControlMaster** saves 0.1 s per connection and nothing else; the pool
  levers were not helped by it because none of them pays here. For a
  client that opens N connections it is the difference between N ssh
  logins and one.
- `--skip-substitutable` skips nothing (no substituters on the scratch
  daemon) and costs one op, 0.01 s.

## Recommendation

Keep the five-op flow: one connection, one `AddMultipleToStore`, one
`BuildPathsWithResults` on the root. No lever became the default, so the
levers and their flags were removed (bc64b0d) rather than kept as options
nobody would set:

- Per-derivation and batched submission never beat the root op on one
  connection (+42 ms and a round trip per op, a `Worker` per op) and lose
  by up to 10× at k = 1. The daemon already schedules the graph with
  per-node completion inside one op; the client gains nothing from
  redoing it except events it can also read from the activity stream.
- Extra connections pay only as extra `Worker`s when a layer is wider than
  `max-jobs` (g32x4: −20%). The cheaper form of that is a larger
  `max-jobs` in `SetOptions`, which the daemon applies per connection
  (daemon.cc:240); worth a `--max-jobs` flag, not a pool.
- Upload batching and a second uploader only add round trips and SQLite
  contention; the daemon's sequential per-path registration is the
  bottleneck (0.3-0.5 ms per path) and no client-side cut changes it.
- Critical-path ordering has nothing to order when every op takes the
  whole ready set.
- `--skip-substitutable` cannot skip a path whose referrer is uploaded
  (registration needs every reference valid), and on these closures
  nothing qualified.

What would pay, outside this protocol's levers: fewer round trips per
connection (already 8), a daemon that registers paths in one transaction
per op instead of one per path, and ControlMaster or a persistent
connection to remove the 0.1 s ssh setup, which is now the largest fixed
cost of a warm build on a real host.

## Commands

The matrix ran as, at 6b93209:

```
export TMPDIR=$HOME/tmp/nixtmp NIX_REMOTE=daemon
nix develop -c cargo build --release
nix develop -c nu scripts/sched-bench.nu --runs 3 --latency 0,20 --targets shim,unix
nix develop -c nu scripts/sched-bench.nu --host root@bld1.ztm.io --targets host \
  --workloads g8x6,g32x4,chain30,nixos-desktop
```

Each run wrote `bench/results/sched/<stamp>.{json,md}`; the tables above are
the merged `local.{json,md}` and `bld1.{json,md}`, kept in history at 6b93209.
The synthetic graphs are still `bench/graph.nix`:

```
nix-instantiate bench/graph.nix --arg width 8 --arg depth 6
```

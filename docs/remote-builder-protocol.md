# Remote builder protocol

Status: v2, 2026-09-17: content is addressed by amber-store keys (v1 of
2026-09-16 used sha256 of 4 MiB NAR slices). Wire: gRPC over HTTP/2
cleartext (tonic + prost), chosen because it is boring, tooled, and matches
nix-grpc-store and tribuchet. The protocol lives in `proto/builder.proto`
and is transport-agnostic; QUIC (iroh) can carry the same messages
later.

Fallback: the same `ietsp build` speaks the Nix worker protocol to any
`nix-daemon` (`--builder daemon`, `unix://`, `ssh-ng://`); see the section at
the end.

## Problem

The Nix worker protocol is a serial RPC stream designed for a local daemon.
As a remote-build transport it costs one round trip per query and per path,
serializes uploads as full NARs, re-sends inputs the builder already has,
and forces the client to own a local store. `nix-grpc-store` measured 5.5x
over ssh-ng by batching alone. The client we want never touches a local
store: it evaluates in memory, asks the builder what is missing, streams
only that, and waits for the root output.

## Client flow

```
eval  ->  Derivation graph in memory (ietsp)
      ->  Plan: closure of the root drv: drvs and input sources, hashed
      ->  Have(keys, paths)                  one request, one response
      ->  Put(stream of missing objects)     one stream, then manifests and drvs
      ->  Build(root drv, wanted outputs)    one request; server streams events
      <-  events: scheduled, started, log, built, failed, done
```

Three RPCs, three round trips before the build starts (plus the TCP/HTTP2
connect). Everything the builder can look up by hash it looks up by hash.

## Object model (`src/amber/`)

A store path is not shipped as a NAR but as amber-store's object graph:
the model of `amber-store-core` (core-rs, github.com/amber-store/core-rs
at rev `72f38e9e`) with the parameters our amber deployment uses for every
Nix store path. The keys are amber's, byte for byte: an object ietsp
uploads is the object an amber store ingest would store for the same
tree, so an `amber-hostd` holding a closure could answer `Have` and take
`Put` directly, and two clients uploading the same file pay for it once.

Key (`amber::key`): 32 bytes. Byte 0 is the object type in the high nibble
(0 Blob, 1 FileNode, 2 DirLeaf, 3 DirNode) and the size of the length
field minus one in the low three bits; then the logical length, big-endian
with no leading zero; then the BLAKE3 digest of the object's bytes,
truncated to fill the key. Hex is the wire and log form.

Objects (`amber::objects`), all deterministic CBOR (RFC 8949 4.2, the bytes
fxamacker/cbor emits for the Go implementation) except the Blob:

| type | bytes | key length field |
|---|---|---|
| Blob | raw file content, one CDC chunk | the byte count |
| FileNode | array of child keys (Blob or FileNode), file order | the file size (sum of the children) |
| DirLeaf | array of entry maps, sorted by name | own bytes + sum of the entries' content-key lengths |
| DirNode | array of `[separator name, child key]` pairs | own bytes + sum of the children's lengths |

An entry map has integer keys: 0 name (bytes), 1 mode (POSIX `st_mode`),
2 uid, 3 gid, 4 mtime (ns), 5 content key (files and directories), 6 link
target (symlinks). ietsp writes what Nix leaves on a registered store path
(`amber::tree::Meta::Canonical`): uid 0, gid 0, mtime 1 s, mode 0444 or
0555 for files, 0555 for directories, 0777 for symlinks. That is the
metadata `lstat` reports inside `/nix/store`, so the keys of an uploaded
source equal those of the same path ingested by an amber store deployment, and they depend
on the content alone.

Chunking (`amber::cdc`): UltraCDC over file bytes with min 8 KiB, normal
32 KiB, max 64 KiB (an ISC port of PlakarKorp/go-cdc-chunkers through
core-rs). A file with one chunk is that Blob; more chunks get a FileNode,
and the child-key stream is itself cut by the item chunker (average run
2^7 items, min 32, max 512; a boundary where the low 7 bits of
BLAKE3(item) are zero), so a file above about 16 MB gets a second index
level. A directory's sorted entries are cut the same way into DirLeaf
runs, with DirNode levels above 512 entries. An empty file is the empty
Blob (`0000af13...`); an empty directory is the empty DirLeaf (`0x80`).

Equality with core-rs is proven, not assumed: `tools/amber-diff/` builds
an `examples/` program inside a scratch copy of the core-rs crate that runs
the real `ingest::objects` with our deployment's parameters, and `ietsp amber-keys
PATH` prints the same list from `src/amber/`. Run 2026-09-17: a synthetic
tree (empty, 1-byte, exactly 8 KiB / 32 KiB / 64 KiB, 200 KiB, 5 MiB and 40
MiB random, 300 KB of zeros, executable, symlinks, nested dirs, an empty
dir, unicode and punctuation names, a 3,000-entry directory), the nixpkgs
source tree (93,904 objects), a hello output (155) and firefox-unwrapped
(10,146): 213,106 keys and every non-Blob object's bytes compared, 0
differences. The core-rs golden vectors (`keys.json`, `ultracdc.json`,
`item_chunker.json`) and the Go oracle digests of the builders run as unit
tests. ietsp does not depend on the crate itself: it is a git dependency,
not a crates.io release, and it would pull its whole dependency tree into
the nix package build.

## Messages (`proto/builder.proto`)

- `Object { key, data }`: one amber object. The builder recomputes the key
  from `data` (children included, for the length field) and rejects a
  mismatch. `ObjectBatch { objects[] }` carries about 1 MiB of them.
- `Manifest { store_path, nar_hash, nar_size, ca, references[], root,
  executable, symlink_target, keys[] }`: a source tree, addressed by its
  store path. `root` is the key of the top-level node (DirLeaf or DirNode
  for a directory; Blob or FileNode for a regular file, with `executable`),
  or empty for a symlink with `symlink_target`. `keys` is every object the
  tree needs, root included. `ca` says how Nix derived the path: `SOURCE`
  (recursive sha256, no references: `./path`, `fetchTarball`) or `TEXT`
  (flat sha256 of one regular file plus references: `builtins.toFile`; one
  file object). The builder restores the tree, hashes the NAR, recomputes
  the path, and rejects a manifest that does not match, so it never trusts
  the client's path.
- `DrvBlob { drv_path, content }`: a derivation. Its identity is its store
  path; `content` is the key of the ATerm as a file object. The builder
  reads the ATerm, parses it, and checks that it hashes to `drv_path`.
- `HaveRequest { keys[], paths[] }` -> `HaveResponse { missing_keys[],
  missing_paths[] }`: one batch over the union of every tree's keys and
  every store path.
- `PutRequest { objects | manifest | drv }` client-streamed; `PutResponse {
  objects, paths }` counts. Objects precede the manifest or drv that needs
  them, so the builder checks that every key in `Manifest.keys` (or the
  drv's content) is present when the record arrives.
- `BuildRequest { root_drv_path, outputs[], keep_going, priority }` ->
  stream of `BuildEvent { Scheduled | Started | Log | Built | Failed | Done }`.
- `FetchRequest { store_path }` -> stream of `NarChunk { data }` (the NAR
  in 4 MiB pieces). The client uses it for `--fetch-to`.

## Client (`src/remote/`, `ietsp build`)

`ietsp build [--builder URL] [--timings] [--no-build] [--out NAME]...
[-I prefix=path]... [--arg name expr] [--argstr name str] [-A attr]
(-E EXPR | FILE)` evaluates like `ietsp eval`, forces the result to
derivations (an attrset with `drvPath`, a string with derivation context,
or a list of those; `!out!drv` context also names the wanted output), and
builds each root. Nothing is read from the local store on this path:

- Every derivation the evaluation created is in `Evaluator::drv.drvs`
  (`DrvInfo { outputs, references, aterm }`); `derivationStrict` keeps the
  ATerm it hashed, so the plan never re-serializes a drv. An input drv that
  is not in the table came from a pre-existing `/nix/store/*.drv` reached
  through string context or `import`; the plan reads that file and
  `--timings` reports the count as `read from /nix/store`.
- Every source the evaluation added is in `Evaluator::sources`
  (`store::source::SourceOrigin`, accessor `source_origins()`): `./path`
  and `builtins.path` record the original local tree plus the set of
  entries a `filterSource` filter accepted, so the plan re-dumps the same
  NAR; `builtins.toFile` records the contents and references in memory.
  After hashing, the plan checks that the NAR still yields the planned
  store path and fails if the tree changed since evaluation.
- Sources with no recorded origin (only possible under a drv read from the
  store) go to a batch locator: one `nix path-info --json` call.

`plan.rs`: `plan(roots, Inputs { drvs, sources }, locate)` walks the drv
closure once, iteratively, with a visited set (O(n)), emits drvs in
post-order, and hashes the sources on a thread pool: the NAR hash (one
sequential dump, what Nix addresses the path by) and the amber object
graph (`amber::tree::ingest_path`, subdirectories and files in parallel
under a semaphore). The plan keeps every key per tree (`Tree { root, keys
}`), not the bytes. `client.rs`: `build(url, plan, root, outputs,
upload_only, on_event)`: one `Have` over the union of keys and paths, one
`Put` stream (a blocking thread re-walks each tree that has a missing key
and sends the missing objects in 1 MiB batches, a key once even when
several trees hold it, then the manifest or drv if the path is missing),
then `Build` with events forwarded; `--no-build` stops after `Put`.

Legacy mode: `ietsp build [--source STOREPATH=LOCAL]... DRV` where `DRV` is
a `.drv` store path or a copy named `<hash>-<name>.drv`. The root comes from
that file, its closure from `/nix/store`, and `--source` names the original
local tree of an input source. Same planner, empty eval tables.

## Reference server (`ietsp-builderd --listen ADDR --store-dir DIR`)

Objects live under `DIR/objects/<key hex>` as their raw bytes; manifests
and drvs under `DIR/paths/<store basename>`. On `Build`:

1. Walk the closure of the root from the object store (a drv's ATerm is
   read back through its content key).
2. `nix-store --check-validity --print-invalid <all paths>`: one call tells
   which paths the local Nix store lacks.
3. For each missing source, restore the tree from its objects into a temp
   dir (`amber::tree::restore`: files from Blob and FileNode, entries from
   DirLeaf and DirNode, the executable bit from the entry mode, symlinks
   inline), then dump it (`store::nar::nar_hash_filtered`) and check the
   NAR hash, size, and that the content yields the planned store path.
   Write each missing drv's ATerm to a file.
4. Register everything with one `nix eval --impure --json` of a generated
   expression: `builtins.path { path; name; }` for a `SOURCE` path, and
   `builtins.toFile name (builtins.appendContext (builtins.readFile file)
   { "<ref>" = { path = true; }; ... })` for a `TEXT` path and for every
   `.drv` (its references are `inputSrcs` plus `inputDrvs`). The list is in
   topological order, so each path's references are valid before it is
   added. The daemon recomputes every path; the server compares the result
   to the plan and fails the build on any mismatch.
5. `nix-store --realise --log-format internal-json <root>`; activity type
   105 (actBuild) becomes `Started`, result type 101 becomes `Log`, `msg`
   lines and raw stderr become unattributed `Log`.
6. `nix-store -q --outputs <root>` zipped with the drv's sorted output names
   becomes `Built` events, then `Done`.

What did not work for registering a `.drv`: `nix store add --mode text`
ignores references, so it yields a different path
(`text:sha256:...` without the `:ref` list in the type string).
`nix derivation add` works but needs the JSON form, which means a second
serializer. `nix-store --import` needs a trusted user or `require-sigs =
false`. `builtins.toFile` with appended opaque context is the smallest route
and produced the exact path in every test.

## Measured (`scripts/remote-e2e.nu`, release build, localhost)

Legacy `.drv` path (closure read from `/nix/store`):

| case | plan | have | put | build (realise) |
|---|---|---|---|---|
| 1 file tree + 1 drv, both absent on the builder and deleted from the store | 0.002 s | 6 keys, 2 paths, 8 missing, 0.001 s | 6 objects, 2 paths, 810 B, 0.001 s | 1.4-2.2 s |
| hello (616 drvs, 160 sources), cold builder | 0.136 s | 786 keys, 776 paths, 1,562 missing, 0.010 s | 786 objects, 776 paths, 2.0 MB, 0.083 s | 0.42 s |
| hello, warm builder | 0.162 s | 786 keys, 776 paths, 0 missing, 0.010 s | skipped | 0.36 s |

v1 for comparison (sha256 chunks): hello cold `have` 936 digests 0.010 s,
`put` 776 objects + 160 chunks, 2.0 MB, 0.102 s.

The plan phase is dominated by `nix path-info --json` on 160 paths
(about 0.11 s); the NAR dump and hashing of 2 MB take the rest. The
"build" column is `nix-store --realise` on an already-built closure, so it
measures the Nix side: validity check, registration (a no-op here), and
the realise itself. Case 1 includes the actual sandbox build.

Eval path (no local store reads; `strace -e openat` shows only the dynamic
loader opening glibc for the `ietsp` binary itself):

| case | eval | plan | have | put | build (realise) | total |
|---|---|---|---|---|---|---|
| `-E 'derivation { ... "${./somefile}" ... }'`, absent on the builder | 0.003 s | 0.001 s | 2 keys, 2 paths, 4 missing | 2 objects, 2 paths, 462 B, 0.001 s | 1.47 s | 1.5 s |
| hello via `-E '(import <nixpkgs> {}).hello'`, cold builder | 0.116 s | 0.011 s | 786 keys, 776 paths, 1,562 missing, 0.017 s | 786 objects, 776 paths, 2.0 MB, 0.094 s | 0.39 s | 0.62 s |
| hello, warm builder (eval from the cache) | 0.028 s | 0.013 s | 786 keys, 776 paths, 0 missing, 0.011 s | skipped | 0.40 s | 0.45 s |
| `bench/workloads/nixos-desktop.nix` (closure 3,533 drvs, 651 sources), cold builder, `--no-build` | 1.19 s | 0.154 s | 7,563 keys, 4,184 paths, all missing, 0.078 s | 7,563 objects, 4,184 paths, 34.1 MB, 0.66 s | skipped | 2.17 s |
| nixos-desktop, warm builder, `--no-build` | 1.19 s | 0.121 s | 7,563 keys, 4,184 paths, 0 missing, 0.045 s | skipped | skipped | 1.44 s |

v1 for comparison, same machine (`scripts/remote-bench.nu` did not exist;
the numbers are from the 2026-09-16 run): nixos-desktop cold `have` 4,839
digests 0.049 s, `put` 4,184 objects + 655 chunks, 36.5 MB, 0.52 s; warm
`have` 0.028 s. The eval column is not comparable (the eval cache landed in
between).

What the object model changes for nixos-desktop:

- Objects: the 651 sources and 3,533 drvs emit 7,875 keys; 7,563 are
  unique, and 305 unique keys are held by two or more trees (a drv's ATerm
  is unique, so the sharing is among sources). The 312 duplicate emissions
  are sent once: 34.1 MB on the wire against 36.5 MB, 6.6% less, for the
  same closure. Every source with one chunk is one Blob, the same object
  under any name; a re-upload after a one-file edit resends that file's
  chunks and the DirLeaf on its path, not the whole NAR.
- Counts: `Have` asks about 7,563 keys plus 4,184 paths (11,747 items
  against 4,839 digests) and takes 0.078 s; `Put` writes 11,747 files on
  the builder against 4,839 and takes 0.66 s against 0.52 s. The per-object
  file store of the reference server is the cost; the profile is the drv
  parser and BLAKE3 verification, not the socket. Running `put_one` on a
  blocking thread per message costs 1.2 s more (measured, reverted).
- Plan: 0.154 s against 0.101 s, the second pass over the source bytes
  (NAR hash, then chunking).

Keeping every ATerm in memory costs +15 MB RSS and no measurable eval time;
the key lists add 7,875 x 32 bytes. A `nix build` equivalent on the same
machine has no remote-only path to compare against: `nix-instantiate`
writes 6.6k drvs and 650 sources into the local store first, then `nix copy
--to` and a remote realise; see the estimate below.

## Compared with the Nix worker protocol (estimate)

For measurements of the worker protocol driven by this client, see
"Fallback: worker protocol" below. This section estimates what `nix-build`
itself does. No ssh loopback is configured on this machine, so this is read from
`src/libstore/worker-protocol.hh`, `remote-store.cc`, `store-api.cc`
(`copyPaths`), `derivations.cc` (`writeDerivation`), `libfetchers/
fetch-to-store.cc`, and `src/nix/build-remote/build-remote.cc` in Nix
2.35pre (`/nix/store/2ijv0g6069dsh55z3bdr5ln2iv69mw7r-source`). Each worker
op is one request/response round trip on a serial connection.

To reach the same state as one `ietsp build` of hello (616 drvs, 160
sources) with `nix-build --builders ssh-ng://host`:

| phase | ops on the local daemon | ops on the remote |
|---|---|---|
| eval writes sources (`fetchToStore`): `IsValidPath` then `AddToStore` per path | 160 + up to 160 | 0 |
| eval writes drvs (`writeDerivation`): `AddTempRoot`, `IsValidPath`, `AddToStore` per drv | 616 x 2 + up to 616 | 0 |
| `BuildPathsWithResults` for the root | 1 | 0 |
| build hook per built drv: `copyPaths` = `QueryValidPaths` (1) + `QueryPathInfo` per missing path (local) + `AddMultipleToStore` (1) | per drv: missing paths | per drv: 2 |
| `BuildDerivation` per built drv, then `copyPaths` of outputs back: `QueryValidPaths` + `QueryPathInfo` per output + `AddMultipleToStore` | per drv: 1 | per drv: 1 + 1 + outputs |

Cold local store and one built drv with all inputs cached on the remote:
about 1,700 daemon round trips before the build plus 5 remote round trips,
against 4 round trips total here. Warm local store: about 1,400 daemon
round trips (the validity checks and temp roots still run) plus the same
remote ops. The remote-side batching in Nix >= 2.4 is good; the cost sits
in the mandatory local store and in the per-path eval writes. Nix also
sends full NARs; this protocol sends content-defined chunks and skips the
ones the builder has.

## Not in v2

- Auth. Next step is mTLS on the tonic channel; the reference server binds
  to localhost cleartext.
- Retries, resume, and backpressure beyond HTTP/2 flow control.
- `Fetch` on the client is only `--fetch-to DIR` on the root outputs, whole
  NARs collected in memory; no object reuse against what the client has.
  Outputs could travel as the same object graph (the builder would ingest
  them as amber-store does), which is the natural next step.
- The object model carries what a NAR carries. Extended attributes,
  devices, fifos and sockets are refused at ingest, and the builder's
  `DirLeaf` decoder rejects entries with those payloads (map keys 7-9);
  amber's `dir-cbor` refuses them the same way. Hard links are ingested
  as separate files (they dedup by content, as in a NAR).
- Directory entry mtime: amber's `dir-cbor` path uses mtime 0; we use the
  canonical store-path metadata (mtime 1 s), which is what `lstat` reports
  on a registered store path.
- A tree that has a missing key is re-walked whole on `Put`, and the plan
  reads every source twice (NAR hash, then chunks). Both are cheap next to
  eval today; a fused walk that hashes the NAR while chunking would remove
  the second read.
- The reference server keeps one file per object; a pack file with an
  index (what amber's packstore is) would make `Have` and `Put` cheaper
  for closures with hundreds of thousands of objects.
- `amber::tree::Meta::Lstat` exists for the differential test only; the
  protocol always sends canonical metadata.
- Input sources other than `SOURCE` and `TEXT`: flat fixed-output paths
  (`nix-store --add-fixed`) and output paths used as `inputSrcs`
  (`builtins.storePath`) are rejected at plan time.
- `outputs`, `keep_going`, `priority` in `BuildRequest` are carried but the
  reference server builds every output of the root.
- Signatures on `Built` events, blob garbage collection, concurrent-build
  locking on the server (the daemon serializes conflicting registrations).
- The nixos-desktop comparison against `nix build --builders ssh-ng://`
  (needs an ssh loopback builder on this machine).
- Flat fixed-output sources (`builtins.path { recursive = false; }`) have
  no recorded origin; they fall back to the store locator, which rejects
  them.

## Fallback: worker protocol

`ietsp build --builder daemon | unix:///path | ssh-ng://[user@]host` speaks
the Nix worker protocol (`src/libstore/worker-protocol.hh`, Nix 2.34.8) to a
`nix-daemon`, from the same in-memory plan. `daemon` is
`/nix/var/nix/daemon-socket/socket`; `ssh-ng://` runs `ssh host nix-daemon
--stdio` and uses its stdio, as `ssh-store.cc` does. `http://` and `https://`
stay on gRPC. Code: `src/remote/worker/` (`wire.rs` framing and the
`FramedSink`; `mod.rs` transport, handshake, the `STDERR_*` loop, the ops;
`build.rs` the client flow). No Nix library, no store; the protocol is
written from the C++ source.

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

The client flow mirrors the gRPC one, one op per phase:

1. `QueryValidPaths` on every planned path (sources, text paths, drvs) in one
   call, substitute flag off.
2. `AddMultipleToStore` of the missing ones in one framed stream, in plan
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
3. `BuildPathsWithResults` in normal mode on `<root drv>!<out1,out2>` (`!*`
   without `--out`), the legacy `DerivedPath` string that protocol >= 1.30
   reads. A failure status becomes a `Failed` event.
4. `QueryDerivationOutputMap` on the root: `Built` events per output.
5. `NarFromPath` per wanted output with `--fetch-to DIR`, restored with
   `store::nar::restore_path` (the gRPC path uses `Fetch` for the same flag).

`STDERR_NEXT` lines are unattributed `Log` events; `STDERR_START_ACTIVITY` of
type `actBuild` (105) is `Started` and maps the activity id to its drv;
`STDERR_RESULT` of type `resBuildLogLine` (101) or `resPostBuildLogLine`
(107) is a `Log` attributed through that map; `STDERR_ERROR` is decoded as
`readError` (message plus traces). `--timings` counts worker ops and round
trips (ops plus the three handshake exchanges) and bytes written to the
socket.

### Measured (`scripts/daemon-e2e.nu`, release build)

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

### Against gRPC (same client, same plan, same machine)

| case | gRPC (Have / Put / Build), v2 | worker protocol |
|---|---|---|
| custom drv, absent everywhere | 3 RPCs, 462 B, build 1.47 s | 6 ops / 9 round trips, 2.9 KB, build 0.87 s |
| hello cold upload | Have 0.017 s, Put 786 objects + 776 paths 2.0 MB 0.094 s | QueryValidPaths 0.008 s, AddMultipleToStore 776 paths 2.6 MB 0.247 s |
| hello warm | Have 0.011 s, 786 keys + 776 paths | QueryValidPaths 0.011 s, 776 paths, 62 KB |
| nixos-desktop cold upload | Have 0.078 s, Put 7,563 objects + 4,184 paths, 34.1 MB, 0.66 s | QueryValidPaths 0.022 s, AddMultipleToStore 4,184 paths, 40.4 MB, 1.89 s |
| nixos-desktop warm | Have 0.045 s | QueryValidPaths 0.034 s, 333 KB |

Same shape on both: one query, one stream, one build. The worker protocol
costs three more round trips (handshake, features, `SetOptions`) plus one for
the output map, and sends 18% more bytes (a `ValidPathInfo` with the
reference list per drv, NAR framing around each ATerm, and every source as
a whole NAR, against amber objects sent once). The upload is 3x slower on
the daemon side: the reference server writes objects to disk and validates
the trees at build time, the daemon hashes each NAR, parses each drv, and
inserts 4,184 rows in its SQLite database inside the op. The gRPC path
skips objects the builder has; the worker protocol sends whole NARs, which
matters for a source that changed by a few bytes. What the worker protocol
buys: any Nix >= 2.4 daemon is a builder
without deploying `ietsp-builderd`, and the paths land in a real store
that other Nix tools can read.

### Gaps

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

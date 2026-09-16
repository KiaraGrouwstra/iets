# Remote builder protocol

Status: v1 implemented on branch `remote`, 2026-09-16. Wire: gRPC over
HTTP/2 cleartext (tonic + prost), chosen for v1 because it is boring, tooled,
and matches nix-grpc-store and tribuchet. The protocol lives in
`proto/builder.proto` and is transport-agnostic; QUIC (iroh, weft) can carry
the same messages later.

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
      ->  Have(chunks, objects)              one request, one response
      ->  Put(stream of missing blobs)       one stream, content-addressed chunks
      ->  Build(root drv, wanted outputs)    one request; server streams events
      <-  events: scheduled, started, log, built, failed, done
```

Three RPCs, three round trips before the build starts (plus the TCP/HTTP2
connect). Everything the builder can look up by hash it looks up by hash.

## Objects (`proto/builder.proto`)

- `Digest { algo, bytes }`. v1 accepts sha256 only, for Nix compatibility;
  `BLAKE3` is reserved in the enum for a later version.
- `Chunk { digest, data }`: a slice of a NAR, at most 4 MiB, addressed by
  the sha256 of `data`.
- `Manifest { store_path, nar_hash, nar_size, chunks[], ca, references[] }`:
  a source tree. Addressed by its NAR hash. `ca` says how Nix derived the
  path: `SOURCE` (recursive sha256, no references: `./path`, `fetchTarball`)
  or `TEXT` (flat sha256 of one regular file plus references:
  `builtins.toFile`). The builder recomputes the path from the content and
  rejects a manifest that does not match, so it never trusts the client's
  path.
- `DrvBlob { drv_path, aterm }`: a derivation as ATerm bytes, addressed by
  sha256(aterm). The builder parses it and checks that the ATerm hashes to
  `drv_path`.
- `HaveRequest { chunks[], objects[] }` -> `HaveResponse { missing_chunks[],
  missing_objects[] }`. Two namespaces: a one-chunk NAR has chunk digest ==
  NAR hash, so a single list would conflate a chunk with its manifest.
- `PutRequest { chunk | manifest | drv }` client-streamed; `PutResponse {
  stored[] }`. Chunks precede their manifest, so the builder can check that
  every referenced chunk is present when the manifest arrives.
- `BuildRequest { root_drv_path, outputs[], keep_going, priority }` ->
  stream of `BuildEvent { Scheduled | Started | Log | Built | Failed | Done }`.
- `FetchRequest { store_path }` -> stream of `Chunk` (the NAR). Implemented
  on the server; the client does not call it yet.

## Client (`src/remote/`)

- `plan.rs`: from the root `Derivation` and a loader closure (drv path ->
  `Derivation`), walks `inputDrvs` recursively and collects `inputSrcs`.
  Sources are NAR-dumped and hashed (NAR hash plus per-chunk sha256) on a
  thread pool. Output: `Plan { objects }` in topological order, sources
  first, each drv after its inputs. A batch locator closure maps source
  store paths to (local path, addressing kind); `ietsp build` answers it
  with one `nix path-info --json` call, eval will answer from memory.
- `client.rs`: `build(url, plan, root, outputs, on_event)`. One `Have` with
  every digest. One `Put` stream: a blocking thread re-dumps each source
  whose chunks are missing and pushes chunks into a bounded channel while
  tonic sends them over HTTP/2 (dump and upload overlap). Then `Build`,
  forwarding events as they arrive. `--timings` prints each phase.
- `ietsp build [--builder URL] [--timings] [--out NAME]...
  [--source STOREPATH=LOCAL]... DRV`. `DRV` is a `.drv` store path, or a copy
  of the file named `<hash>-<name>.drv`; input drvs are read from
  `/nix/store`. `--source` names the original local tree of an input
  source, which is what eval will pass after the merge.

## Reference server (`ietsp-builderd --listen ADDR --store-dir DIR`)

Blobs live under `DIR/blobs/<sha256>`, manifests and drvs under
`DIR/objects/<sha256>` with a `DIR/paths/<store basename>` index. On `Build`:

1. Walk the closure of the root from the object store.
2. `nix-store --check-validity --print-invalid <all paths>`: one call tells
   which paths the local Nix store lacks.
3. For each missing source, restore the NAR from its chunks into a temp dir
   (`store::nar::restore_path`, the byte-exact inverse of `dump_path`) while
   hashing the stream; check the NAR hash, size, and that the content yields
   the planned store path. Write each missing drv's ATerm to a file.
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

## Measured (`scripts/remote-e2e.sh`, release build, localhost)

| case | plan | have | put | build (realise) |
|---|---|---|---|---|
| 1 file tree + 1 drv, both absent on the builder and deleted from the store | 0.001 s | 3 digests, 3 missing, 0.001 s | 2 objects, 1 chunk, 1.6 KB, 0.002 s | 1.45 s |
| hello (616 drvs, 160 sources), cold builder | 0.171 s | 936 digests, 936 missing, 0.010 s | 776 objects, 160 chunks, 2.0 MB, 0.102 s | 0.38 s |
| hello, warm builder | 0.142 s | 936 digests, 0 missing, 0.006 s | skipped | 0.48 s |

The plan phase is dominated by `nix path-info --json` on 160 paths
(about 0.11 s); the NAR dump and hashing of 2 MB take the rest. The
"build" column is `nix-store --realise` on an already-built closure, so it
measures the Nix side: validity check, registration (a no-op here), and
the realise itself. Case 1 includes the actual sandbox build.

Round trips per build: 1 connect + `Have` + `Put` + `Build` = 4, and `Put`
is skipped on a warm builder. Messages for the cold hello build: 1 `Have`
request with 936 digests, 936 `Put` messages in one stream (160 chunks, 160
manifests, 616 drvs), 1 `Build` request, and a handful of events back.

## Compared with the Nix worker protocol (estimate)

No ssh loopback is configured on this machine, so this is read from
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
sends full NARs; this protocol sends 4 MiB chunks and skips the ones the
builder has.

## Not in v1

- Auth. Next step is mTLS on the tonic channel; the reference server binds
  to localhost cleartext.
- Retries, resume, and backpressure beyond HTTP/2 flow control.
- `Fetch` on the client: outputs stay on the builder.
- Input sources other than `SOURCE` and `TEXT`: flat fixed-output paths
  (`nix-store --add-fixed`) and output paths used as `inputSrcs`
  (`builtins.storePath`) are rejected at plan time.
- `outputs`, `keep_going`, `priority` in `BuildRequest` are carried but the
  reference server builds every output of the root.
- Signatures on `Built` events, blob garbage collection, concurrent-build
  locking on the server (the daemon serializes conflicting registrations).
- The benchmark from the draft (nixos-desktop cold and warm against
  `nix build --builders ssh-ng://`) waits for the eval wiring.

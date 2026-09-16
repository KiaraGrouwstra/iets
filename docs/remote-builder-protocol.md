# Remote builder protocol (draft)

Status: draft, 2026-09-16. Decisions marked OPEN need Jonas.

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
      ->  Plan: closure of the root drv: drvs, input sources, fixed-output drvs
      ->  Have(hashes)                       one request, one response
      ->  Put(stream of missing blobs)       one stream, content-addressed chunks
      ->  Build(root drv, wanted outputs)    one request; server streams events
      <-  events: scheduled, started, log chunk, output built, done
```

Four calls, three round trips before the build starts. Everything the
builder can look up by hash it looks up by hash.

## Objects

- **Blob**: bytes addressed by BLAKE3 (or sha256 for Nix compatibility;
  OPEN). Chunks of at most 4 MiB. A file, a NAR, or a .drv is a blob or a
  Merkle list of chunks.
- **Source tree**: the NAR of a path, chunked. Its Nix store path is
  derived by the server from the NAR hash with the Nix rules, so the
  server can register it without trusting the client's path.
- **Derivation**: ATerm bytes, sent as a blob, referenced by its Nix drv
  path so the server can hand it to a Nix daemon unchanged.
- **Realisation**: (drv path, output name) -> output store path, with the
  builder's signature.

## Calls

```
Have(Request { hashes: [Hash] }) -> Response { missing: [Hash] }
Put(stream ChunkOrTree) -> Ack { stored: [Hash] }
Build(Request { root: DrvPath, outputs: [String], keep_going, priority })
    -> stream Event { Scheduled | Started{drv} | Log{drv, bytes}
                    | Built{drv, output, path, sig} | Failed{drv, reason} | Done }
Fetch(Request { path }) -> stream Chunk         # optional, for pulling outputs
```

## Wire (OPEN)

Options that fit the existing fleet:

1. gRPC over HTTP/2 with mTLS. Matches nix-grpc-store and tribuchet.
   Boring, tooled, fast enough. Recommended for the first version.
2. QUIC streams over iroh or weft. Matches amber-store and subportal. NAT
   safe, key pinned. Better long term; more glue now.

The protocol is defined once in protobuf and carried by either.

## Server

`ietsp-builderd`: a reference server that keeps blobs in a directory,
imports source trees and drvs into a local Nix store through the daemon,
runs `nix-store --realise` (or the daemon build op), and streams events.
Small, for tests and benchmarks; not the product. A production server
would sit on tribuchet or amber.

## Benchmark for phase B

Cold: nixos-desktop closure, empty builder, measure time to first build.
Warm: same, builder has everything, measure time from `ietsp build` to
`Done`. Compare `nix build --builders ssh-ng://...` and
`nix copy` + remote `nix-build`.

## Not in scope

Local builds, sandboxing, substituters on the client, signing keys on the
client, the eval cache.

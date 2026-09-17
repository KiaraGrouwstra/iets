# Proposal: an eval cache layer on the Nix binary cache

Status: proposal, 2026-09-17. Nothing implemented.

## The claim

A binary cache answers "what are the bytes of store path P". It cannot
answer "what is P for attribute A of source S", so every consumer
evaluates S to find out, and evaluation is the slow part of every deploy,
every `nix run`, every CI job. The producer already knows the answer: the
CI that built P evaluated A of S to get there. This layer publishes that
answer next to the NAR, keyed by content, signed by the same key.

It is not the flake eval cache. That one is a local sqlite write cache,
attribute-granular, keyed by a fingerprint, and it misses on every edit.
This one is read-only for consumers, produced once by whoever evaluates
anyway, and keyed by the immutable identity of the source.

## Key

Everything a consumer needs to find its shard is content it already has:

```
source   = narHash of the locked flake source (lock folded in) or the nixpkgs tarball
system   = x86_64-linux
attrpath = nixosConfigurations.host.config.system.build.toplevel
shard    = first 10 bits of sha256(attrpath)
```

Two machines with the same checkout compute the same location without
talking to anyone; a git-clean checkout gets its narHash from the tree
object. Pure evaluation is a function of source, system and attrpath,
so the lookup is sound without a read set. Impure inputs (`currentTime`,
`getEnv`, fetchers, `--impure`) are never published.

## Measured (2026-09-17)

Data: `ci/eval/outpaths.nix` for x86_64-linux at two nixos-unstable
commits three weeks apart (2026-08-23 `56c02bc`, 2026-09 `a831408e`),
80,245 and 80,314 jobs. Cache timings from this datacenter box; a laptop
on wifi sees a tenth of the bandwidth and two to four times the latency.

| artifact | raw | zstd -19 |
|---|---|---|
| nix-eval-jobs lines with `meta` | 237 MB | 12.1 MB |
| attr, drvPath, outputs, system as JSON | 23.9 MB | 5.5 MB |
| attr, drv hash, output names and hashes | 11.8 MB | 4.5 MB |
| same in CBOR with 20-byte binary hashes (computed) | 8.5 MB | about 4.3 MB |
| delta between the two commits: 4,316 changed, 72 added, 3 removed | 0.9 MB | 0.27 MB |

The floor is entropy: 80k drv hashes plus 126k output hashes are 4.1 MB
of incompressible bytes. Encoding choice moves the full index by 5 %.
Only the delta and the shard shape move it by an order of magnitude.

| transfer | cache.nixos.org | a builder on the local network |
|---|---|---|
| time to first byte | 55 to 150 ms | 3 ms |
| bandwidth | 133 MB/s here; 5 to 20 MB/s on a laptop | 2.5 MB/s (tunnel) |

What a lookup competes against (this box):

| consumer | ietsp eval | nix eval | ietsp tracing-cache hit |
|---|---|---|---|
| `nixpkgs#hello` at a channel | 0.10 s | 0.4 s | 0.03 s |
| a NixOS system configuration | 1.0 s | 4 to 19 s | 0.03 s |
| whole nixpkgs, one system | 70 s | 250 s | none |

## Trade-off

Three object shapes, each right for one consumer:

1. **Shard.** Attrs bucketed by the first 10 bits of sha256(attrpath):
   1,024 shards of about 4.5 KB compressed, one COSE_Sign1 CBOR map
   `attr -> [drvhash, [[output, hash]...]]` each. One lookup is one GET
   of a few KB: 60 to 150 ms on a public cache, 3 ms on the local network. That
   beats evaluation for everything except a warm local tracing-cache hit,
   which nobody publishes anyway. Publishing a commit re-PUTs only the
   shards that changed: a 100-attr commit touches under 100 shards; the
   three-week delta above touched most of them once. A per-attr object
   (about 120 bytes) is not better than this: it saves nothing on the
   wire and costs 80k PUTs per commit.
2. **Delta.** For bulk consumers (CI compare, search, a channel mirror):
   a full index once per source line, then one delta per commit,
   `[attr, drvhash, outputs]` for changed and added attrs and `[attr]`
   for removed. 270 KB for three weeks of nixos-unstable; a single
   commit is usually under 10 KB, a mass rebuild is a new full index.
   The consumer folds deltas locally into its copy of the index. This
   is a mirror of published, signed, content-keyed data, not an
   evaluation cache: nothing local is derived.
3. **Full index.** 4.3 MB, published so a delta chain has a base. Never
   fetched for a single attr; on a laptop it costs as much as
   evaluating.

Dropped from the first draft: the standalone per-attr `evalinfo`
object. The shard replaces it.

## Encoding

CBOR, deterministic encoding (RFC 8949 4.2), hashes as 20-byte strings,
one COSE_Sign1 envelope (RFC 9052, ed25519) per shard, per delta, and
per full index, signed with the cache key. Shards are maps keyed by the
attrpath string. Deltas and the full index are CBOR sequences so a
consumer can stream them. `ietsp eval-cache show URL` prints any of
them as JSON for humans. The compressed size is within 5 % of JSON; the
reasons are canonical signing without a canonicalization spec, and one
decoder shared with amber, whose directory objects are already CBOR.

## Layout

```
eval/v1/<source narHash>/<system>/shard/<10-bit prefix as 3 hex>   COSE(CBOR map)
eval/v1/<source narHash>/<system>/index                            COSE(CBOR seq)
eval/v1/<source narHash>/<system>/delta/<previous source narHash>  COSE(CBOR seq)
```

The source narHash is the locked flake source or the nixpkgs tarball;
the lock's narHash is folded into it for flakes (hash of both). Args
other than the system are not part of v1: jobsets are published as the
producer evaluated them.

## Producer

`ietsp eval-jobs --publish DIR` writes the shards, the full index, and,
given `--delta-from DIR`, the delta; the directory is shipped with the
same `nix copy` or rsync the NARs use. Cost on top of the evaluation:
under a second of CPU, 1,024 small PUTs for a full publish.

## Consumer

`ietsp build`, `print-dev-env`, and `eval` take `--eval-cache URL`:
compute the source narHash and the attrpath, GET the shard, verify the
signature, use the drv and output hashes. Substitutable outputs are
realised; otherwise the `.drv` closure is fetched from the same cache
and built. A miss, a bad signature, or a dirty checkout falls through to
evaluation. Bulk tools (`ietsp eval-jobs --compare`, a search index)
take `--eval-cache URL` too and use index plus deltas.

## Trust

The same key that signs "P has these bytes" signs "A of S is P". A
consumer that trusts the cache for binaries already trusts the producer
to have built the right thing; the evalinfo is a weaker statement than
the NAR. Unsigned evalinfos are ignored, like unsigned narinfos under
`require-sigs`. A malicious cache can already substitute any output; it
gains nothing new.

## Where it plugs in

- harmonia and niks3: one more route or S3 prefix; they never read the
  objects. nix-grpc-store: same.
- an amber-backed Nix daemon: an evalinfo maps naturally to a realisation-like
  record keyed by source, system and attrpath; the index is a blob.
- Nix itself: nothing changes. Consumers that want it use ietsp or a
  small wrapper. If it proves out, the format is simple enough to
  propose upstream as a `nix build --eval-cache-url` feature.

## What it does not do

- Partial results. A miss is a full local evaluation. Attribute-level
  sharing within one evaluation is the flake eval cache, and it stays
  rejected.
- Dev loops. A dirty checkout has no published key. ietsp's tracing
  cache covers that case locally and stays as is.
- Reproducibility proofs. An evalinfo says what the producer got, not
  that another evaluator would agree. `ietsp eval-jobs` differential
  runs against nix-eval-jobs are how we keep that true.

## First milestone

1. Producer in `ietsp eval-jobs`: `--publish DIR` writing evalinfos and
   the index to a directory that `nix copy`/rsync ships to the cache.
2. Consumer in `ietsp build` and `print-dev-env`: `--eval-cache URL`,
   hit path only, falling back to evaluation.
3. Measure a NixOS system switch with a hit versus today.
4. Then the nixpkgs index for one channel commit, and `ietsp build
   nixpkgs#hello` against it.

About two agent-days. The format decisions above are the ones worth
arguing about before that: the key fields, one object per attr versus
the index, and whether `.drv` NARs are published by default.

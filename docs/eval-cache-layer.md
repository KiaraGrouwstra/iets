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

```
evalkey = sha256(
  "eval:1",
  source narHash            # the locked flake source, or a nixpkgs tarball
  lock narHash              # flake.lock contents; empty for a plain tree
  system
  attrpath                  # "nixosConfigurations.nv1.config.system.build.toplevel"
  args                      # canonical JSON of --arg/--argstr, usually {}
)
```

Everything in the key is content. Two machines with the same checkout
compute the same key without talking to anyone; a git-clean checkout
gets its narHash from the tree object. Pure evaluation is a function of
these inputs, so the key is sound without a read set. Impure inputs
(`currentTime`, `getEnv`, fetchers, `--impure`) are simply never
published.

## Objects

Stored beside `<hash>.narinfo` in the same bucket or served by the same
daemon, under one prefix:

```
eval/<evalkey>.evalinfo
  Key: <evalkey>
  Source: <source narHash>
  Attr: nixosConfigurations.nv1.config.system.build.toplevel
  System: x86_64-linux
  DrvPath: /nix/store/...-nixos-system-nv1.drv
  Outputs: out=/nix/store/...-nixos-system-nv1
  Sig: hairesis-cache-1:...
```

Plus, optionally, an index per source for whole jobsets:

```
eval/index/<source narHash>/<system>.jsonl.zst
  one nix-eval-jobs line per attr, exactly what `ietsp eval-jobs` prints
```

The `.drv` files themselves are ordinary store paths: publish their NARs
like any other path when a consumer may need to build. A consumer that
only substitutes never touches them.

## Producer

`ietsp eval-jobs --publish s3://... --sign KEY` (or `ietsp build
--publish`): for every job line, write the evalinfo, sign it, and push
the `.drv` closure through the normal `nix copy` path. Hydra, nixpkgs
CI, and a flake's own CI already run the evaluation; this adds one
object per job to what they upload. For the nixpkgs channels the index
is the interesting artifact: 80k lines, about 10 MB compressed, one
file per commit and system.

## Consumer

```
ietsp build .#nixosConfigurations.nv1...   (or nix build via a wrapper)
  1. key from the checkout                   ~1 ms, no evaluation
  2. GET eval/<key>.evalinfo                 one round trip
  3. hit: outputs known
       substitutable? -> realise by substitution, done
       else           -> fetch the .drv closure, build as usual
  4. miss: evaluate locally (ietsp: ~1 s), optionally publish
```

On a hit the machine never parses nixpkgs. `hai apply` on nv1 becomes:
key, one GET, `nix-store --realise` of a path that bld1 already holds.
`nix run nixpkgs#hello` at a channel commit becomes one GET plus the
existing narinfo lookups.

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
- amber-nix-daemon: an evalinfo maps naturally to a realisation-like
  record keyed by evalkey; the index is a blob.
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
3. Measure on ~/home: `hai apply` on nv1 with a hit versus today.
4. Then the nixpkgs index for one channel commit, and `ietsp build
   nixpkgs#hello` against it.

About two agent-days. The format decisions above are the ones worth
arguing about before that: the key fields, one object per attr versus
the index, and whether `.drv` NARs are published by default.

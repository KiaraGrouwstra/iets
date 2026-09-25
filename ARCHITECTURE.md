# ietsp architecture

A map of the crate. The reasoning behind each part is in
`docs/explanation/`; this file says what exists and where it lives.

ietsp is a from-scratch Nix language evaluator in Rust. It parses Nix,
evaluates lazily, computes derivations and store paths in memory, and
prints results. It writes no store of its own: builds go to a Nix daemon or
to a nix-grpc-store service, and a flake input it fetches itself lives in
its own cache.

## Goals, in order

1. Byte-exact parity with `nix-instantiate --eval --strict` on the
   `.drvPath` of NixOS systems from nixpkgs. The judge is
   `scripts/difftest.nu`.
2. Faster than Nix on `bench/workloads/nixos-desktop.nix` with the pinned
   nixpkgs. Sequential speed first, parallel forcing second.
3. Small, readable code. One crate, modules by phase.
4. The fastest client to a remote builder. Evaluate in memory, ask the
   store what it lacks, send only that in one operation, build.

## Non-goals

Local store writes, local builds, a builder daemon of our own, unlocked
flakes (registries, `nix flake lock`), fetchers that hit the network (a
locked flake input is the one exception), a REPL, a debugger, a bytecode
VM.

nix-grpc-store (github.com/Mic92/nix-grpc-store) is the supported gRPC
transport, spoken natively from its vendored `proto/nix_remote.proto`. Its
daemon is somebody else's daemon, which is the point: ietsp is a client of
it, not a replacement for it.

## Pipeline

```
source bytes
  -> syntax::lexer / syntax::parser     -> ast::Ast (per file, arena of Expr)
  -> compile                            -> ir::Ir (scopes resolved, attrpaths merged)
  -> eval                               -> runtime::Word (lazy graph, forced on demand)
  -> print / json / drvPath
```

Why each stage is shaped as it is: `docs/explanation/evaluation.md`.

## Modules

The evaluator:

- `syntax/`: `symbol.rs` (interner), `ast.rs`, `lexer.rs`, `parser.rs`,
  `dump.rs` (`ietsp parse`). The parser keeps raw bindings; attribute-path
  merging and scope resolution happen in `compile`.
- `compile/`: AST to IR (`ir.rs`). Every variable becomes a local slot or a
  `with` lookup. `cache.rs` is the on-disk IR cache.
- `runtime/`: `arena.rs` (bump allocator per worker, no GC), `word.rs`
  (8-byte tagged word), `value.rs` (heap objects), `census.rs` (the memory
  census).
- `eval/`: the tree-walking evaluator over IR — `force`, apply, the `with`
  chain, operators, equality, `coerce.rs`, `error.rs`.
- `builtins/`: primops, one file per family.
- `parallel/`: worker threads, fibers, work-stealing deques, the thunk
  claim protocol.
- `store/`: pure functions. Nix base32, the four hash algorithms, NAR
  serialization and hashing, store paths, ATerm derivations,
  `hashDerivationModulo`, `drvdiff.rs`; `locate.rs` finds derivation roots
  and local-store sources, `source.rs` holds where the bytes of a path are
  and the origin each source came from.
- `corepkgs/`: `derivation.nix` and `fetchurl.nix`, embedded from Nix
  2.34.8.
- `print.rs`: the `nix-instantiate --eval` printer, and JSON.

Around it:

- `flake/`: locked flakes. Nix's `call-flake.nix` embedded verbatim,
  `fetchTree` for locked inputs.
- `source/`: a reference resolves to a tree and a lock. The fetchers and
  the input store are behind it.
- `fetch/`: the HTTP client, the tarball spool and the unpack rules.
- `treestore/`: the input store, where a fetched input lives.
- `trace/`: the tracing evaluation cache.
- `pure-eval`: `Evaluator::pure`, fixed by `Evaluator::with`, checked in
  `builtins::store_hooks`.
- `cachefs.rs`: the on-disk hygiene all three stores share.
- `lint/`: the lint registry, level resolution, and one file per family.
- `remote/`: the store transports. `plan.rs` (the upload plan), `worker/`
  (the Nix worker protocol), `grpc/` (nix-grpc-store), `copy.rs`,
  `event.rs`.
- `build/`: the dispatcher every build goes through, its two backends, the
  uploaders, the renderer and the result files.
- `jobs/`: the eval-jobs walk.
- `cli/search.rs`: the package search and its index.
- `devenv/`: `print-dev-env`.
- `provenance.rs`: store paths and derivations back to local trees and
  source positions.
- `config.rs`: `nix.conf`. `settings.rs`: `ietsp.conf` and every `IETSP_*`
  variable.
- `nixbin.rs`: the real Nix binaries behind the classic fallback.
- `error.rs`: the error type outside the evaluator.
- `api.rs`: the library facade. Frontends use this and never reach into
  `cli`; `examples/` holds four of them.
- `main.rs` and `cli/`: the command line on top of `api`, including the
  classic names in `cli/classic/`.

## The layer rule

A capability is implemented once, below the command line, and a porcelain
command is written against it. `tests/layering.rs` enforces it: no file
under `src/cli/` may name `crate::eval`, `crate::store`, `crate::build`,
`crate::remote`, `crate::flake`, `crate::jobs`, `crate::devenv`,
`crate::trace`, `crate::compile` or `crate::parallel` outside a per-file
allow-list whose length is asserted.

What the rule does *not* ban is porcelain-side libraries for presentation,
argv compatibility and interactive flows, however large.

Why, and what the migration to it measured:
`docs/explanation/plumbing-and-porcelain.md`.

## Where the explanations are

| question | page |
|---|---|
| How does evaluation work, and why is parallel evaluation safe? | `docs/explanation/evaluation.md` |
| What is on disk, and why is the input store safe without a daemon? | `docs/explanation/caching.md` |
| How does a flake reference become bytes? | `docs/explanation/flakes-and-sources.md` |
| Why is there a plumbing layer, and what did it cost? | `docs/explanation/plumbing-and-porcelain.md` |
| Why trace a store path back to a file? | `docs/explanation/provenance.md` |
| What was tried and removed? | `docs/explanation/history.md` |

Reference material — every command, setting, error code, lint, builtin,
divergence and library entry point — is under `docs/reference/`.

## Testing

`cargo test` for the units; `nix flake check` adds clippy, fmt, the
examples, `scripts/langtests.nu`, `scripts/partest.nu`,
`scripts/diff-e2e.nu` and the NixOS module. The other scripts need a live
`nix-daemon` or `nix` and run by hand in the dev shell; `scripts/lib.nu`
holds what they share.

| script | what it compares |
|---|---|
| `langtests.nu` | Nix 2.34.8's functional tests, `tests/lang/` |
| `corpus.nu` | `tests/corpus/` against `.exp` from `nix-instantiate` (`--update` rewrites them) |
| `parity.nu` | `match`, `split`, `fromTOML` against the real `nix-instantiate` |
| `partest.nu` | 1 against N workers |
| `cachetest.nu` | the tracing cache's hits and misses |
| `difftest.nu`, `drvdiff.nu` | one expression against `nix-instantiate`; the first differing derivation |
| `diff-e2e.nu` | `ietsp diff` on two small derivations |
| `classic-e2e.nu` | the classic names against the real nix, stdout byte for byte |
| `daemon-e2e.nu`, `grpc-e2e.nu` | `ietsp build` over the worker protocol and over `grpc://` |
| `fastbuild-e2e.nu` | `ietsp fast-build`, and against `nix-fast-build` |
| `flaketest.nu` | flake evaluation and the input store against `nix eval` |
| `prefetch-e2e.nu` | `ietsp prefetch` against `nix store prefetch-file` and `nix hash` |
| `devenv-test.nu` | `print-dev-env` against `nix print-dev-env` |
| `provenance-e2e.nu` | the rewritten paths and positions |
| `evaljobs-flags.nu`, `evaljobs-cmp.nu` | `ietsp eval-jobs` against `nix-eval-jobs` |

`bench/` holds the workloads and the runners (`bench/run.nu`, `ab.nu`,
`perf.nu`, `eval-jobs.nu`, `nar.nu`, `cache-growth.nu`).

## Conventions

- Rust 2024. Every `unsafe` block names the invariant it relies on.
- No comments that restate code. Comments explain why.
- `cargo clippy` clean, `cargo fmt`.
- Commit small, descriptive messages.

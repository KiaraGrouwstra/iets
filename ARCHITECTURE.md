# ietsp architecture

ietsp is a from-scratch Nix language evaluator in Rust. It parses Nix,
evaluates lazily, computes derivations and store paths in memory, and prints
results. It writes no store of its own: builds go to a Nix daemon or a
nix-grpc-store service (`docs/remote-build.md`), and a flake input it fetches
itself lives in its own cache (`docs/flakes.md`).

## Goals, in order

1. Byte-exact parity with `nix-instantiate --eval --strict` on `.drvPath`
   of NixOS systems from nixpkgs. The judge is `scripts/difftest.nu`.
2. Faster than fix and Nix on `bench/workloads/nixos-desktop.nix` with the
   pinned nixpkgs. Sequential speed first, parallel forcing second.
3. Small, readable code. One crate, modules by phase.
4. The fastest client to a remote builder. Evaluate in memory, ask the
   store what it lacks, send only that in one op, build.

## Non-goals

Local store writes, local builds, a builder daemon of our own, unlocked
flakes (registries, `nix flake lock`), fetchers that hit the network (they
error; a locked flake input is the one exception, see `docs/flakes.md`), a
REPL, a debugger, a bytecode VM.

nix-grpc-store (github.com/Mic92/nix-grpc-store) is the supported gRPC
transport, spoken natively from its vendored `proto/nix_remote.proto`. Its
`nix-grpc-daemon` is somebody else's daemon, which is the point: we are a
client of it, not a replacement for it.

## Pipeline

```
source bytes
  -> syntax::lexer / syntax::parser     -> ast::Ast (per file, arena of Expr)
  -> compile                            -> ir::Ir (scopes resolved, attrpaths merged)
  -> eval                               -> runtime::Word (lazy graph, forced on demand)
  -> print / json / drvPath
```

## Modules

The evaluator:

- `syntax/`: `symbol.rs` (interner, `Sym` = u32), `ast.rs`, `lexer.rs`,
  `parser.rs`, `dump.rs` (`ietsp parse`). Parser output keeps raw bindings;
  attrpath merging and scope resolution happen in `compile`. Variable-length
  node payloads live in side tables of the `Ast` addressed by `Range`, so a
  file costs nine allocations instead of one per node.
- `compile/`: AST -> IR (`ir.rs`). Every variable becomes
  `Local { depth, index }` or `WithVar(sym)` (only when no lexical binder
  exists). Static attrset bindings are sorted by `Sym` id. Literal folding.
  `cache.rs`: the compiled IR of every imported file, on disk
  (docs/memory.md, "IR cache").
- `runtime/`: `arena.rs` (bump allocator per worker, no GC), `word.rs`
  (8-byte tagged `Word`), `value.rs` (heap objects), `census.rs` (the
  `stats2` memory census).
- `eval/`: tree-walking evaluator over IR: `force`, apply, `with` chain,
  operators, equality, `coerce.rs`, `error.rs` (errors and traces).
- `builtins/`: primops, one file per family (`docs/builtins-api.md`).
- `parallel/`: worker threads, fibers, work-stealing deques, the thunk claim
  protocol (`docs/parallel.md`).
- `store/`: pure functions. Nix base32, the four hash algorithms, NAR
  serialization and hashing (`docs/nar.md`), store paths, ATerm derivations,
  `hashDerivationModulo`; `locate.rs` finds derivation roots and local-store
  sources, `source.rs` holds `SrcPath` (where the bytes of a path are: disk,
  input-store spool or memory; read by `builtins::store_hooks`) and the
  `SourceOrigin` each source came from.
- `corepkgs/`: `derivation.nix`, `fetchurl.nix` embedded from Nix 2.34.8.
- `print.rs`: the `nix-instantiate --eval` printer and JSON.

Around it:

- `flake/`: locked flakes. `PATH#attr` on the command line, Nix's
  `call-flake.nix` embedded verbatim, `fetchTree` for locked inputs, the
  HTTP client and the tarball spool (`docs/flakes.md`).
- `inputs.rs`: the input store, where a fetched flake input lives.
- `trace/`: the tracing evaluation cache (`docs/tracing-cache.md`).
- `pure-eval`: `Evaluator::pure`, fixed by `Evaluator::with`, checked by
  `builtins::store_hooks::open` (`docs/pure-eval.md`).
- `lint/`: the lint registry (name, family, hook, default level, doc), the
  level resolution (`ietsp.toml`, `IETSP_LINT`, `--lint`), the emission with
  per-site deduplication, and one file per family with the triggers the
  parser, compiler and evaluator call (`docs/lints.md`).
- `cachefs.rs`: on-disk hygiene both caches share (`docs/caches.md`).
- `remote/`: the store transports. `plan.rs` (the upload plan), `worker/`
  (the Nix worker protocol), `grpc/` (nix-grpc-store), `copy.rs` (a closure
  between two worker stores), `event.rs` (what a daemon reports).
- `build/`: the dispatcher every build goes through, its two backends, the
  uploaders, the renderer and the result files (`docs/remote-build.md`).
- `jobs/`: the eval-jobs walk (`docs/eval-jobs.md`).
- `devenv/`: `print-dev-env` (`docs/direnv.md`).
- `provenance.rs`: store paths and derivations back to local trees and
  source positions (`docs/provenance.md`).
- `config.rs`: nix.conf; `settings.rs`: ietsp.conf and every `IETSP_*`
  variable (`docs/config.md`, `docs/settings.md`).
- `nixbin.rs`: the real Nix binaries behind the classic fallback.
- `error.rs`: the error type outside the evaluator.
- `api.rs`: the library facade. Frontends use this and never reach into
  `cli`; `examples/` holds three of them (`docs/library.md`).
- `main.rs` and `cli/`: the command line on top of `api`: `ietsp eval`,
  `build`, `eval-jobs`, `fast-build`, `print-dev-env`, `parse`,
  `lint`, `cache gc`, `cache dump`, the help texts (`cli/help.rs`), and
  the classic names (`cli/classic/`, `docs/classic.md`).

## Value model

`Word` is one 8-byte word with a 3-bit tag in the low bits.

| tag | meaning |
|---|---|
| INT | 61-bit immediate integer; out-of-range ints box to a heap object |
| PTR | pointer to a heap object with a one-byte kind header |
| THUNK, THUNK_APP, THUNK_APP2 | pointer to a 16- or 24-byte thunk object, not yet evaluated |
| THUNK_MAP | a `mapAttrs` entry: descriptor pointer plus entry index, no object |
| LAMBDA | pointer to a two-word closure |
| BLACKHOLE | thunk under evaluation (recursion detection, parallel claim) |
| NULL, TRUE, FALSE | immediates |

Heap kinds: `Float`, `BigInt`, `String { bytes, context }`, `Path`, `List
{ [Cell] }`, `Attrs { [(Sym, Pos, Cell)] sorted by Sym id }`, `AttrsUnion`
(the result of `//`: flat leaves, no copy), `Lambda { env, ir }`,
`PrimOp`, `PrimOpApp`, `Env { up, [Cell] }`, `Thunk { code, state }`.

A `Cell` is an `AtomicU64` holding a `Word`. Env slots, list elements, and
attr values are cells. Forcing a thunk overwrites the thunk object in
place: its `code` word (IR or function) becomes the value and its `state`
word (env or argument) becomes EMPTY; cells that held the thunk pointer
are rewritten with the value as they are read (graph reduction). Values
are immutable after forcing. See docs/memory.md for the layouts.

Attrs are sorted by `Sym` id, like C++ Nix. Name order (for `attrNames`,
printing, JSON, derivation env) is computed on demand.

Strings carry context: `Opaque(store path)`, `DrvDeep(drv path)`,
`Built { drv path, output name }`. Context flows through `+`, interpolation,
and `concatStringsSep`. `derivationStrict` turns context into inputDrvs and
inputSrcs.

## Laziness

- Every non-cheap argument, attr value, list element, and let binding
  becomes a thunk. Cheap expressions (literal, variable) reuse the existing
  word or cell, so a variable that holds a thunk shares that thunk pointer
  instead of allocating a new one.
- `force(cell)`: load word; if THUNK, store BLACKHOLE, evaluate, store the
  result; if BLACKHOLE, error "infinite recursion".
- `with`: the evaluator keeps a chain of with-scopes in the env. `WithVar`
  walks the chain innermost first. Lexical bindings always win.

## Errors

`Result<Word, Box<Error>>`. `tryEval` catches `Throw` and `Assert` only;
a lint at level `error` is kind `Lint` and escapes it (docs/lints.md).
Errors carry a message, a span, and a trace. Position information is kept
per IR node for `unsafeGetAttrPos`, lambda positions, and error messages.

## Parallelism

Cells are atomic, so the sequential path and the parallel one share one
shape. Worker threads, work-stealing deques, stackful fibers that park on a
BLACKHOLE claimed by another fiber, and strict-demand fan-out on lists and
attrsets; `--workers 1` is the sequential path. See `docs/parallel.md`.

## Testing

`cargo test` for the units; `nix flake check` adds clippy, fmt, the
examples, `scripts/langtests.nu`, `scripts/partest.nu` and the NixOS module.
The other scripts need a live `nix-daemon` or `nix` and run by hand in the
dev shell (`scripts/lib.nu` holds what they share):

| script | what it compares |
|---|---|
| `langtests.nu` | Nix 2.34.8's functional tests, `tests/lang/` |
| `corpus.nu` | `tests/corpus/`, our own cases against `.exp` from `nix-instantiate` (`--update` rewrites them) |
| `parity.nu` | `match`, `split`, `fromTOML` against the real `nix-instantiate` (`tests/parity/`) |
| `partest.nu` | 1 against N workers |
| `cachetest.nu` | the tracing cache's hits and misses |
| `difftest.nu`, `drvdiff.nu` | one expression against `nix-instantiate`; the first differing derivation |
| `classic-e2e.nu` | the classic names against the real nix, stdout byte for byte |
| `daemon-e2e.nu`, `grpc-e2e.nu` | `ietsp build` over the worker protocol and over `grpc://` |
| `fastbuild-e2e.nu` | `ietsp fast-build`, and against `nix-fast-build` |
| `flaketest.nu` | flake evaluation and the input store against `nix eval` |
| `devenv-test.nu` | `print-dev-env` against `nix print-dev-env` |
| `provenance-e2e.nu` | the rewritten paths and positions |
| `evaljobs-flags.nu`, `evaljobs-cmp.nu` | `ietsp eval-jobs` against `nix-eval-jobs`; two saved outputs |

`bench/` holds the workloads and the runners (`bench/run.nu`, `ab.nu`,
`perf.nu`, `eval-jobs.nu`, `nar.nu`, `cache-growth.nu`).

## Conventions

- Rust 2024. Every `unsafe` block names the invariant it relies on.
- No comments that restate code. Comments explain why.
- `cargo clippy` clean, `cargo fmt`.
- Commit small, descriptive messages.

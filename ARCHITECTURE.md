# ietsp architecture

ietsp is a from-scratch Nix language evaluator in Rust. Scope: evaluation
only. It parses Nix, evaluates lazily, computes derivations and store paths
in memory, and prints results. It never writes to a store.

## Goals, in order

1. Byte-exact parity with `nix-instantiate --eval --strict` on `.drvPath`
   of NixOS systems from nixpkgs. The judge is `scripts/difftest.nu`.
2. Faster than fix (1.46 s) and Nix on `bench/workloads/nixos-desktop.nix`
   with the pinned nixpkgs. Sequential speed first, parallel forcing second.
3. Small, readable code. One crate, modules by phase.

4. Phase B: the fastest possible client to a remote builder. Evaluate in
   memory, ask the builder what it lacks, stream only that, wait for the
   root. This replaces the Nix worker protocol, which stays as a fallback
   transport to any `nix-daemon`; see `docs/remote-builder-protocol.md`.

## Non-goals

Local store writes, local builds, the Nix worker protocol as the primary
transport (it is the fallback: `docs/remote-builder-protocol.md`), unlocked
flakes (registries, `nix flake lock`), fetchers that hit the network (they
error; a locked flake input missing from `/nix/store` is the one exception,
see `docs/flakes.md`), a REPL, a debugger, a bytecode VM.

## Pipeline

```
source bytes
  -> syntax::lexer / syntax::parser     -> ast::Ast (per file, arena of Expr)
  -> compile::resolve                   -> ir::Ir (scopes resolved, attrpaths merged)
  -> eval                               -> runtime::Word (lazy graph, forced on demand)
  -> print / json / drvPath
```

## Modules

- `syntax/`: `symbol.rs` (interner, `Sym` = u32), `ast.rs` (shared AST,
  written first, do not change shape without updating compile), `lexer.rs`,
  `parser.rs`. Parser output keeps raw bindings; attrpath merging and
  scope resolution happen in `compile`. Variable-length node payloads
  (attrpaths, string parts, bindings, arguments, formals, literal bytes)
  live in side tables of the `Ast` addressed by `Range`; the parser
  builds them on scratch stacks and commits each run when its parent
  completes, so a file costs nine allocations instead of one per node.
- `compile/`: AST -> IR. Every variable becomes `Local { depth, index }`
  or `WithVar(sym)` (only when no lexical binder exists). Attrset bindings
  are sorted by `Sym` id at compile time when static. Literal folding.
  Records "cheap" expressions (literals, variables) so the evaluator can
  skip thunk allocation.
- `runtime/`: `arena.rs` (bump allocator, no GC, per-thread later),
  `word.rs` (8-byte tagged `Word`), `value.rs` (heap objects: string with
  context, list, attrs, lambda, primop, thunk, env), `symbol` reuse.
- `eval/`: tree-walking evaluator over IR. `force`, apply, `with` chain,
  operators, equality, coercions, error and trace.
- `builtins/`: primops, one file per family. Registered through one table.
- `store/`: pure functions. Nix base32, sha1/sha256/sha512/md5 (crates
  allowed), NAR serialization and hashing, store path computation, ATerm
  derivation serialization, `hashDerivationModulo`, text and fixed-output
  paths. No dependency on `runtime`.
- `corepkgs/`: `derivation.nix`, `fetchurl.nix` embedded from Nix 2.34.8.
- `trace/`: the tracing evaluation cache. Records every observation of
  the outside world during evaluation (files, directories, stats, copied
  trees, environment), keys the run on the command, and replays the
  result when the read set still validates. See `docs/tracing-cache.md`.
- `flake/`: locked flakes. `PATH#attr` on the command line, Nix's
  `call-flake.nix` embedded verbatim, `fetchTree` for locked inputs, the
  root tree copied like `git+file://` or `path:`. See `docs/flakes.md`.
- `devenv/`: `ietsp print-dev-env`, the `nix print-dev-env` replacement for
  `.envrc`: the `-env` derivation, the environment JSON, the printed bash.
  See `docs/direnv.md`.
- `main.rs`: CLI. `ietsp eval [--strict] [--json] [--raw] [--no-cache] [-I prefix=path] [-E expr | file | dir#attr]`,
  `ietsp build`, `ietsp print-dev-env`, `ietsp parse`, `ietsp cache-dump`.

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

`Result<Word, Box<Error>>`. `tryEval` catches `Throw` and `Assert` only.
Errors carry a message, a span, and a trace. Position information is kept
per IR node for `unsafeGetAttrPos`, lambda positions, and error messages.

## Parallelism (phase 2)

Cells are atomic from day one so the sequential path never changes shape.
Phase 2 adds: worker threads, work-stealing deques, stackful fibers that
park on a BLACKHOLE claimed by another fiber, strict-demand fan-out on
lists and attrsets, and a speculation gate. Designed after fix; see
`docs/parallel.md` when it exists.

## Testing

- `tests/lang/`: Nix 2.34.8 functional language tests (`eval-okay-*.nix`
  with `.exp`, `eval-fail-*.nix`). `scripts/langtests.nu` runs them.
- `tests/corpus/`: our own cases; `.exp` captured from `nix-instantiate`.
- `scripts/difftest.nu <expr-or-file>`: runs both evaluators and diffs.
- `bench/`: the fix workloads and `bench/run.nu` (hyperfine).

## Conventions

- Rust 2024, no `unsafe` outside `runtime/`. Document every `unsafe` block
  with the invariant it relies on.
- No comments that restate code. Comments explain why.
- `cargo clippy` clean, `cargo fmt`.
- Commit small, descriptive messages.

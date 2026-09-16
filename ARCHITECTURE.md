# ietsp architecture

ietsp is a from-scratch Nix language evaluator in Rust. Scope: evaluation
only. It parses Nix, evaluates lazily, computes derivations and store paths
in memory, and prints results. It never writes to a store.

## Goals, in order

1. Byte-exact parity with `nix-instantiate --eval --strict` on `.drvPath`
   of NixOS systems from nixpkgs. The judge is `scripts/difftest.sh`.
2. Faster than iets (2.58 s) and fix (1.46 s) on `bench/workloads/nixos-desktop.nix`
   with the pinned nixpkgs. Sequential speed first, parallel forcing second.
3. Small, readable code. One crate, modules by phase.

## Non-goals

Store writes, builds, daemon protocol, flakes, fetchers that hit the network
(they error), a REPL, a debugger, a bytecode VM.

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
  scope resolution happen in `compile`.
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
- `main.rs`: CLI. `ietsp eval [--strict] [--json] [-I prefix=path] [-E expr | file]`.

## Value model

`Word` is one 8-byte word with a 3-bit tag in the low bits.

| tag | meaning |
|---|---|
| INT | 61-bit immediate integer; out-of-range ints box to a heap object |
| PTR | pointer to a heap object with a one-byte kind header |
| THUNK | pointer to a `Thunk { env, ir }` object, not yet evaluated |
| BLACKHOLE | thunk under evaluation (recursion detection, later: parallel claim) |
| NULL, TRUE, FALSE | immediates |

Heap kinds: `Float`, `BigInt`, `String { bytes, context }`, `Path`, `List
{ [Cell] }`, `Attrs { [(Sym, Cell)] sorted by Sym id }`, `Lambda { env,
ir }`, `PrimOp`, `PrimOpApp`, `Env { up, [Cell] }`, `Thunk`.

A `Cell` is an `AtomicU64` holding a `Word`. Env slots, list elements, and
attr values are cells. Forcing a thunk overwrites its cell in place with
the result (graph reduction). Values are immutable after forcing.

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
  with `.exp`, `eval-fail-*.nix`). `scripts/langtests.sh` runs them.
- `tests/corpus/`: our own cases; `.exp` captured from `nix-instantiate`.
- `scripts/difftest.sh <expr-or-file>`: runs both evaluators and diffs.
- `bench/`: the fix workloads and `bench/run.sh` (hyperfine).

## Conventions

- Rust 2024, no `unsafe` outside `runtime/`. Document every `unsafe` block
  with the invariant it relies on.
- No comments that restate code. Comments explain why.
- `cargo clippy` clean, `cargo fmt`.
- Commit small, descriptive messages.

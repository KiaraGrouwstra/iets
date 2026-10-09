# Add a builtin

This guide is for contributors. You write a Rust function, register it under
its Nix name, and add a test that compares it with Nix.

## 1. Write the function

Here is `concatMap`. It shows most of what a builtin does:

```rust
fn concat_map(ev: &mut Evaluator, args: &[Word], pos: Pos) -> EvalResult<Word> {
    let f = ev.force_function(args[0], pos, "while evaluating the first argument passed to builtins.concatMap")?;
    let l = ev.force_list(args[1], pos, "while evaluating the second argument passed to builtins.concatMap")?;
    let mut lists = Vec::with_capacity(l.len as usize);
    for c in l.cells() {
        let r = ev.call1(f, c.get(), pos)?;
        lists.push(ev.force_list(r, pos, "while evaluating the return value of the function passed to builtins.concatMap")?);
    }
    Ok(ev.concat_lists(&lists))
}
```

Every builtin has this type:

```rust
pub type PrimFn = fn(&mut Evaluator, &[Word], Pos) -> Result<Word, Box<Error>>;
```

You get exactly `arity` arguments, and **none of them is forced yet**.
`pos` is the call site.

Return a forced value, not a thunk. The one exception is a lazy
application that you build on purpose with `ev.arena.mk_thunk_app(f, x)`.
That is how `map`, `mapAttrs`, `genList` and `zipAttrsWith` stay lazy.

Give each helper the error context that Nix uses, such as "while evaluating
the first argument passed to builtins.foo". Then your traces match Nix's.

If your builtin reads a store path, get the bytes through
`src/builtins/store_hooks.rs`. Its module comment tells you how.

## 2. Register it

Builtins live in `src/builtins/<family>.rs`. Each file has one `register`
function:

```rust
pub fn register(r: &mut Registry) {
    r.op("__stringLength", 1, string_length);   // global `__stringLength`, `builtins.stringLength`
    r.op("baseNameOf", 1, base_name_of);        // global `baseNameOf`, `builtins.baseNameOf`
    r.constant("__langVersion", Word::imm_int(6));
}
```

The name works as it does in Nix. With a `__` prefix, the prefixed name is a
global and the plain name goes in `builtins`. Without the prefix, the name
is both.

For a new family file, add it to `src/builtins/mod.rs` and call its
`register` from `install`.

## 3. Add a test

Put a case in `tests/corpus/<name>.nix`. If it needs flags, put them in
`<name>.flags`. Then record what Nix says:

```sh
scripts/corpus.nu --update <name>
```

That writes `<name>.exp` from `nix-instantiate --eval --strict`. To check
every case against iets, run:

```sh
scripts/corpus.nu
```

Last, update the row in the status table of `docs/reference/builtins.md`.

## Helpers you will use

### Forcing (all on `Evaluator`)

| helper | returns | error when |
|---|---|---|
| `force(w)` | forced `Word` | evaluation fails |
| `force_cell(&cell)` | forced `Word`, written back to the cell | |
| `force_int(w, pos, ctx)` | `i64` | not an int |
| `force_float(w, pos, ctx)` | `f64` (ints convert) | |
| `force_bool(w, pos, ctx)` | `bool` | |
| `force_string(w, pos, ctx)` | `&StringObj` (bytes + context) | not a string |
| `force_string_no_ctx(w, pos, ctx)` | `&[u8]` | string has context |
| `force_list(w, pos, ctx)` | `&ListObj` (`.cells()`) | |
| `force_attrs(w, pos, ctx)` | `&AttrsObj` (`.get(sym)`, `.attrs()`) | |
| `force_function(w, pos, ctx)` | the function word (lambda, primop, or functor set) | |
| `force_deep(w)` | recursively forced | |
| `coerce_to_string(w, &mut ctx, pos, err_ctx, coerce_more, copy_to_store, canonicalize)` | `Cow<[u8]>`, contexts appended to `ctx` | not coercible |
| `eq_values(a, b, pos)` / `less_than(a, b, pos)` | `bool` | |
| `call1(f, a, pos)`, `call2(f, a, b, pos)`, `apply(f, &args, pos)` | forced result | |

`ctx` is a `&str`: the error context from step 1.

### Making values

| constructor | notes |
|---|---|
| `ev.mk_int(i)`, `ev.mk_float(f)`, `Word::from_bool(b)`, `Word::NULL`, `Word::TRUE`, `Word::FALSE` | ints are immediates when they fit 61 bits |
| `ev.mk_string(&bytes)` | no context |
| `ev.arena.mk_string_ctx(&bytes, &ctx)` | `ctx: &[Ctx]`; use `ctx_to_arena(&mut ev.arena, vec)` to sort and dedup |
| `ev.arena.mk_string_static(bytes, ctx)` | when bytes already live in the arena (e.g. a substring of an existing string) |
| `ev.arena.mk_path(&bytes)` | path value |
| `ev.mk_list(&words)` | or `ev.arena.alloc_list(n)` and fill `.cells()` |
| `ev.mk_attrs(&mut entries, pos)` | `entries: Vec<(Sym, Pos, Word)>`; sorts, errors on duplicates |
| `ev.arena.mk_attrs_sorted(&entries)` | entries already sorted by `Sym` id and unique |
| `ev.empty_attrs()` | |
| `ev.sym(b"name")` | intern a symbol; `ev.interner.name(sym)` reads it back |
| `ev.mk_pos_attrs(pos)` | `{ file, line, column }` or null |
| `ev.arena.mk_thunk_app(f, x)` | lazy application |

Common symbols are ready on `ev.s`: `out_path`, `drv_path`, `type_`,
`name`, `value`, `functor`, `to_string` and more.

### Errors

```rust
use crate::eval::{ErrorKind, bail};
bail!(ErrorKind::Eval, pos, "cannot create list of size {}", n);
```

The kinds are in `src/eval/error.rs`. `tryEval` catches only `Throw` and
`Assert`. When iets does not support a call, use `Unsupported`. Then a
classic command name runs the real Nix instead. To add a trace line, call
`e.add_trace(pos, msg)`.

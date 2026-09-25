# How to embed the evaluator in your own program

The crate is a library with a thin `ietsp` binary on top. Another command
line, an editor server or a build service links the library and calls the
entry points. Nothing is process-global, so two evaluators can run side by
side in one process.

The full entry-point table is in
[reference/library.md](../reference/library.md).

**One rule: a frontend never reaches into evaluator internals.** No `cli`
module, no private field, no copied helper. If something you need lives
only in `cli`, move it into `crate::api` and let `cli` call it there.

## Set up a session

A `Session` holds what a run used to keep in statics: the settings, where
messages go, the tree store, the `nix.conf` that was read, the hash
mismatches a build reported.

```rust
let session = ietsp::session::Session::new();
session.load_settings(&[])?;            // ietsp.conf, optional
session.log.write_to_stderr();          // or leave the sink unset
let _entered = session.enter();         // for this thread
let mut ev = ietsp::eval::Evaluator::new();   // adopts the session
```

The library never writes to stdout or stderr on its own: a session's log
sink starts as nothing, and `write_to_stderr()` is what the `ietsp` binary
itself does. The library never exits either. It returns errors and your
program decides the exit code.

Threads the library starts inherit the session of the thread that started
them. For a thread you start yourself, call `Session::enter` on it, or use
`session::bind(f)`. `Evaluator::in_session(session, system, pure)` skips the
thread-local altogether.

## Evaluate and print

`examples/eval.rs` is a complete frontend. Its core:

```rust
use ietsp::api::{self, EvalArgs, Input};
use ietsp::eval::Evaluator;

let mut a = EvalArgs { strict: true, json: true, ..EvalArgs::new() };
a.input = Some(Input::Expr("{ a = 1; b = [1 2]; }".into()));

let workers = a.workers;
// the closure runs as the main fiber, so `force` may park while helpers work
let code = ietsp::parallel::try_run(workers, Evaluator::new(), move |ev| {
    match api::evaluate(ev, a) {
        Ok(v) => { /* force and print */ 0 }
        Err(e) => { api::report(ev, &e); 1 }
    }
});
```

Run it:

```sh
cargo run --release --example eval -- -E '{ a = 1; b = [1 2]; }'
```

```
{"a":1,"b":[1,2]}
```

`parallel::try_run` is the plain version. `api::run::Run` is the same step
plus the lints and the tracing cache:

```rust
let run = api::run::Run::new(&args);
run.go(|ev, cache| { /* everything here is the main fiber */ });
```

Use `Run` unless you have a reason not to: it is the one scaffold every
command in the binary goes through, so you get the cache and the lint
configuration for free.

## Walk a value safely

`api::value` is the safe surface. `Eval<'e>` wraps a `&'e mut Evaluator`.
`Value<'e>` is one value of that evaluator: a `Copy` machine word with no
methods that reach into the arena on their own.

The shared `'e` is the safety story. A `Value<'e>` only exists while the
evaluator is borrowed, so it cannot outlive the arena it points into.
`Value` is invariant in `'e`, so it never widens to another borrow. Nothing
in the surface exposes an arena pointer or a tag.

```rust
use ietsp::api::value::Eval;
use ietsp::eval::Evaluator;

let mut evaluator = Evaluator::new();
let mut e = Eval::new(&mut evaluator);
let root = e.eval_expr(r#"rec { name = "demo"; version = 3; greet = who: "hello ${who}"; }"#, b"/")
    .and_then(|v| e.force(v))?;

println!("root is {}", root.kind().type_of());

let n = root.attrs_len().unwrap();
let mut names: Vec<Vec<u8>> = Vec::with_capacity(n);
for i in 0..n {
    let (name, _) = e.attr_at(root, i).unwrap().unwrap();
    names.push(name.to_vec());
}
names.sort();   // index order is the evaluator's own, not alphabetical
```

`examples/value.rs` is that walk in full:

```sh
cargo run --release --example value
```

```
root is set
  dir : path = /tmp
  enabled : bool = true
  greet : lambda = lambda
  name : string = "demo"
  nested : set = 1 attributes
  nothing : null = null
  ratio : float = 0.5
  tags : list = 2 elements
  version : int = 3
version + 0 = Some(3)
greet "world" = "hello world"
```

Four things to remember:

- **A getter on an unforced value reports `ValueKind::Thunk` and returns
  `None`.** It never panics and never forces. Force first. Methods on
  `Value` read an already-forced value; methods on `Eval` may evaluate,
  intern a name or allocate.
- **`attr_at` index order is the evaluator's internal symbol order**, not
  alphabetical, exactly as Nix's `nix_get_attr_byidx` warns. Sort the names
  yourself when the order must be reproducible.
- **Reading borrows, constructing copies.** `as_bytes`, `as_path` and the
  attribute names borrow the arena with no copy. `string` and `path` copy
  their bytes in, and `list` and `attrs` gather a temporary first.
  Construction is the cold path; nothing on a walk allocates.
- **Keep one `Eval` per evaluator.** A value from one evaluator handed to a
  different `Eval` does not dangle, because no arena is ever freed, but it
  reads names through the wrong symbol table. The lifetime does not rule
  that out.

This surface is modelled on Nix's own C API, so a binding for another
language has a shape to follow;
[reference/library.md](../reference/library.md) maps each item to its
`nix_api_value.h` name.

## Build what you evaluated

`examples/build.rs` evaluates to derivations, plans the upload and builds
through the dispatcher:

```sh
cargo run --release --example build -- --builder daemon ./greeting.nix
```

```
/nix/store/vwp4kv8mzqx77xsf8x4dff531r5bcxkz-greeting
```

The four calls it makes:

```rust
let roots = api::installable::drv_roots(ev, value)?;
let plan = api::build::plan(ev, &roots)?;
let transport = api::build::transport(flag, &api::build::config())?;
let built = api::build::Dispatch::open(&transport, &options, plan, prov, style)?
    .realise(&roots, &name)?;
```

`Dispatch` owns the plan, the source overrides and the dispatcher, so you
never build an upload object yourself. `Built::paths(roots)` gives one
output path per root.

## Read the derivations an evaluation made

```sh
cargo run --release --example drv_info -- \
  -E 'derivation { name="x"; system=builtins.currentSystem; builder="/bin/sh"; args=["-c" "true"]; }'
```

```
drv	attr	position	outputs
/nix/store/k5d0ypnr0h8bd289z0mkhpq5p95mfl5w-x.drv	-	-	out
1 derivations
```

Every derivation the run created is in `Evaluator::drv.drvs`, keyed by
store path. Each `DrvInfo` carries `attr` and `pos`, and
`provenance::Provenance::snapshot(ev)` maps store paths back to the local
trees they were copied from. See
[explanation/provenance.md](../explanation/provenance.md).

## Report errors the way ietsp does

```rust
api::report(ev, &err);          // to the session log; returns the exit code
let record = api::report_of(ev, &err);   // to render yourself
```

`report` rewrites store paths and positions back to the local trees they
came from. Use `report_of` and leave the session's sink unset when you want
to render the record yourself.

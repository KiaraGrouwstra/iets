# Nix's C API on ietsp

`ietsp-c` is a shared and static library that exports Nix's own C API.
A program compiled against Nix's headers — `nix_api_util.h`,
`nix_api_expr.h`, `nix_api_value.h` — links this library in place of
Nix's and needs no source change. The headers are the specification:
same symbol names, same signatures, same ownership rules, same error
codes, same NULL handling.

It is a separate crate in the workspace, not a feature of `ietsp`. The
`ietsp` crate stays a plain `lib` plus one `bin`; only `ietsp-c` is built
as `cdylib` and `staticlib`, so an ordinary build is unchanged and the
`unsafe` boundary code lives in one place. `ietsp-c` may use only the
`pub` surface of `ietsp`, so the ABI cannot drift away from the safe
embedding API it wraps (docs/library.md).

## Build and link

```console
$ nix build .#c-api
$ ls result/lib
libietspc.a  libietspc.so  libnixexprc.so  libnixstorec.so  libnixutilc.so
```

The three `libnix*c.so` are symlinks to the one library, so a build
system that already passes `-lnixexprc -lnixutilc -lnixstorec` keeps
working. Compile against Nix's headers and link against ietsp:

```console
$ cc -std=c11 main.c \
    -I"$(nix eval --raw nixpkgs#nix.dev.outPath)/include" \
    -L result/lib -lnixexprc -lnixutilc -lnixstorec -o main
```

In the dev shell the headers are already on the include path:

```console
$ cargo build --release -p ietsp-c
$ cc -std=c11 tests/c-api/main.c -Ltarget/release -lietspc \
    -Wl,-rpath,"$PWD/target/release" -o target/c-api-test && ./target/c-api-test
```

`nix flake check` runs that same program as the `c-api` check, compiled
against Nix's headers.

## What is implemented

`nix_api_util.h`
: `nix_c_context_create`, `nix_c_context_free`, `nix_err_code`,
  `nix_err_msg`, `nix_err_info_msg`, `nix_err_name`, `nix_set_err_msg`,
  `nix_clear_err`, `nix_set_verbosity`, `nix_version_get`,
  `nix_setting_get`, `nix_setting_set`, `nix_libutil_init`.

`nix_api_expr.h`
: `nix_libexpr_init`, `nix_expr_eval_from_string`, `nix_value_call`,
  `nix_value_call_multi` (and so the `NIX_VALUE_CALL` macro),
  `nix_value_force`, `nix_value_force_deep`, `nix_eval_state_builder_new`,
  `nix_eval_state_builder_load`, `nix_eval_state_builder_set_lookup_path`,
  `nix_eval_state_build`, `nix_eval_state_builder_free`,
  `nix_state_create`, `nix_state_free`, `nix_gc_incref`, `nix_gc_decref`,
  `nix_gc_now`, `nix_gc_register_finalizer`.

`nix_api_value.h`
: `nix_alloc_value`, `nix_value_incref`, `nix_value_decref`,
  `nix_get_type`, `nix_get_typename`, `nix_get_bool`, `nix_get_int`,
  `nix_get_float`, `nix_get_string`, `nix_get_path_string`,
  `nix_get_list_size`, `nix_get_attrs_size`, `nix_get_list_byidx`,
  `nix_get_list_byidx_lazy`, `nix_get_attr_byname`,
  `nix_get_attr_byname_lazy`, `nix_has_attr_byname`,
  `nix_get_attr_byidx`, `nix_get_attr_byidx_lazy`,
  `nix_get_attr_name_byidx`, `nix_init_bool`, `nix_init_int`,
  `nix_init_float`, `nix_init_null`, `nix_init_string`,
  `nix_init_path_string`, `nix_init_apply`, `nix_copy_value`,
  `nix_make_list_builder`, `nix_list_builder_insert`,
  `nix_list_builder_free`, `nix_make_list`, `nix_make_bindings_builder`,
  `nix_bindings_builder_insert`, `nix_bindings_builder_free`,
  `nix_make_attrs`.

`nix_api_store.h`, only what the expression API needs
: `nix_libstore_init`, `nix_libstore_init_no_load_config`,
  `nix_store_open`, `nix_store_free`, `nix_store_get_uri`,
  `nix_store_get_storedir`.

## What is not implemented

These symbols are absent, so a program that uses one fails to link
rather than misbehave at runtime.

- Primops: `nix_alloc_primop`, `nix_register_primop`, `nix_init_primop`.
  A C callback would have to run inside the evaluator's fibers.
- External values: `nix_get_external`, `nix_init_external`, and all of
  `nix_api_external.h`. ietsp has no external value type, so
  `NIX_TYPE_EXTERNAL` never comes out of `nix_get_type` either.
- `nix_string_realise` and `nix_realised_string_*`: realising needs the
  build path, not the evaluator.
- The rest of `nix_api_store.h` (store paths, realisation, derivations,
  closures), `nix_api_fetchers.h`, `nix_api_flake.h`, `nix_api_main.h`.

## Lifetimes and memory

**Values live until the state is freed.** ietsp allocates into an arena
that belongs to the evaluator and is never collected, so reference
counting has nothing to do: `nix_value_incref`, `nix_value_decref`,
`nix_gc_incref` and `nix_gc_decref` return `NIX_OK` and change nothing,
and `nix_gc_now` does nothing. The contract the header states — a value
stays alive while you hold a reference — is honoured, more strongly than
required. The cost is that **memory grows until `nix_state_free`**: every
`nix_alloc_value`, every value a getter returns, and every string
`nix_get_path_string` or `nix_get_attr_name_byidx` hands back is held by
the state. A long-running consumer that walks many values should build a
state per unit of work and free it.

`nix_gc_register_finalizer` records nothing and the finalizer never runs,
because nothing is ever collected.

`nix_state_free` frees the evaluator, its arena, its value cells and its
strings at once. Every `nix_value *`, `const char *` name and path string
that came from that state becomes invalid at that point. `Store *`,
`nix_eval_state_builder *`, `ListBuilder *`, `BindingsBuilder *` and
`nix_c_context *` are freed by their own calls, as the headers say.

Strings the headers describe as "valid until state is freed" are
deduplicated per state, so walking the same attribute set repeatedly does
not grow memory.

## Where ietsp cannot honour the header

- **A NULL context swallows the error.** The headers say that passing
  `NULL` where a `nix_c_context *` is expected makes the API throw a C++
  exception. There is no C++ here and Rust panics abort, so a call with a
  NULL context returns its error code (or NULL, or the documented
  fallback value) and drops the message. Pass a context.
- **`nix_setting_get` and `nix_setting_set` always answer `NIX_ERR_KEY`.**
  ietsp has no global mutable settings table; a state reads its own
  configuration when it is built, from `ietsp.conf` and the environment
  (docs/settings.md). Setting Nix options from C is not supported, and an
  error is the honest answer.
- **`nix_set_verbosity` validates the level and drops it.** ietsp's log
  has no verbosity knob (docs/library.md); the session's log sink is what
  a host controls, and the C API has no call for it.
- **A type mismatch is an error, not an assertion.** Nix's
  implementation `assert`s in `nix_get_int` and friends, which is a crash
  with assertions on and undefined behaviour with them off. Here the call
  sets `NIX_ERR_NIX_ERROR` with the name `type` and returns the header's
  documented fallback (`0`, `0.0`, `false`, `NULL`). A correct consumer
  never sees the difference.
- **The getters force what Nix asserts is already forced.**
  `nix_get_attr_byname` and the `byidx` calls force the attribute set
  first instead of requiring it forced. This is a superset of the header;
  a program that forces first behaves identically.
- **`nix_get_typename` is coarser for functions.** Nix names the built-in
  behind a value ("the built-in function 'add'"); ietsp reports
  `a function` for every lambda, primop and partial application. The
  other names, `a string with context` included, match `nix::showType`.
- **`nix_err_name` gives ietsp's error codes.** They are the stable codes
  of docs/cli.md — `parse`, `type`, `throw`, `assert`, `abort`,
  `undefined-var`, `infinite-recursion`, `missing-attr`, `io`,
  `unsupported`, `lint`, `hash-mismatch`, `eval` — not the demangled C++
  class names Nix returns. There is no C++ class to name.
- **`nix_err_msg` and `nix_err_info_msg` carry ietsp's wording.** The
  message is the full report the `ietsp` command line prints, with the
  position and the trace; the info message is the bare text. A consumer
  that matches on Nix's exact message text will not match.
- **`nix_version_get` returns `2.34.8-ietsp`.** That is the Nix version
  this ABI answers to, with the implementation named, so a consumer that
  prints it says which evaluator it loaded.
- **The store handle is a name only.** `nix_store_open` remembers the URI
  so `nix_store_get_uri` can answer; ietsp opens its own store when it
  needs one, and a state built from a different `Store *` evaluates the
  same way. `nix_store_get_storedir` always reports `/nix/store`.
- **Evaluation is sequential.** ietsp's worker pool
  (`parallel::try_run`) runs for the length of one call and hands the
  evaluator to a closure, which a handle-based C API cannot express
  without marshalling every call to a dedicated thread. A state built
  here evaluates on the calling thread. The tracing cache is keyed on a
  whole run's arguments (`api::setup_cache`), which
  `nix_expr_eval_from_string` does not have, so it is off as well. Both
  are a later stage.
- **A panic aborts.** The workspace builds with `panic = "abort"`, so a
  bug in the library cannot be turned into an error code. Evaluation
  failures are not panics; they come back through the context.
- **One state, one thread at a time.** The header says a `nix_c_context`
  must not be used by two calls at once. The same holds for an
  `EvalState *` and for the values it owns. Two states in one process are
  independent, as `Session` guarantees (docs/library.md).

## A worked example

```c
#include <nix_api_util.h>
#include <nix_api_expr.h>
#include <nix_api_value.h>
#include <stdio.h>
#include <string.h>

static void copy_str(const char * start, unsigned int n, void * user_data)
{
    char * out = user_data;
    memcpy(out, start, n);
    out[n] = '\0';
}

int main(void)
{
    nix_c_context * ctx = nix_c_context_create();
    nix_libexpr_init(ctx);

    Store * store = nix_store_open(ctx, "dummy://", NULL);
    EvalState * state = nix_state_create(ctx, NULL, store);

    nix_value * root = nix_alloc_value(ctx, state);
    if (nix_expr_eval_from_string(
            ctx, state, "{ greet = who: \"hello ${who}\"; }", "/", root)
        != NIX_OK) {
        printf("%s\n", nix_err_msg(NULL, ctx, NULL));
        return 1;
    }

    nix_value * greet = nix_get_attr_byname(ctx, root, state, "greet");
    nix_value * arg = nix_alloc_value(ctx, state);
    nix_init_string(ctx, arg, "world");

    nix_value * out = nix_alloc_value(ctx, state);
    nix_value_call(ctx, state, greet, arg, out);

    char buf[256];
    nix_get_string(ctx, out, copy_str, buf);
    printf("%s\n", buf);          /* hello world */

    nix_state_free(state);        /* every value above dies here */
    nix_store_free(store);
    nix_c_context_free(ctx);
    return 0;
}
```

`tests/c-api/main.c` is the longer version: it walks an attribute set,
reads every scalar type, calls a curried function, builds a list and an
attribute set with the builders, and checks the error paths for a syntax
error, a wrong type and a missing attribute.

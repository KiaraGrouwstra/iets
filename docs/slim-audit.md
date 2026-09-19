# Slim audit

State: `origin/main` at `f67fd13`, 50,315 lines of Rust under `src/`
(51,217 with `build.rs`, `examples/` and `tests/`), 242 `#[test]`
functions.

Method for the dead-code pass: every `pub` in the library was rewritten to
`pub(crate)`, then the true external surface (what `src/main.rs`,
`examples/*` and `tests/*` reach) was restored one compiler round at a
time until `cargo check --bins --examples --tests` was clean again. With
no `pub` roots left over, rustc's `dead_code` lint reports what nothing
reaches. 48 items came back.

## 1. Size per module and per feature

| Feature | Modules | Lines | `#[test]` |
|:---|:---|---:|---:|
| evaluator core | `syntax` 3392, `compile` 2477, `runtime` 2303, `eval` 2630, `builtins` 6382 | 17,184 | 73 |
| parallel | `parallel` (sched 1076, force 656, thunk 464, fiber 455, spec 248, shared 234, interner 233, deque 178, mod 163) | 3,707 | 14 |
| cli | `cli` minus `classic`/`fastbuild` (jobs 564, devenv 531, build 334, mod 131, eval 97, cache_gc 63, cache_dump 52, parse 48) | 1,820 | 1 |
| classic | `cli::classic` (nix 810, shell 749, mod 513, instantiate 282, build 237, develop 185) | 2,776 | 8 |
| fast-build | `cli::fastbuild` (mod 480, flags 400) | 880 | 0 |
| remote | `remote` (worker 1471, grpc 2161, plan 549, copy 122, event 56, mod 8) | 4,367 | 19 |
| build | `build` (mod 826, render 778, results 433, backend 386, upload 362, fanout 224, event 89) | 3,098 | 22 |
| store | `store` (drv 1018, nar 956, hash 588, path 473, locate 147, context 137, source 45) | 3,374 | 42 |
| flake | `flake` (spool 907, fetch 847, http 465, unpack 418, mod 385, root 306, lock 184) | 3,512 | 28 |
| jobs | `jobs` (mod 885, collector 423, constituents 422, line 340, cache 317, instantiate 289, stream 167, emit 98) | 2,941 | 6 |
| caches | `trace` 1964, `compile::cache` 1072, `cachefs` 537, `inputs` 607 | 4,180 | 33 |
| devenv | `devenv` | 734 | 10 |
| support | `config` 688, `provenance` 510, `print` 498, `settings` 410, `api` 327, `nixbin` 165, `error` 96, `lib` 23 | 2,717 | 34 |

(`compile::cache` and `trace` are counted once under caches; the
evaluator-core row holds `compile` minus its cache.)

Headline: the evaluator core is 34% of the tree. Everything that is not
evaluation — cli, classic, fast-build, remote, build, jobs, devenv — is
16,616 lines, 33%. Caches are 8%.

## 2. Dead code

48 items the lint reports as unreachable. Genuinely dead in production:

| Where | What |
|:---|:---|
| `store/context.rs` | the whole module: `ContextElem`, `parse`, `to_full_string`, `to_internal_string`, `parse_path`. A second string-context parser; `builtins/context.rs` is the live one. 137 lines. |
| `config.rs` | `Config::{from_str, empty, int, paths, substituters, trusted_public_keys, builders, experimental_features, nix_path, sandbox}`, `CACHE_NIXOS_ORG_KEY`, `expand_builder_lines`, `parse_nix_path`, `is_pseudo_url`. Nix-parity getters with no consumer. |
| `provenance.rs` | `SETTING`, `Map::{is_empty, len, rev_of}`, `Provenance::is_on`, field `rev` |
| `parallel` | `deque::Deque::is_empty`, `interner::{get, is_empty}` (x2), `sched::{driver, set_eager_wakes}`, `shared::{Shared::new?, SharedMap::{new, is_empty}, Files::is_empty}`, `force::fanout_attrs` (plus its unused re-export in `parallel/mod.rs`) |
| `runtime` | `arena::alloc_str`, `value::{alloc_attrs, merge_ctx}`, `value::Value::Env` variant, `word::Word::is_bool` |
| `syntax/ast.rs` | `Attrs::{EMPTY, is_empty}`, `capacity_bytes` |
| `eval/mod.rs` | `Evaluator::{file, mk_string_ctx}`, field `Globals::derivation` |
| misc | `build/event.rs` `Kind::Cached`, `build/render.rs` `is_failed`, `cli/classic/mod.rs` `Common::print`, `compile/ir.rs` `is_cheap`, `error.rs` `Error::Cache`, `jobs/cache.rs` `into_connection`, `print.rs` `sym_name`, `remote/plan.rs` `describe` (x2), `store/drv.rs` `drv_path`, `flake/spool.rs` `{root, size}` |

Flagged but kept, with the reason:

- `Interner::is_empty`, `SharedMap::is_empty`, `Files::is_empty`,
  `Range::is_empty`: their `len` is live, and clippy's
  `len_without_is_empty` is an error under `-D warnings`. The lint makes
  these four mandatory, not optional.
- `Interner::get`, `Runtime::set_eager_wakes`, `Derivation::drv_path`:
  used only by unit tests, but as real test support (a lookup that does
  not intern; a deterministic wake; the store path of a derivation with
  no ATerm at hand). Kept, with the doc comment saying so.
- `Kind::Env`: `Kind` is `#[repr(u8)]` and `Word::heap_kind` reads the
  discriminant straight off the heap object. Dropping a variant shifts
  every discriminant after it for one line of saving. Not worth it.

Dead in production but alive as a test oracle — **not** cut:
`flake::unpack::{unpack, make_parents, mkdir, clear, singleton}` and
`flake::http::decompress` (~160 lines). `flake::spool` replaced
`flake::unpack` on the production path, and `spool`'s own tests still
dump a tree through `unpack` and compare NARs. Deleting the second
implementation deletes the differential check that guards the first.
`unpack::{member_path, parts_of}` stay in production use.

### Call-graph checks asked for

- **`remote::worker::build::{build, build_on}` — ALIVE, not dead.**
  `build_on` has three callers (`cli/classic/instantiate.rs:232`,
  `cli/classic/shell.rs:411`, `cli/devenv.rs:451`); `build` wraps it with
  a connection open and is the `docs/library.md` entry point that
  `examples/build.rs` uses. `build::Dispatcher` did not take every
  caller: it took the batching ones (`ietsp build`, `nix-build`,
  `nix build`, `fast-build`). The three that remain are single-root,
  no-batch builds. The overlap is real — 392 lines of a second build flow
  — but folding them into `Dispatcher` changes the renderer, and
  classic-e2e compares stdout byte for byte. Proposed, not cut.
- **`remote::grpc::build::build`** does not exist. `grpc/build.rs`'s
  entry points (`build_paths`, `build_derivation`, `substitute`,
  `add_multiple`, `query_valid_paths`, `fetch_into_dir`, `register`,
  `drv_table`, `output_path`) are all reached from `grpc/backend.rs`.
- **`remote::copy`** alive: `closure` from `build/backend.rs:323`
  (out-links from a remote builder), `topo_sort` from
  `remote/grpc/build.rs:22`.
- **`cli::api` re-exports** — all twelve are used by the subcommand
  modules. A pure forwarding line block (4 lines); left alone.
- **`trace::run` vs `api::setup_cache`** — not duplicates.
  `api::setup_cache` builds the key from `EvalArgs`; `trace::run::Cache`
  is the lookup/write object. Keep both.
- **`plan::hash_source`** alive: `plan.rs:183` still hashes sources that
  evaluation did not record (`SourceInfo::hashed == None`).
- **`jobs::stream`** has one consumer, `cli::fastbuild`. That is the
  streaming half of eval-jobs and it is 167 lines; keep.
- **`Options`/`Config`/`Settings`** — three different things, not a
  trio to collapse. `Config` = nix.conf as Nix reads it. `Settings` =
  ietsp's own `IETSP_*`/`ietsp.conf` knobs. `Options` = the five build
  options the worker protocol puts on the wire, derived from `Config`
  via `TryFrom`. Keep.
- **`parallel::spec`** — see section 3.

## 3. Abstractions with one implementation

| Thing | Impls / callers | Verdict |
|:---|:---|:---|
| `build::backend::Backend` | 3 (`Worker`, `Grpc`, test) | keep — the two transports are the point |
| `build::upload::Uploader` | 3 (`PlanUpload`, `StreamUpload`, test) | keep — fixed set vs streamed batches |
| `remote::plan::NarSource` | 2 (`inputs::Spooled`, `flake::spool::Tree`) | keep |
| `parallel::sched::Driver` | 4 | keep |
| `flake::http::Stream` | 1 blanket impl | keep — trait alias for `Box<dyn>` |
| `parallel::force::ViewPrimop` | 1 impl, 1 call site | **inline** (11 lines) |
| `parallel::shared::Shared<T>` | 1 user (`DrvState::hash_cache`) | **inline** to `Arc<Mutex<_>>` (22 lines) |
| `parallel::force::EvalDriver(SyncEv(Evaluator))` | two wrapper layers, one construction site | **inline** — `impl Driver for SyncEv` |
| `store/context.rs` | 0 users | **delete** (137 lines) |
| `Value::Env`, `Kind::Cached`, `Error::Cache` | never constructed | **delete** |

## 4. Duplicated flows

### Argument parsers

| Parser | Distinct flags | Parse lines |
|:---|---:|---:|
| `cli::parse_eval_args` | 26 | 92 |
| `cli::build` | 6 | 33 |
| `cli::jobs::parse_jobs_args` | 33 | 113 |
| `cli::fastbuild::flags::parse` | 49 | 144 |
| `cli::classic::common_flag` | 23 | 52 |
| `cli::classic::{instantiate,build,nix,shell,develop}` own flags | 17/19/28/22/12 | ~350 |

The shared handling already exists, twice: every ietsp-native subcommand
consumes its own flags and pushes the rest to `parse_eval_args`, and the
classic family shares `Argv` + `common_flag`. `fastbuild::flags` already
keeps its pass-through classes in three declarative tables
(`REFUSED`, `IGNORED`, `IGNORED_BINARIES`).

Proposed design, for the record: one `&[Flag]` table per command, each
entry `{ names: &[&str], arity: u8, action: fn(&mut Ctx, &[String]) }`,
plus a shared cursor that reads arity and calls the action. Per-command
subsets become table slices, so `--timings` and `--cache-dir` are
declared once.

Estimated saving: **negative to zero.** The ~730 lines of parse code are
almost all action bodies that a table keeps verbatim; what disappears is
`match` punctuation, roughly 120 lines, against a ~100-line dispatcher
and a table entry per flag. Four of the arms are not uniform actions at
all (`--arg-from-file` rewrites argv, `--log-format` and
`--override-input` exit with a per-flag reason, `--repair` likewise), and
`Argv::value`'s "a flag without its value falls back" rule differs from
`parse_eval_args`'s "a flag without its value prints usage". The
by-hand `usage()` string and byte-identical classic stdout would both be
at risk for no line win. **Not cut.**

### Three on-disk wire formats

`cachefs` is already the shared store layer: format version, writer lock,
atomic write, sweep shapes, `touch`. What the three stores do not share
is the primitive codec:

| Store | Codec | Lines |
|:---|:---|---:|
| `trace::wire` | `Enc`/`Dec`, LEB128, `R<T>` results | ~105 |
| `compile::cache` | `Ser`/`De` + free `put_*`, LEB128, `ok` flag | ~45 |
| `flake::spool` (inputs index) | free `put_*` + `Dec`, LEB128, `Option` | ~50 |

One `varint` helper would replace maybe 90 of those 200 lines and add
~50. Net ≈ 40 lines, 0.08% of the tree, for a change that touches three
on-disk formats at once. **Proposed, not cut.**

### `parallel::spec`

Speculation has been off since day one (`IETSP_SPEC` defaults to 0,
docs/parallel.md line 412). Its cost is `spec.rs` 248 lines, a second
deque lane per worker in `sched.rs`, hooks in `force.rs`, and three
`Settings` keys. Measured (`bench/run.nu nixos-desktop`): see the
Measurement section below. Cut only if it does not pay.

## 5. Dependencies

`cargo tree -e no-dev --depth 1`: 24 direct runtime deps, 3 build deps.

| Dep | Used by |
|:---|:---|
| `libc` | 12 files (mmap, signals, fibers, stat) |
| `serde_json` | 14 files (eval-jobs lines, drv wire, flake locks, results) |
| `sha2`, `sha1`, `md-5` | `store::hash` (Nix's four hash algorithms), `sha2` also in `trace` and `build.rs` |
| `blake3` | `compile::cache` entry names |
| `toml` | `builtins::text::fromtoml` (`builtins.fromTOML`) |
| `tonic`, `tonic-prost`, `prost` | `remote::grpc` (`prost`/`tonic-prost` only through generated code) |
| `tokio`, `tokio-stream` | `remote::grpc`, `flake::http` |
| `zstd` | `flake::http`, `remote::grpc::build` |
| `hyper`, `hyper-util`, `http-body-util`, `tokio-rustls`, `rustls`, `rustls-native-certs` | `flake::http` only |
| `tar` | `flake::{unpack, spool}` |
| `flate2` | `flake::{http, unpack, spool}` |
| `bzip2`, `lzma-rust2` | `flake::http` only |

Nothing here is covered by std or by another dep. Two notes, neither a
cut today:

- `bzip2` and `lzma-rust2` exist for two archive filters that flake
  inputs almost never use (`Compression::sniff` handles gzip, xz, zstd,
  bzip2). Dropping them would drop `fetchTarball` support for `.tar.bz2`
  and `.tar.xz` — a behaviour cut, not a slimming cut, so it needs
  Jonas's call.
- `rustls-native-certs` duplicates what `tonic`'s `tls-native-roots`
  feature already pulls in, but `flake::http` drives its own
  `tokio-rustls` client and needs the root store directly.

## 6. Measurement

`bench/run.nu nixos-desktop`, `IETSP_SPEC=1` against the default off:
recorded in the commit that acts on it.

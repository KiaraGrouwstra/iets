# Plumbing and porcelain

ietsp has one code path. Every capability is an operation in the library,
with a machine-first contract. A plumbing command exposes that operation
with JSON in and JSON out. A user-facing command — the porcelain — is
written against the same operation.

## The rule

**A capability is implemented once, below the command line.** Evaluation,
store access, build dispatch, fetching and flakes live in the library. No
command re-implements one.

**Porcelain may have its own libraries.** Presentation logic, interactive
flows, Nix argv compatibility, colour, tables, progress and UI caches are
porcelain-side code. They may be large. They may be shared between
commands. They are not a second implementation of a plumbing capability.

**The plumbing must expose what the porcelain needs.** If a porcelain
command reaches past `crate::api` into `crate::eval`, `crate::store`,
`crate::build`, `crate::remote`, `crate::flake` or `crate::jobs`, the
plumbing has a gap. Close the gap in the operation layer first.

This is git's split. It keeps the line count down, because the second
copy is what costs.

## What is duplicated today

Measured on `67a52a1`. `src/cli` is 7526 lines, `src/flake` is 4689,
`src/api*` is 904.

### 1. Installable resolution

"A flake reference, a FILE with `-A`, or `-E EXPR`" becomes derivations
in nine places, in four different shapes.

| Where | Shape | Lines |
|:---|:---|---:|
| `src/cli/build.rs:206-216` | `evaluate` + `drv_roots` + `set_attr` | ~12 |
| `src/cli/build.rs:132-141` | the same again, for `fix-hashes` | ~10 |
| `src/cli/diff.rs:155-190` | `evaluate` + `drv_roots`, exactly one root | ~36 |
| `src/cli/prefetch.rs:113-121` | `evaluate` + `drv_roots`, exactly one root | ~9 |
| `src/cli/devenv.rs:338-366` | flake fragment, candidate paths, `drv_roots` | ~29 |
| `src/cli/search.rs:305-362` | flake fragment, candidate paths, all matches | ~58 |
| `src/cli/classic/nix.rs:446-508` | flake fragment, candidate paths, first match | ~63 |
| `src/cli/classic/mod.rs:305-370` | `get-drvs.cc` walk (`nix-build`, `nix-shell`) | ~66 |
| `src/devenv/mod.rs:460-479` | a third `attr_paths` table | ~20 |

Three separate `attr_paths` tables exist for the same
`getActualAttrPaths` rule: `cli/classic/nix.rs` (packages), `devenv/mod.rs`
(devShells) and an inline list in `cli/search.rs` (package sets). The
candidate loop that follows them — select, take the first or all, report
`does not provide attribute` — is written three times.

### 2. Build dispatch

`config` → `settings::builder` → `Transport::parse_checked` → `plan::plan`
→ `PlanUpload::new` → `Dispatcher::open` → `submit` appears in
`cli/build.rs:305-400`, `cli/prefetch.rs:176-205`, `cli/classic/build.rs:119-170`,
`cli/classic/instantiate.rs:166-230`, `cli/classic/nix.rs:341-700`,
`cli/classic/shell.rs:281-395`, `cli/devenv.rs:451-460` and
`cli/fastbuild/mod.rs:86-95`. `cli/classic/mod.rs:468-482` (`plan_roots`)
is a partial extraction that only the classic commands use.

### 3. The evaluate-then-walk-then-report scaffold

`a.evaluator()` → `configure_lints` → `setup_cache` → cache hit →
`ev.trace = enabled` → `run_parallel` → `report(ev, &e)` is written out in
`cli/eval.rs:9-32`, `cli/build.rs:150-188`, `cli/search.rs:139-167`,
`cli/devenv.rs:120-158`, `cli/jobs.rs:236-260`, `cli/diff.rs:155-161`,
`cli/prefetch.rs:70-75` and `cli/fastbuild/mod.rs:150-157`. Sixteen
`run_parallel` call sites, five `setup_cache` call sites, each with its
own cache-hit branch.

### 4. Text-or-JSON rendering

Hand-written per command: `cli/build.rs:print_missing` and `render_size`,
`cli/diff.rs:run` and `closure_diff`, `cli/prefetch.rs:print_hash`,
`cli/search.rs:show_results`, `cli/jobs.rs:jobs_summary`,
`cli/fastbuild/mod.rs:summary` and `write_step_summary`,
`cli/classic/instantiate.rs:print_bytes` (a copy of
`cli/classic/nix.rs:print_bytes`). No shared writer, no shared size or
table formatting.

### 5. Argument parsing

Four parsers over the same flags: `cli/mod.rs:parse_eval_args` (native),
`cli/mod.rs:eval_flag` (shared), `cli/classic/mod.rs:common_flag`
(classic) and `cli/classic/nix.rs:global_flag`. `cli/diff.rs:parse` and
`cli/prefetch.rs:cmd_prefetch` each re-drive `eval_flag` by hand because
`parse_eval_args` takes the whole argv.

## The plumbing command set

One binary, one namespace: `ietsp plumbing OP`. Each op reads flags and
optionally JSON on stdin, and writes one JSON object (or JSON lines) on
stdout. Errors go to stderr as `log::Report` records and set the exit
code, per docs/cli.md. Nothing else is printed on stdout.

### Why a namespace and not a second binary

* A second binary doubles a large static binary and the Nix package, for
  no gain.
* Classic mode dispatches on `argv[0]`; a second binary complicates the
  symlink set and the exec fallback.
* A hidden prefix hides a contract that must be stable. A contract nobody
  can find gets copied instead of called.

`ietsp plumbing` gets one line in the overview and its own `--help`. This
file is the contract.

### Shared input

Every op that evaluates takes the common evaluation flags: `-E EXPR`,
`FILE`, `FLAKEREF[#fragment]`, `-A PATH`, `--arg`, `--argstr`, `-I`,
`--impure`/`--pure-eval`, `--option`, `--workers`, `--no-cache`. This is
one parser (`api::EvalArgs`), not a per-op parser.

### The ops

| Op | Contract |
|:---|:---|
| `resolve` | an installable becomes derivation roots |
| `eval` | an installable becomes a value |
| `drv-show` | derivation paths become derivations |
| `plan` | derivation roots become an upload plan |
| `missing` | a plan and a builder become what will be built and fetched |
| `build` | derivation roots are built; outcomes come back |
| `drv-diff` | two derivations become a difference |
| `closure-diff` | two derivations become a package-level closure difference |
| `index` | an installable becomes a package index |
| `fetch` | a URL or a fixed-output derivation becomes a hash |
| `flake-info` | a flake reference becomes its inputs and its lock |
| `flake-tree` | a flake reference or input becomes a tree |
| `jobs` | a jobset becomes derivation lines |
| `dev-env` | an installable becomes a build environment |

Composition: `resolve` feeds `plan`, `build`, `drv-diff` and `dev-env`;
`plan` feeds `missing`; `flake-info` feeds `flake-tree`. Every op that
takes roots accepts `--roots -` and reads `resolve`'s output on stdin.

### `resolve`

```
ietsp plumbing resolve [EVAL FLAGS] [--prefixes package|dev-shell|package-set]
                       [--want first|all] [--drvs|--no-drvs]
```

```json
{
  "attrPaths": ["packages.x86_64-linux.hello"],
  "roots": [
    {"drv": "/nix/store/…-hello-2.12.1.drv", "output": "out", "attr": "hello"}
  ]
}
```

`attrPaths` is the candidate list that was tried, in order, so a caller can
report `does not provide attribute` itself. `output` is `null` when the
value named the derivation and not one output. `--no-drvs` stops after the
value and reports only `attrPaths`. This is the whole of duplication
cluster 1.

### `eval`

```
ietsp plumbing eval [EVAL FLAGS] [--strict] [--raw]
```

Output is the value as JSON, or the raw bytes under `--raw`. Identical to
`ietsp eval --json` today; the porcelain keeps the human printer
(`print::print_ambiguous`), which is presentation.

### `drv-show`

```
ietsp plumbing drv-show [--roots -] DRV...
```

```json
{"/nix/store/…​.drv": {"name": "hello", "outputs": {…}, "inputDrvs": {…},
  "inputSrcs": […], "system": "…", "builder": "…", "args": […], "env": {…}}}
```

Nix's `nix derivation show` shape. A derivation this run evaluated is
answered from memory; anything else is read from the store.

### `plan`

```
ietsp plumbing plan [--roots -] DRV...
```

```json
{"drvs": ["/nix/store/…​.drv"], "sources": ["/nix/store/…-source"],
 "storeDrvs": 3}
```

`drvs` is in dependency order, references first, as the upload needs it.

### `missing`

```
ietsp plumbing missing [--builder URL] [--roots -] DRV...
```

```json
{"willBuild": […], "willSubstitute": […], "unknown": […],
 "downloadSize": 0, "narSize": 0}
```

This is byte-for-byte what `ietsp build --dry-run --json` prints today.

### `build`

```
ietsp plumbing build [--builder URL] [--out NAME]... [--fetch-to DIR]
                     [--no-build] [--roots -] DRV...
```

```json
{"outcomes": [{"drv": "…", "success": true,
  "outputs": {"out": "/nix/store/…-hello-2.12.1"},
  "reason": null, "logTail": []}]}
```

The exit code follows docs/cli.md. Build progress goes to stderr through
the existing reporter; the renderer is porcelain.

### `drv-diff` and `closure-diff`

```
ietsp plumbing drv-diff [--depth N|--all] A B
ietsp plumbing closure-diff [--builder URL] A B
```

The two JSON shapes `ietsp diff --json` and `ietsp diff --closure --json`
print today, unchanged. Exit 0 equal, 1 different.

### `index`

```
ietsp plumbing index [EVAL FLAGS] [--prefixes package-set]
```

JSON lines, one per package:

```json
{"attr": "hello", "pname": "hello", "version": "2.12.1",
 "description": "A program that produces a familiar, friendly greeting"}
```

Regex matching, exclusion, colour and the `--json` object shape stay in
`ietsp search`: they are presentation. The tracing-cache index stays below,
because it is the same index either way.

### `fetch`

```
ietsp plumbing fetch [--unpack] [--hash-type ALGO] URL
ietsp plumbing fetch [EVAL FLAGS]            # a fixed-output derivation
```

```json
{"hash": "sha256-…", "url": "https://…"}
{"hash": "sha256-…", "drvPath": "/nix/store/…​.drv"}
```

### `flake-info` and `flake-tree`

```
ietsp plumbing flake-info FLAKEREF
ietsp plumbing flake-tree FLAKEREF[#input.path]
```

```json
{"dir": "/home/x/p", "source": {"type": "git", "root": "/home/x/p", "subdir": ""},
 "inputs": {"nixpkgs": {"type": "github", "owner": "NixOS", "repo": "nixpkgs",
   "rev": "…", "narHash": "sha256-…"}},
 "lockVersion": 7}
```

```json
{"path": "/nix/store/…-source", "narHash": "sha256-…",
 "lastModified": 1700000000, "rev": "…", "shortRev": "…"}
```

### `jobs` and `dev-env`

`jobs` is today's `ietsp eval-jobs` output, which is already a machine
contract (nix-eval-jobs' JSON lines). `dev-env` is `ietsp print-dev-env
--json`. Both move under the namespace unchanged; the existing names stay
as porcelain aliases.

## How the porcelain maps onto it

| Porcelain | Plumbing it calls | Porcelain-only part |
|:---|:---|:---|
| `ietsp eval` | `eval` | the human value printer, `--timings` |
| `ietsp build` | `resolve`, `plan`, `missing`, `build` | `--dry-run` text, `render_size`, `fix-hashes` prompts, `.drv` legacy input |
| `ietsp search` | `index` | regex compile and match, `--exclude`, colour, `--pretty`, the no-results hint |
| `ietsp diff` | `resolve`, `drv-diff`, `closure-diff` | the text rendering, the `diff(1)` exit codes |
| `ietsp eval-jobs` | `jobs` | the worker/collector process model, `--max-memory-size`, the summary line |
| `ietsp fast-build` | `jobs`, `build` | the terminal report, `--result-file`, the step summary |
| `ietsp print-dev-env` | `resolve`, `dev-env` | the bash rendering, the direnv cache, the GC root |
| `ietsp prefetch` | `fetch` | `--json` shape choice |
| `ietsp lint`, `ietsp parse`, `ietsp cache` | none | they are already thin over the library |
| `nix-instantiate` | `resolve` (get-drvs walk), `plan` | Nix argv parsing, `--add-root`, out-links, the exec fallback |
| `nix-build` | `resolve`, `plan`, `build` | `result` symlinks, `--add-root`, Nix's exit codes, the fallback |
| `nix-shell` | `resolve`, `plan`, `build` | the env, `execve` of the shell, `--run`, `--pure` |
| `nix-eval-jobs` | `jobs` | argv compatibility |
| `nix-fast-build` | `jobs`, `build` | argv compatibility, the report |
| `nix eval` | `eval` | Nix's printer and argv |
| `nix build` | `resolve`, `plan`, `build` | `result` symlinks, `--json` Nix shape, the fallback |
| `nix search` | `index` | Nix's output format and argv |
| `nix develop` | `resolve`, `dev-env` | the shell it execs |
| `nix print-dev-env` | `dev-env` | the bash rendering |
| `nix flake metadata` | `flake-info` | Nix's output format |

### Honest gaps

**(a) Capabilities the plumbing must expose, and does not yet.**

1. **The `get-drvs.cc` walk.** `nix-build` and `nix-shell` use
   `classic::derivations`, which walks an attrset with
   `recurseForDerivations` and returns `(drv, outputName)` pairs. `resolve`
   must grow a `--walk get-drvs` mode, or a fourth `Prefixes` value. Today
   only the classic commands can do this.
2. **`outputsToInstall`.** `classic/nix.rs:target` reads
   `meta.outputsToInstall` and `outputSpecified` to pick outputs.
   `resolve`'s `roots[].output` must carry that, or `nix build` keeps its
   own copy.
3. **The ATerm table of a run.** `ietsp diff` needs the ATerms the
   evaluation produced, because a derivation that was never written to the
   store has none on disk. `resolve` must be able to emit
   `--dump-drvs DIR`, and `drv-show` must read that directory, or
   `drv-diff` must accept the installable itself.
4. **Uploading with a local source override.** `ietsp build --source
   STOREPATH=LOCAL` reaches `locate_in_store` with a map. `plan` and
   `build` must take `--source`.
5. **Hash mismatch capture.** `ietsp prefetch` drives a build that is
   meant to fail, and reads the mismatch out of the log
   (`log::want_mismatch`, `log::take_mismatches`). `build` must report a
   mismatch as structured output, not as a log line the caller greps.
   `ietsp build`'s `fix-hashes` reads the same channel.
6. **GC roots and out-links.** `nix-build`, `nix-instantiate --add-root`
   and `print-dev-env` register roots with the *local* daemon while the
   build may run elsewhere. `build` must take `--gc-root DIR` and report
   the registered paths.
7. **The flake source resolver.** See below. `flake-tree` does not exist;
   every caller goes through `flake::eval_flake`, which fetches as a side
   effect of evaluation.
8. **`--check-cache-status`.** `eval-jobs` asks the store whether each
   output is already there. That belongs in `missing`, per root.
9. **The eval-store split.** `eval-jobs --eval-store` instantiates on one
   store while building on another. `plan` and `build` take one builder
   today.

**(b) Legitimately porcelain-side.**

1. The classic argv parsers and the exec fallback to real Nix
   (`cli/classic/`). This is compatibility, not capability.
2. `result` symlinks, `--add-root` naming, `--out-link`: file-system
   conveniences of a user interface.
3. `fix-hashes` prompting and file rewriting (`build/fixhash.rs` drives
   it, the prompt is UI).
4. The `eval-jobs` worker/collector process model and
   `--max-memory-size`. This is a resource policy of a command line, not
   an operation.
5. Terminal rendering: progress bars, colour, `--pretty`, `render_size`,
   the `fast-build` report and step summary.
6. `nix-shell`'s `execve`, `--run`, `--pure`, and the interactive shell
   it builds.
7. The direnv cache in `print-dev-env` and the `.envrc` integration.
8. Regex matching and exclusion in `search`.
9. `diff(1)`'s exit codes and the text layout.
10. `--timings` lines. They describe a run of a command, not an
    operation.

These are allowed to live in porcelain-side libraries — `src/cli/render/`
for the shared writer, `src/cli/classic/` for compatibility,
`src/cli/interactive/` for prompts. They may be shared between porcelain
commands.

## How the rule is enforced

1. **A module boundary, checked by a test.** `tests/layering.rs` reads
   `src/cli/**/*.rs` and fails when a file names
   `crate::eval`, `crate::store`, `crate::build`, `crate::remote`,
   `crate::flake`, `crate::jobs`, `crate::devenv`, `crate::trace`,
   `crate::compile` or `crate::parallel`, outside an explicit allow-list
   keyed by file. The allow-list starts as today's set and may only
   shrink; the test asserts its length against a constant, so an entry
   that is removed cannot come back unnoticed.
2. **Types.** Once `api::installable` exists, `Word` (an evaluator value)
   must not appear in a `src/cli` signature. The same test greps for it.
   A porcelain command that cannot name a value cannot evaluate one.
3. **A review checklist**, in this file:
   * Does the change add a call from `src/cli` into a layer below
     `crate::api`? Then the plumbing is missing an operation. Add it
     there first.
   * Does the change add a second way to do something `api` already
     does? Then extend the existing one.
   * Is the new code presentation, compatibility or interaction? Then it
     belongs in a porcelain-side library, and the allow-list does not
     change.
4. **Every plumbing op has one e2e case** in `scripts/`, checked against
   the JSON shape in this file. The shape is the contract; a change to it
   is a change to this file.

## Flakes behind a source resolver

`src/flake` is 4689 lines in 8 files. It reaches into `crate::inputs`
(the input store), `crate::builtins::store_hooks`, `crate::eval` and HTTP.
`crate::flake::` is named 29 times outside `src/flake`, in
`src/cli/search.rs`, `src/cli/classic/nix.rs`, `src/cli/devenv.rs`,
`src/cli/diff.rs`, `src/cli/mod.rs`, `src/cli/prefetch.rs`,
`src/builtins/paths.rs`, `src/builtins/store_hooks.rs`, `src/eval/mod.rs`,
`src/inputs.rs` and `src/api.rs`. Four of those are `is_flake_ref`, three
are `parse_attr_path`, three are `select`, and `src/cli/prefetch.rs`
reaches into `flake::spool` and `flake::http` for a job that has nothing
to do with flakes.

### The interface

A reference becomes a tree plus a lock. Fetching and the input store are
behind it.

```rust
/// Where a tree comes from. A locked input, a local path, or a URL.
pub struct SourceRef(…);

/// A tree that is there: a readable path, and what it hashes to.
pub struct Tree {
    pub path: PathBuf,
    pub nar_hash: Hash,
    pub last_modified: i64,
    pub rev: Option<String>,
}

/// What a flake locked, as data. No evaluator, no fetching.
pub struct Lock {
    pub root: NodeId,
    pub nodes: BTreeMap<NodeId, LockedInput>,
}

pub trait SourceResolver {
    /// The reference this string names, without touching the network.
    fn parse(&self, s: &str, cwd: &Path) -> Result<SourceRef>;
    /// The tree, fetched and cached if needed.
    fn tree(&self, r: &SourceRef) -> Result<Tree>;
    /// The lock of a flake reference, read from its `flake.lock`.
    fn lock(&self, r: &SourceRef) -> Result<Lock>;
    /// The bytes of one file in a tree, without materialising the tree.
    fn read(&self, r: &SourceRef, path: &str) -> Result<Vec<u8>>;
}
```

`api::flake` is then four functions over one `SourceResolver`:

```rust
pub fn is_flake_ref(s: &str) -> bool;
pub fn info(r: &SourceRef) -> Result<FlakeInfo>;          // -> flake-info
pub fn tree(r: &SourceRef) -> Result<Tree>;               // -> flake-tree
pub fn outputs(ev: &mut Evaluator, r: &SourceRef) -> EvalResult<Word>;
```

`crate::flake` becomes private. `builtins::fetchTree`, `builtins::getFlake`
and the flake evaluation all call the resolver. `prefetch` calls
`resolver.tree()` instead of `flake::spool` and `flake::http` directly, or
those two move out of `src/flake` into `src/fetch`, where they belong:
neither knows anything about flakes.

### Migration order

1. Move `flake::http`, `flake::spool` and `flake::unpack` to `src/fetch`.
   They are an HTTP client, a spooled tree and an archive unpacker. No
   caller changes except the module path. **-0 lines, 3 files move.**
2. Introduce `SourceRef`, `Tree` and `SourceResolver` in `src/source`,
   implemented by today's `flake::fetch`. `flake::fetch_tree` becomes a
   thin call.
3. Move `is_flake_ref`, `parse_attr_path` and `select` to `api::flake`.
   The six `src/cli` call sites change to `api::`.
4. `flake::lock` becomes `source::Lock`, with `flake-info` on top.
5. `crate::flake` becomes `pub(crate)`; the layering test adds it to the
   banned list.

Nothing in this section is implemented yet.

## Staged migration

Every stage is one merge, and the checks pass at its commit.

| # | Stage | src delta | Where the reduction comes from |
|--:|:---|---:|:---|
| 1 | `api::installable` + `plumbing resolve`; move `search`, `print-dev-env`, `nix build`/`nix eval` onto it | **-60** | three `attr_paths` tables and three candidate loops become one; the new op is ~120 lines against ~180 removed |
| 2 | `api::render` + `src/cli/render`: one JSON writer, one size and table formatter | -120 | nine hand-written renderers, two copies of `print_bytes`, one `render_size` |
| 3 | `api::session::Run`: the eval scaffold (evaluator, lints, cache, trace, parallel, report) as one call | -260 | 16 `run_parallel` call sites and 5 cache-hit branches become 1 |
| 4 | `api::build`: one dispatch path; `plumbing plan`, `missing`, `build` | -300 | 8 copies of the builder/transport/plan/upload/dispatcher sequence |
| 5 | `api::installable` grows the get-drvs walk and `outputsToInstall`; `nix-build`, `nix-shell`, `nix-instantiate`, `ietsp build`, `diff`, `prefetch` move onto it | -180 | gaps (a)1, (a)2, (a)3, (a)4 close; `classic::derivations` and `classic::plan_roots` lose their callers |
| 6 | `src/fetch` split out of `src/flake`; `plumbing fetch` | -40 | `prefetch`'s private use of `flake::spool`/`flake::http` |
| 7 | `source::SourceResolver`; `crate::flake` private; `plumbing flake-info`, `flake-tree` | -150 | one lock reader, one tree fetcher; `flake::root` and `flake::fetch` stop overlapping |
| 8 | `plumbing index`, `jobs`, `dev-env`, `drv-show`, `drv-diff`, `closure-diff`; porcelain becomes flags and rendering | +150 | the ops are new surface; the porcelain shrinks by about as much |
| 9 | `tests/layering.rs` and the allow-list; `crate::flake` and `crate::store` banned from `src/cli` | +60 | the test is new; it is what keeps stages 1-8 from rotting |

Net: about **-1100 lines** in `src`, with `docs/plumbing.md` as the
contract. The reduction is entirely the second copy: three attr-path
tables, eight build-dispatch sequences, sixteen eval scaffolds, nine
renderers. The new plumbing surface costs about 400 lines and is charged
above.

## Review checklist

Before merging a change under `src/cli`:

1. Does it call anything below `crate::api`? If yes, and it is not
   presentation, compatibility or interaction, the plumbing has a gap.
2. Does it do something a plumbing op already does, in its own way?
3. Does a new plumbing op have a JSON shape in this file and an e2e case?
4. Did the layering allow-list grow?

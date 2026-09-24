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

**Closed by stages 1 and 5.** Stage 1 made the three tables and the three
candidate loops one `api::installable`. Stage 5 moved the three walks that
turn the value found into derivations: `drv_roots` (out of
`crate::store::locate`, which five `src/cli` files reached past `crate::api`
to call), the `get-drvs.cc` walk (out of `cli/classic/mod.rs`) and
`outputsToInstall` (out of `cli/classic/nix.rs:target`, with its `Target`
type, now `installable::Install`). What is left in `src/cli` is a flake
reference becoming its outputs, which stage 8 takes.

### 2. Build dispatch

**Closed by stage 4.** `config` → `settings::builder` →
`Transport::parse_checked` → `plan::plan` → `PlanUpload::new` →
`Dispatcher::open` → `submit` was written out in `cli/build.rs`,
`cli/prefetch.rs`, `cli/classic/build.rs`, `cli/classic/instantiate.rs`,
`cli/classic/nix.rs`, `cli/classic/shell.rs`, `cli/devenv.rs` and
`cli/fastbuild/mod.rs`, with `classic::plan_roots` as a partial
extraction only the classic commands used. It is now `api::build`:
`transport`, `plan`/`plan_with`, and a `Dispatch` that owns the plan, the
`--source` overrides and the dispatcher, so no caller builds a
`PlanUpload`. `Dispatch::realise` plus `Built::paths` is the
merge-into-targets, submit, look-the-output-up block that `nix-build` and
`nix build` each wrote out, and `classic::Root` moved to `api::build`.

Two of the eight keep a connection of their own: `nix-shell` and
`print-dev-env` upload a plan and then speak the worker protocol
themselves (build the derivation's *inputs*, `execve` a shell; read the
env output back). They use `api::build::plan` and
`api::build::locate_store` and drive `PlanUpload` on that connection,
because routing them through `Dispatch` would open a second one. Stage 9's
`plumbing dev-env` is where that ends.

### 3. The evaluate-then-walk-then-report scaffold

**Closed by stage 3.** `a.evaluator()` → `configure_lints` →
`setup_cache` → cache hit → `ev.trace = enabled` → `run_parallel` →
`report(ev, &e)` was written out in `cli/eval.rs`, `cli/build.rs`,
`cli/search.rs`, `cli/devenv.rs`, `cli/jobs.rs`, `cli/diff.rs`,
`cli/prefetch.rs`, `cli/plumbing.rs`, `cli/fastbuild/mod.rs` and the four
classic commands: sixteen `run_parallel` call sites and four
`setup_cache` call sites, each with its own cache-hit branch. It is now
`api::run::Run`, which every one of them goes through.

### 4. Text-or-JSON rendering

**Closed by stage 2.** Hand-written per command:
`cli/build.rs:print_missing` and `render_size`, `cli/diff.rs:run` and
`closure_diff`, `cli/prefetch.rs:print_hash`, `cli/search.rs:show_results`,
`cli/jobs.rs:jobs_summary`, `cli/fastbuild/mod.rs:summary` and
`write_step_summary`, `cli/classic/instantiate.rs:print_bytes` (a copy of
`cli/classic/nix.rs:print_bytes`). There was no shared writer and no shared
size formatting. All nine now go through the two modules below.

#### What went below, and what stayed porcelain-side

The split is one question: **would a plumbing op emit this too?** If yes it
is a value, and it belongs in `api::render`. If it only reaches a terminal,
it is presentation, and it belongs in `src/cli/render`.

`api::render` (the operation layer) holds the trait `Report`, which gives a
value its JSON, and the four values whose JSON is already a contract in this
file:

| Value | The op it is the contract of | Was |
|:---|:---|:---|
| `Missing` | `missing` | `cli/build.rs:print_missing`, inline `json!` |
| `Closure` + `Change` | `closure-diff` | `cli/diff.rs:closure_diff`, an anonymous four-tuple |
| `Fetched` | `fetch` | `cli/prefetch.rs:print_hash`, inline `json!` |
| `impl Report for drvdiff::Diff` | `drv-diff` | `store/drvdiff.rs:json` |

`Missing::of` carries the two orderings — `willBuild` in the plan's order,
`willSubstitute` by store-path name — because they are part of the answer,
not of the layout. `Closure::of` carries the added/removed/changed verdict
for the same reason. `Diff` keeps only `equal` at the root: an input
derivation under it has the shape the recursion gives it.

`src/cli/render` (porcelain) holds everything a terminal needs, and may be
shared by every command:

* `bytes` — the one stdout writer, from the two `print_bytes` copies.
* `json` — the `nlohmann::json::dump` shape, from `search`'s `json_out`,
  `prefetch`'s and `plumbing resolve`'s `println!`.
* `pretty` — `--pretty`'s default, a terminal on stdout.
* `size` — `util.cc:renderSize`, from `cli/build.rs`.
* `status` — the one status line on stderr, for `jobs_summary` and
  `fast-build`'s two summaries.
* `Text` and `emit` — the `if json { … } else { … }` that was written four
  times, and the layout of each value above. `store::drvdiff`'s `text`,
  `line_diff`, `one_line`, `short` and `drv_name` moved here whole: a module
  below the command line does not format for a terminal.

`search`'s `--json` object stays in `search`, as this file already said:
regex matching, exclusion, colour and that object shape are presentation.
So do `jobs_summary` and `fast-build`'s report, which describe a run of a
command and not an operation. Note that real Nix writes its ANSI escapes to
stdout whether or not stdout is a terminal; `nix search` parity depends on
it, so `bytes` adds nothing and decides nothing.

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

`ietsp plumbing` is not in the top-level overview: that help is for
people, and the operations are for programs. It stays discoverable
through `ietsp plumbing --help` (and `ietsp help plumbing`), which lists
every op, and through this file, which is the contract.

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
ietsp plumbing resolve [EVAL FLAGS]
       [--prefixes package|dev-shell|package-set|none]
       [--want first|all] [--walk context|get-drvs|to-install]
       [--dump-drvs DIR] [--no-drvs]
```

`--prefixes package` is the default: `#hello` is tried at
`packages.<system>.hello`, then `legacyPackages.<system>.hello`, then
`hello`, as `nix build` tries it. The plumbing is opinionated, so the
common case needs no flag. `dev-shell` and `package-set` are the other two
`getActualAttrPaths` tables, and `none` makes the fragment the attribute
path itself. The three-table search stays available through the flag, and
stays the behaviour of the porcelain commands that need it.

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

`--walk` says how the value that was found becomes roots. The three modes
are the three walks the porcelain had:

| `--walk` | Nix | Who wanted it |
|:---|:---|:---|
| `context` (default) | `drvPath`, or the derivations in a string's context | `ietsp build`, `diff`, `prefetch` |
| `get-drvs` | get-drvs.cc:getDerivations, recursing with `recurseForDerivations` | `nix-build`, `nix-shell`, `nix-instantiate` |
| `to-install` | get-drvs.cc:queryOutputs: `outputSpecified`, else `meta.outputsToInstall`, else every output (`out` for a flake) | `nix build` |

`get-drvs` walks an attrset in lexicographic name order, takes a derivation
whole, and descends only where `recurseForDerivations = true` is set; a list
is always walked. `attr` is the path it reached each derivation by, so a
caller can name them as `nix-build` does. `to-install` answers one root per
output the installable installs.

`--dump-drvs DIR` is an evaluation flag, so every op that evaluates takes
it. It writes the ATerm of every derivation this run created to
`DIR/<base name>`, and `resolve` then reports the directory:

```json
{"attrPaths": [], "drvDir": "dump",
 "roots": [{"drv": "/nix/store/…​.drv", "output": null, "attr": null}]}
```

That is what makes the roots usable by another process; see
**Naming roots**.

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

### Naming roots

`plan`, `missing` and `build` take derivation paths. An argument is `DRV`
or `DRV!OUT[,OUT]`; naming no output means every output, as `!*` does in
the worker protocol. `--roots -` reads a previous op's stdout instead: the
`roots` of a `resolve`, or the `drvs` of a `plan`.

These three read the closure from `/nix/store` by default, so a root must be
there. `resolve` alone does not put it there — it evaluates and keeps the
derivations in memory, which is what lets `ietsp build` ship a closure that
was never written to disk.

`resolve --dump-drvs DIR` is how that closure crosses a process boundary.
`plan`, `missing` and `build` read the derivations from `DIR` when
`--drvs DIR` names it, or when `--roots -` reads a `resolve` whose output
carries `drvDir`. So this works with nothing in any store:

```console
$ ietsp plumbing resolve --dump-drvs d ./chain.nix | ietsp plumbing plan --roots -
{"drvs":["/nix/store/…-dep.drv","/nix/store/…-top.drv"],"sources":[],"storeDrvs":0}
```

`storeDrvs` is 0: no derivation was read from `/nix/store`. Without the
dump the same pipe fails, because the root is in no store:

```console
$ ietsp plumbing resolve ./chain.nix | ietsp plumbing plan --roots -
error: cannot read derivation '/nix/store/…-top.drv': No such file or directory
```

The dump carries derivations only. An input **source** that evaluation
produced and never wrote to the store is still read from the local store,
so a closure with such sources needs `ietsp build --no-build` first. That
is the remaining half of (a)3, and it is recorded, not designed, here: the
shape it wants is the batch form of `build --source STOREPATH=LOCAL`.

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
ietsp plumbing missing [--builder URL] [--check-cache-status] [--roots -] DRV...
```

```json
{"willBuild": […], "willSubstitute": […], "unknown": […],
 "downloadSize": 0, "narSize": 0}
```

This is byte-for-byte what `ietsp build --dry-run --json` prints today.
`--check-cache-status` adds `roots`, one object per root, with the keys
`eval-jobs` reports (gap (a)8):

```json
{"roots": [{"drv": "/nix/store/…​.drv", "cacheStatus": "notBuilt",
  "neededBuilds": ["/nix/store/…​.drv"], "neededSubstitutes": []}]}
```

### `build`

```
ietsp plumbing build [--builder URL] [--out NAME]... [--fetch-to DIR]
                     [--source STOREPATH=LOCAL]... [--gc-root DIR]
                     [--no-build] [--roots -] DRV...
```

```json
{"outcomes": [{"drv": "…", "success": true,
  "outputs": {"out": "/nix/store/…-hello-2.12.1"},
  "reason": null, "logTail": []}]}
```

The exit code follows docs/cli.md. Build progress goes to stderr through
the existing reporter; the renderer is porcelain.

`--out NAME` adds an output to every root. `--source STOREPATH=LOCAL`
uploads that path from a local tree (gap (a)4). `--gc-root DIR` links each
output at `DIR/<base name>` on the **local** daemon whatever the builder
is, which brings the closure back first, and adds `gcRoots` to the object
(gap (a)6). `--no-build` registers the closure and prints
`{"outcomes": []}`.

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

The capability is `api::fetch`: `url_hash` for a URL, `drv_hash` for the
fixed-output derivation an installable names, which builds it with a fake
hash and reads the mismatch back (gap (a)5 is still open: the mismatch comes
out of the log, not out of a structured outcome). `ietsp prefetch` is the
other frontend and is now the flag `--json` plus this op.

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
| `ietsp build` | `plan`, `missing`, `build` (through `api::build::Dispatch`) | `--dry-run` text, `render_size`, `fix-hashes` prompts, `.drv` legacy input |
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

1. ~~**The `get-drvs.cc` walk.**~~ **Closed by stage 5.**
   `classic::derivations` is `api::installable::get_drvs`, and `resolve`
   reaches it with `--walk get-drvs`. `nix-build`, `nix-shell` and
   `nix-instantiate` call the same function.
2. ~~**`outputsToInstall`.**~~ **Closed by stage 5.**
   `classic/nix.rs:target` is `api::installable::to_install`, and
   `resolve --walk to-install` answers one root per output the installable
   installs. `classic/nix.rs:Target` went with it, as
   `installable::Install`.
3. **The ATerm table of a run.** **Mostly closed by stage 5.** A derivation
   that was never written to the store has no ATerm on disk.
   `resolve --dump-drvs DIR` writes the run's whole table and reports
   `drvDir`; `plan`, `missing` and `build` read it, through `--drvs DIR`
   or through the `drvDir` on stdin, so `resolve | plan` composes across
   processes. `api::build::drvs_in_dir` is the reader, and
   `api::build::drv_info` is the one ATerm-to-`DrvInfo` conversion, which
   `ietsp build`'s `.drv` argument shares. What is left is the **sources**:
   the dump has none, so a closure whose sources are not in the local store
   still needs an instantiation first. `ietsp diff` keeps its in-memory
   table, because it evaluates and compares in one process; `drv-show`
   (stage 9) reads the directory.
4. ~~**Uploading with a local source override.**~~ **Closed by stage 4.**
   The map lives in `api::build::Dispatch::overrides`, and `build` takes
   `--source STOREPATH=LOCAL`. `plan` does not need it: a plan names the
   paths, and only the upload reads them.
5. **Hash mismatch capture.** `ietsp prefetch` drives a build that is
   meant to fail, and reads the mismatch out of the log
   (`log::want_mismatch`, `log::take_mismatches`). `build` must report a
   mismatch as structured output, not as a log line the caller greps.
   `ietsp build`'s `fix-hashes` reads the same channel.
6. ~~**GC roots and out-links.**~~ **Closed by stage 4.** `build` takes
   `--gc-root DIR` and reports `gcRoots`, over `Backend::out_links`, which
   already copied the closure back to the local daemon before linking.
   `nix-instantiate --add-root` goes through `Dispatch` too. What the
   porcelain keeps is the *naming* of the links (`result-2-dev`,
   `--add-root` numbering), which is a user interface, per (b)2.
7. **The flake source resolver.** See below. `flake-tree` does not exist;
   every caller goes through `flake::eval_flake`, which fetches as a side
   effect of evaluation.
8. ~~**`--check-cache-status`.**~~ **Closed by stage 4.**
   `missing --check-cache-status` answers `cacheStatus`, `neededBuilds`
   and `neededSubstitutes` per root, from `jobs::cache::Resolver`. It
   reads the roots' derivations from `/nix/store`, as `missing` does.
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
for the shared writer and the text layouts (stage 2), `src/cli/classic/`
for compatibility, `src/cli/interactive/` for prompts. They may be shared
between porcelain commands.

## How the rule is enforced

1. **A module boundary, checked by a test.** `tests/layering.rs` reads
   `src/cli/**/*.rs` and fails when a file names
   `crate::eval`, `crate::store`, `crate::build`, `crate::remote`,
   `crate::flake`, `crate::jobs`, `crate::devenv`, `crate::trace`,
   `crate::compile` or `crate::parallel`, outside an explicit allow-list
   keyed by file. The allow-list starts as today's set and may only
   shrink; the test asserts its length against a constant, so an entry
   that is removed cannot come back unnoticed.
2. **Types.** The end state is that `Word` (an evaluator value) does not
   appear in a `src/cli` signature: a porcelain command that cannot name
   a value cannot evaluate one. The same test greps for it, against the
   same shrinking allow-list, so the rule can be adopted file by file.
   Note what this does *not* ban: a porcelain-side library that renders,
   parses Nix argv or prompts. Those may be as large as they need to be.
   The test asks one question only — is this a second implementation of
   something the operation layer owns?
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

`src/flake` was 4689 lines in 8 files, and is 2595 in 5 after stages 6 and
7. It reaches into `crate::treestore` (the tree store),
`crate::builtins::store_hooks`, `crate::eval` and, through `crate::fetch`,
HTTP. `crate::flake::` was named 29 times outside `src/flake` and is named
22, in `src/cli/search.rs`, `src/cli/classic/nix.rs`, `src/cli/diff.rs`,
`src/cli/mod.rs`, `src/builtins/paths.rs`, `src/eval/mod.rs`,
`src/api.rs`, `src/api/installable.rs`, `src/fetch/mod.rs` (a doc
comment) and `src/treestore/mod.rs` (a doc comment). Four of those are
`is_flake_ref`, three are `parse_attr_path`, three are `select`, and
`src/cli/prefetch.rs` reached into `flake::spool` and `flake::http` for a
job that has nothing to do with flakes. **Stage 6 closed that one:** the
three modules are `src/fetch` now, and `prefetch` calls `plumbing fetch`.

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

### The input store is a store, and it should say so once

Two different things are called a store here.

* **Content we own, on local disk.** `src/cachefs.rs` (581 lines) is
  already the shared discipline: a version file, stage and rename, one
  writer under `flock`, a TTL sweep, poison and wipe on doubt. The IR
  cache, the tracing store and the input store all follow it.
* **A Nix store reached over a protocol.** `src/remote/` — the daemon,
  `ssh-ng`, `grpc`. A different trust model. Not ours.

The input store straddles the two: it is (a) by storage and (b) by
content, because it holds NAR-shaped trees under Nix store path names.
That is why `src/flake/spool.rs` (892 lines) carries NAR and store-path
logic that partly mirrors `src/store/nar.rs` (956) and `src/store/path.rs`
(473). Three modules know how a store path is built; two know how a tree
becomes a NAR, which is why `spool_matches_the_tree` exists as a parity
test between them. A third naming site, `flake::fetch::source_store_path`,
sits beside `store::path::make_store_path` and
`store::drv::fixed_output_path`.

Propose one named abstraction: a **local content-addressed tree store**,
on top of `cachefs`. Put a tree in, get a store path. Read bytes back for
evaluation. Stream a NAR out for an upload. Nothing else.

```rust
/// Anything that can be walked as a tree: a spooled archive, a directory
/// on disk, a single file. One NAR writer serves all of them.
pub trait TreeSource {
    fn stat(&self, rel: &[u8]) -> Option<FileType>;
    fn list(&self, rel: &[u8]) -> Option<Vec<(Vec<u8>, FileType)>>;
    fn read(&self, rel: &[u8]) -> io::Result<Vec<u8>>;
}

pub trait TreeStore {
    /// Put a tree in. `expect` is the hash the caller already knows: a
    /// lock entry's `narHash`, or a derivation's `outputHash`. There is
    /// no `put` without it.
    fn put(&self, name: &str, expect: &Hash, src: &dyn TreeSource)
        -> Result<StoredTree>;
    fn get(&self, path: &StorePath) -> Option<StoredTree>;
    fn has(&self, path: &StorePath) -> bool;
}

impl StoredTree {
    pub fn path(&self) -> &StorePath;
    pub fn nar_hash(&self) -> &Hash;
    pub fn nar_size(&self) -> u64;
    /// Evaluation reads through this. Nothing is materialised.
    pub fn read(&self, rel: &[u8]) -> io::Result<Vec<u8>>;
    pub fn stat(&self, rel: &[u8]) -> Option<FileType>;
    pub fn list(&self, rel: &[u8]) -> Option<Vec<(Vec<u8>, FileType)>>;
    /// An upload streams this.
    pub fn dump(&self, filter: &PathFilter, sink: &mut dyn Write)
        -> io::Result<()>;
}
```

`src/inputs.rs` becomes its only current user, and shrinks to the lookup
by lock entry. `src/flake/spool.rs` keeps the part that is really its own
— indexing a decompressed tar so members can be read back by offset — and
implements `TreeSource`. Its NAR writer, its index codec and its
`persist`/`reopen` pair move into the tree store, where one copy serves
every source. `store::nar`'s writer becomes the writer, over the trait.
`source_store_path` becomes the store's naming function.

The flake layer then depends on `TreeStore` and `SourceResolver`, not on
`flake::fetch`, `flake::spool` or `crate::inputs` internals.

**What stage 7 built instead, and why.** `src/inputs.rs` is gone whole:
`crate::treestore` is the store, and `StoredTree` — not `spool::Node` — is
what `builtins::store_hooks` reads through. Three things above were not
built, each for a reason the measurement supports:

* **No `trait TreeStore`.** There is one store and there will be one store;
  a trait with a single implementation is the abstraction the line budget
  says to inline. What the design wanted from it is in the signature:
  `put(name, expect, staged, tree)` hashes what it is given, refuses
  anything else, and names the entry by what it hashed.
* **No `trait TreeSource` yet.** It would have one implementor today and no
  second caller, which is stage 1's lesson. `StoredTree`'s one field is the
  seam: a second source makes it an enum plus a format byte in the index,
  about 30 lines, and every caller already goes through `StoredTree`.
* **The index codec and `persist`/`reopen` stayed in `fetch::spool`.** They
  serialize *that* type's nodes. Moving them would have made the tree store
  know a tar index, and it would have moved 200 lines without deleting one.

One NAR writer was built, and it is `nar::TreeWriter`: the framing, the
buffering and the content copy. The directory walk, the archive index and
`dump_string` drive it and keep their own traversal, because the traversal
is the part that genuinely differs — `readdir` with `d_type` and a name
arena on one side, a sorted index in memory on the other. Routing the
directory walk through an accessor trait would have cost an allocation per
entry on the hottest copy path in the evaluator. `spool_matches_the_tree`
stays: it is now a parity test between the two *walks*, not between two
writers.

Expected reduction, against what stage 7 measured:

| Module | Now | Expected | Measured | Delta |
|:---|---:|---:|---:|---:|
| `src/flake/spool.rs` → `src/fetch/spool.rs` | 892 | ~520 | 889 | -3 |
| `src/inputs.rs` | 280 | ~90 | 0 | -280 |
| `src/store/nar.rs` | 956 | ~870 | 987 | +31 |
| `src/flake/fetch.rs` (naming, and `put`) | — | — | — | -23 |
| `src/builtins/store_hooks.rs` (the spool arms) | — | — | — | -11 |
| `src/treestore/` (new) | 0 | ~380 | 402 | +402 |
| other callers | | | | +1 |
| | | | | **+117** |

`src/cachefs.rs` does not change: the tree store is a user of it, like the
IR cache and the tracing store. What the table shows is rule 2 again, from
the other side: **a move is not a deletion.** -372 for `spool.rs` and -190
for `inputs.rs` were both the cost of code moving to `src/treestore`, which
is charged +380 in the same table and came in at 402. The only real
deletions in the stage are the second NAR framing (-84 in `spool.rs`), one
of the naming wrappers, and the `Node` matching that left `store_hooks`.

#### Consequence, recorded and not designed here

The store is fixed-output only, and that is exactly what makes it safe in
user space. A fixed-output hash is known **before** the fetch, so the
content verifies itself: no signature, no trusted builder, no daemon. The
interface above makes that a property of the type, not a policy check —
`put` takes `expect` and has no overload without it. Nothing that lacks a
prior hash can be admitted.

Today it holds flake inputs only. Every other FOD — `fetchurl`,
`fetchFromGitHub`, `fetchzip`, any derivation with `outputHash` — has the
same shape: a name, a hash known in advance, and a byte stream. Covering
them would be `put` called from `builtins:fetchurl` and from the
fixed-output build path with the derivation's `outputHash`, plus a policy
setting for what the store is allowed to hold. It is a change to one
implementation, not a new store. The proposed interface is already keyed
on exactly a FOD's identity, so yes: small.

**Still true against the built interface.** `put(name, expect, staged, tree)`
is keyed on exactly a FOD's identity, and `fetchurl` has one: a name, an
`outputHash` known before the fetch, and a byte stream. What is missing is
not the store but a second `TreeSource`: today a `StoredTree` holds a
spooled archive, and a flat file or an unpacked directory needs that field
to become an enum with a format byte in the index — about 30 lines, plus
the policy setting for what the store may hold. No caller changes: every
reader already goes through `StoredTree`.

### Migration order

1. ~~Move `flake::http`, `flake::spool` and `flake::unpack` to `src/fetch`.~~
   **Done in stage 6**, at +5 lines: the three files moved and `src/fetch`
   got a `mod.rs`. No caller changed except the module path.
2. ~~`src/treestore` on `cachefs`.~~ **Done in stage 7**, at +117 lines;
   what was and was not built is above.
3. Introduce `SourceRef`, `Tree` and `SourceResolver` in `src/source`,
   implemented by today's `flake::fetch` over the tree store.
   `flake::fetch_tree` becomes a thin call.
4. Move `is_flake_ref`, `parse_attr_path` and `select` to `api::flake`.
   The six `src/cli` call sites change to `api::`.
5. `flake::lock` becomes `source::Lock`, with `flake-info` on top.
6. `crate::flake` becomes `pub(crate)`; the layering test adds it to the
   banned list.

Steps 1 and 2 are in; steps 3 to 6 are stage 8.

## Staged migration

Every stage is one merge, and the checks pass at its commit.

| # | Stage | src delta | Where the reduction comes from |
|--:|:---|---:|:---|
| 1 | `api::installable` + `plumbing resolve`; move `nix build`/`nix eval`, `ietsp search` and `print-dev-env` onto it | **+220 measured** | three `attr_paths` tables and three candidate loops (-84) become one 127-line module; `plumbing resolve` is +156 of new surface |
| 2 | `api::render` + `src/cli/render`: one JSON writer, one size and table formatter | **+178 measured** (+171 outside tests) | nine renderers move; the saving is one `print_bytes`, one JSON dump and four `if json` branches (-60), against two new modules (+210) |
| 3 | `api::run::Run`: the eval scaffold (evaluator, lints, cache, trace, parallel, report) as one call | **-7 measured** | the scaffold lines themselves are only -100; the rest is four per-command `Failed`/`Unsupported` enums and their error matches, against a 102-line module |
| 4 | `api::build`: one dispatch path; `plumbing plan`, `missing`, `build` | **+417 measured** | the refactor is +33: the eight sites shed 220 lines against a 187-line module. The three ops are +384, of which about 80 is the three gaps they close |
| 5 | `api::installable` grows the get-drvs walk and `outputsToInstall`; `nix-build`, `nix-shell`, `nix-instantiate`, `ietsp build`, `diff`, `prefetch` move onto it | **+113 measured** | the refactor is -4: three capabilities moved below the command line and there was one copy of each. Gaps (a)1, (a)2 and most of (a)3 close, at +120 of new surface |
| 6 | `src/fetch` split out of `src/flake`; `plumbing fetch` | **+68 measured** (+66 outside tests) | the move is +5; the op is one capability with two frontends, and `prefetch` sheds 177 lines against a 128-line `api::fetch` and a 110-line op |
| 7 | `src/treestore` on `cachefs`; `inputs.rs` collapses into it | **+117 measured** (+115 outside tests) | the two NAR framings did become one (-84 in the spool, +70 in `nar::TreeWriter`); the rest was a move: `inputs.rs` -280 against a 402-line module |
| 8 | `source::SourceResolver`; `crate::flake` private; `plumbing flake-info`, `flake-tree` | -150 | one lock reader, one tree fetcher; `flake::root` and `flake::fetch` stop overlapping |
| 9 | `plumbing index`, `jobs`, `dev-env`, `drv-show`, `drv-diff`, `closure-diff`; porcelain becomes flags and rendering | +150 | the ops are new surface; the porcelain shrinks by about as much |
| 10 | `tests/layering.rs` and the allow-list; `crate::flake` and `crate::store` banned from `src/cli` | +60 | the test is new; it is what keeps stages 1-9 from rotting |

The estimates below said about **-450 lines** in `src`. Stages 1 to 7
measured +220, +178, -7, +417, +113, +68 and +117, so that number is
wrong; see the next sections for what replaces it. The reduction that is
real is the second copy: three attr-path tables, eight build-dispatch
sequences, sixteen eval scaffolds, two NAR framings. It was not nine
renderers, and it was not the modules a stage merely moves. The new
plumbing surface costs far more than the 550 lines charged above.

### What stages 1, 2, 3 and 4 measured, and what it means for the rest

Stage 1 estimated -60 and measured +220. Stage 3 estimated -260 and
measured -7. Stage 4 estimated -300, was re-estimated at "positive unless
the collapse works", and measured +417. Stage 2 estimated -120, was
re-estimated at -50, and measured +178. Two things were wrong in the first
estimates, and they are wrong in the same direction everywhere else:

1. **A shared module costs 100-150 lines before it saves one.**
   `api::installable` is 175, `api::run` is 102. A stage that removes
   N copies of a K-line sequence nets `N*K - 150`, not `N*K`.
2. **Repeated lines are cheap to count and cheap to keep; repeated
   *types* are what cost.** Folding the cache lookup, the trace switch,
   the lint call and `run_parallel` into one call saved about 100 lines
   over sixteen sites — 6 lines each. Deleting four per-command
   `enum Run { Failed, Unsupported, … }` and the `if is_unsupported(&e)`
   match that fed each one saved as much again, from five sites. Look
   for the type, not for the line.

**Stage 2's nine renderers were nine different outputs.** They were not
nine copies of one renderer, which is what -120 and the -50 re-estimate
both assumed. What actually repeated across them was small: one
`print_bytes`, one JSON dump and four `if json { … } else { … }` branches,
about 60 lines in all. Only one per-command output type existed to delete
— `diff`'s anonymous four-tuple of closure rows — and it became `Closure`,
which is larger than the tuple was because it is now a contract. About 120
of the new lines are `store::drvdiff`'s text renderer moving into
`src/cli/render` unchanged: a boundary fix that is a wash on the ledger
and the point of the stage. `Missing`, `Closure` and `Fetched` each pay
for one call site today; stages 4, 6 and 9 are where they earn their cost,
because those ops then have no JSON of their own to write.

**Stage 4's collapse worked and was still not enough.** `plan_roots`,
`PlanUpload::new` and the `Dispatcher::open`/`submit` pair did collapse
into one call that the classic `realise` functions use, and the per-command
types around it went with them: `classic::Root` moved into `api::build`,
`nix.rs:flat_roots` lost its dedup, and the merge-into-targets loop, the
`HashMap<String, BTreeMap<String, Option<String>>>` and the
"derivation has no output" match — written out twice, ~45 lines each —
became `realise` plus `Built::paths`. That is what turned the -80 the plan
predicted into -220. The module still cost 187, so the refactor is +33.

Two more rules follow, and they are the ones that decide stages 5 to 9:

3. **The plumbing op costs more than the operation it exposes.** The
   three ops are 384 lines against a dispatch layer they call in four
   lines each. Flag parsing, the root vocabulary (`DRV!OUT`, `--roots -`),
   the help text and the JSON shaping are the op; the capability was
   already there. A stage that adds ops nets positive unless the porcelain
   it replaces is deleted in the same stage. Stage 9 is charged +150 for
   six ops on the same arithmetic that cost 384 for three; read it as
   +400 unless the porcelain goes with it.
4. **Count the copies, not the call sites.** Nine renderers, sixteen eval
   scaffolds and eight dispatch sequences are three different numbers of
   copies: sixteen and eight were copies, nine were not. Before estimating
   a stage, say what the second copy is. If the answer is "they all do the
   same *kind* of thing", the saving is the kind, not the thing.

Stage 8 is the last stage charged a reduction for moving code, and stages
5, 6 and 7 all say the same thing about that charge: read it as about 0,
not -150, unless the second copy is named before the work starts. It also
adds two ops, so rule 3 applies on top.

On the measured numbers the net across all ten stages is about **+700**,
not -450. The migration is still worth it — one implementation per
capability, and a contract programs can call — but it is not a line-count
win, and the plan should stop claiming one.

Stage 1 was the first stage that was net positive, and it was honest about
why: a shared value pays for itself at its second caller, and stages 1 and
2 both built values that have only their first. Stage 5 is where
`api::installable` earned its cost: the module grew from 175 lines to 510,
and ten `src/cli` files name it — every command that turns an installable
into derivations. Stages 6 and 9 are where `api::render` earns its.

### What stage 5 measured

Estimated -180, re-estimated at about +100 before the work started,
measured **+113**. **Stage 5 had no second copy at all, and rule 4 said so
in advance.** The plan charged -180 to "`classic::derivations` and
`classic::plan_roots` lose their callers". Losing a caller deletes nothing:
the walk is still one function, and it had exactly one copy.
`classic::plan_roots` was already gone, taken by stage 4.

What actually repeated was one level down: reading a derivation's
attributes — `type`, `drvPath`, `outputName`, a list of strings — written
three times across `derivations`, `target` and `drv_roots`, and twice more
inside `target` itself. Folding those into `is_derivation`, `drv_of`,
`output_name` and `string_list` paid for the module's growth and made the
refactor -4 instead of +100. The rest is the two new flags and the
composition, at +120.

The boundary fix is the point, not the ledger. Before stage 5 the
`recurseForDerivations` walk lived in `src/cli/classic/mod.rs` and
`outputsToInstall` in `src/cli/classic/nix.rs`, so a capability was
reachable only by the command that happened to hold it, and five porcelain
commands reached into `crate::store::locate` for `drv_roots`. That is what
moved. It is the same shape as stage 2's `store::drvdiff` text renderer: a
wash on the ledger and the reason for the stage.

### What stage 6 measured

Estimated -40, measured **+68** (+66 outside tests). `api::render::Fetched`
did earn its cost — `plumbing fetch` writes no JSON of its own, and the two
frontends emit the same value — but that is 8 lines saved, not 40, and rule
3 held: the op is 110 lines of flag parsing, help text and dispatch over a
128-line capability that already existed. What the stage really bought is
that `prefetch` is 34 lines: it parses `--json` and calls the op. The move
itself was +5, as predicted, because a move is not a deletion. That is the
warning for stage 7, which is charged -300 for moves.

### What stage 7 measured

Estimated -300, measured **+117** (+115 outside tests). This was the stage
with per-module counts instead of call-site counts, and the counts were
right about the *modules*; they were wrong about what a module count means.
A fifth rule follows, and it is the one the estimate needed:

5. **A move is not a deletion.** -372 for `spool.rs` and -190 for
   `inputs.rs` were the same lines the table charged +380 for under
   `src/treestore/`. Netting a move against its destination is how -0 turns
   into -562 on paper. Count only what no longer exists anywhere.

Of the three things the stage called duplicates, one was:

* **Two NAR writers: yes, and they are one.** `fetch::spool` framed its own
  NARs with `write_str`; `nar::TreeWriter` is now the only place NAR bytes
  are framed, and `write_str`, `write_u64` and `write_padding` are gone with
  it. That is -84 in the spool against +70 for the writer, so the deletion
  is real and it is 14 lines. What it actually bought is elsewhere: the
  spool's dump gained the buffered sink, and a second tree source will not
  need a NAR writer.
* **Three store-path naming sites: they were one and two wrappers.**
  `make_store_path` is the only implementation; `make_fixed_output_path`
  is the FOD rule over it, and `source_store_path` was a 9-line wrapper.
  Three *callers* of the recursive-no-references form became
  `treestore::store_path`, which is -6 lines, not -30. A wrapper is not a
  copy.
* **One entry codec: there was only ever one.** It serializes
  `spool::Node`, so it stayed with the type it serializes. Moving it would
  have moved 200 lines and deleted none, and it would have made the tree
  store know what a tar index is.

What the stage is worth is not on the ledger: `src/inputs.rs` was a
content-addressed store nobody had named, `builtins::store_hooks` matched
on the spool's node type through it, and `put` could be handed a name and
a hash to believe. Now the store has a name, `StoredTree` is the only way
through it, and `put(name, expect, …)` hashes what it is given and refuses
anything else. Stage 8 can put a `SourceResolver` over that; it could not
have gone over `crate::inputs`.

The correction stage 4 left open is closed: `plumbing missing` assembled its
own `{"willBuild", …}` object and did not sort, so it was not byte-for-byte
`ietsp build --dry-run --json`, which this file says it must be. The daemon
answers `QueryMissing` with a lexicographic `StorePathSet`, so the two
differed whenever a dependency sorted after its dependent. The op now goes
through `api::render::Missing::of`, which carries the two orderings, and the
e2e case compares the two commands byte for byte.

## Review checklist

Before merging a change under `src/cli`:

1. Does it call anything below `crate::api`? If yes, and it is not
   presentation, compatibility or interaction, the plumbing has a gap.
2. Does it do something a plumbing op already does, in its own way?
3. Does a new plumbing op have a JSON shape in this file and an e2e case?
4. Did the layering allow-list grow?

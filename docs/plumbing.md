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

**Closed by stage 3.** `a.evaluator()` → `configure_lints` →
`setup_cache` → cache hit → `ev.trace = enabled` → `run_parallel` →
`report(ev, &e)` was written out in `cli/eval.rs`, `cli/build.rs`,
`cli/search.rs`, `cli/devenv.rs`, `cli/jobs.rs`, `cli/diff.rs`,
`cli/prefetch.rs`, `cli/plumbing.rs`, `cli/fastbuild/mod.rs` and the four
classic commands: sixteen `run_parallel` call sites and four
`setup_cache` call sites, each with its own cache-hit branch. It is now
`api::run::Run`, which every one of them goes through.

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
       [--want first|all] [--no-drvs]
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

Expected reduction:

| Module | Now | After | Delta |
|:---|---:|---:|---:|
| `src/flake/spool.rs` | 892 | ~520 | -372 |
| `src/inputs.rs` | 280 | ~90 | -190 |
| `src/store/nar.rs` | 956 | ~870 | -86 |
| `src/flake/fetch.rs` (naming) | — | — | -30 |
| `src/treestore/` (new) | 0 | ~380 | +380 |
| | | | **-298** |

`src/cachefs.rs` does not change: the tree store is a user of it, like the
IR cache and the tracing store.

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

### Migration order

1. Move `flake::http`, `flake::spool` and `flake::unpack` to `src/fetch`.
   They are an HTTP client, a spooled tree and an archive unpacker. No
   caller changes except the module path. **-0 lines, 3 files move.**
2. `src/treestore` on `cachefs`: `TreeSource`, `TreeStore`, `StoredTree`.
   `fetch::spool::Tree` implements `TreeSource`; the NAR writer and the
   index codec move in; `inputs.rs` becomes a lookup.
3. Introduce `SourceRef`, `Tree` and `SourceResolver` in `src/source`,
   implemented by today's `flake::fetch` over the tree store.
   `flake::fetch_tree` becomes a thin call.
4. Move `is_flake_ref`, `parse_attr_path` and `select` to `api::flake`.
   The six `src/cli` call sites change to `api::`.
5. `flake::lock` becomes `source::Lock`, with `flake-info` on top.
6. `crate::flake` becomes `pub(crate)`; the layering test adds it to the
   banned list.

Nothing in this section is implemented yet.

## Staged migration

Every stage is one merge, and the checks pass at its commit.

| # | Stage | src delta | Where the reduction comes from |
|--:|:---|---:|:---|
| 1 | `api::installable` + `plumbing resolve`; move `nix build`/`nix eval`, `ietsp search` and `print-dev-env` onto it | **+220 measured** | three `attr_paths` tables and three candidate loops (-84) become one 127-line module; `plumbing resolve` is +156 of new surface |
| 2 | `api::render` + `src/cli/render`: one JSON writer, one size and table formatter | -120 | nine hand-written renderers, two copies of `print_bytes`, one `render_size` |
| 3 | `api::run::Run`: the eval scaffold (evaluator, lints, cache, trace, parallel, report) as one call | **-7 measured** | the scaffold lines themselves are only -100; the rest is four per-command `Failed`/`Unsupported` enums and their error matches, against a 102-line module |
| 4 | `api::build`: one dispatch path; `plumbing plan`, `missing`, `build` | -300 | 8 copies of the builder/transport/plan/upload/dispatcher sequence |
| 5 | `api::installable` grows the get-drvs walk and `outputsToInstall`; `nix-build`, `nix-shell`, `nix-instantiate`, `ietsp build`, `diff`, `prefetch` move onto it | -180 | gaps (a)1, (a)2, (a)3, (a)4 close; `classic::derivations` and `classic::plan_roots` lose their callers |
| 6 | `src/fetch` split out of `src/flake`; `plumbing fetch` | -40 | `prefetch`'s private use of `flake::spool`/`flake::http` |
| 7 | `src/treestore` on `cachefs`; `inputs.rs` and `spool.rs` collapse into it | -300 | two NAR writers, three store-path naming sites, one entry codec |
| 8 | `source::SourceResolver`; `crate::flake` private; `plumbing flake-info`, `flake-tree` | -150 | one lock reader, one tree fetcher; `flake::root` and `flake::fetch` stop overlapping |
| 9 | `plumbing index`, `jobs`, `dev-env`, `drv-show`, `drv-diff`, `closure-diff`; porcelain becomes flags and rendering | +150 | the ops are new surface; the porcelain shrinks by about as much |
| 10 | `tests/layering.rs` and the allow-list; `crate::flake` and `crate::store` banned from `src/cli` | +60 | the test is new; it is what keeps stages 1-9 from rotting |

Net: about **-450 lines** in `src` on the estimates below, with
`docs/plumbing.md` as the contract. The reduction is the second copy:
three attr-path tables, eight build-dispatch sequences, sixteen eval
scaffolds, nine renderers, two NAR writers. The new plumbing surface
costs about 550 lines and is charged above.

### What stages 1 and 3 measured, and what it means for the rest

Stage 1 estimated -60 and measured +220. Stage 3 estimated -260 and
measured -7. Two things were wrong in both estimates, and they are wrong
in the same direction everywhere else:

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

Stages 2 and 4 are the two that read like stage 3's estimate. Stage 2 is
nine renderers of about 15 lines against a new writer module; on the
arithmetic above it is nearer -50 than -120 unless it also deletes the
per-command output types. Stage 4 is eight copies of a 10-line dispatch
sequence (-80), a shared module (+130) and three new plumbing ops
(+150): as written it is positive, and it only goes negative if
`plan_roots`, `PlanUpload::new` and the `Dispatcher::open`/`submit`
pair collapse into one call that the classic `realise` functions also
use. Stages 5 to 8 are backed by per-module counts and stand.

Stage 1 is the one stage that is net positive, and it is honest about
why: the shared resolver pays for itself only at three call sites today,
while the plumbing command is entirely new surface. Stage 5 is where that
module earns its cost.

## Review checklist

Before merging a change under `src/cli`:

1. Does it call anything below `crate::api`? If yes, and it is not
   presentation, compatibility or interaction, the plumbing has a gap.
2. Does it do something a plumbing op already does, in its own way?
3. Does a new plumbing op have a JSON shape in this file and an e2e case?
4. Did the layering allow-list grow?

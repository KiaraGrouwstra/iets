# Plumbing operations

Every `ietsp plumbing` operation: its contract, its flags, the exact JSON
it writes, its exit codes, and how the operations compose.

```
ietsp plumbing OP [FLAGS] ...
```

Each op reads flags, and optionally JSON on stdin, and writes one JSON
object on stdout. Nothing else reaches stdout. Errors go to stderr as
`log::Report` records and set the exit code
(`docs/reference/errors.md`). Why the operations exist and where the
boundary between them and the user-facing commands runs:
`docs/explanation/plumbing-and-porcelain.md`.

## The operations

| Op | Contract |
|:---|:---|
| `resolve` | an installable becomes derivation roots |
| `plan` | derivation roots become an upload plan |
| `missing` | a plan and a builder become what will be built and fetched |
| `build` | derivation roots are built; outcomes come back |
| `fetch` | a URL or a fixed-output derivation becomes a hash |

These five exist. `ietsp plumbing --help` lists them, and an unknown op
is the usage error `unknown subcommand 'X'`, with a suggestion when it is
close to one of the five.

## Shared input

Every op that evaluates — `resolve`, and `fetch` in its expression form —
takes the shared evaluation flags of `docs/reference/commands.md`: `-E
EXPR`, `FILE` (or `-f FILE`, the same thing), `FLAKEREF[#FRAGMENT]`,
`-A PATH`, `--arg`, `--argstr`,
`-I`, `--impure`/`--pure-eval`, `--option`, `--workers`, `--no-cache`,
`--cache-dir`, `--log-format`, `--show-trace`, `--dump-drvs`. One parser
serves all of them (`api::EvalArgs`).

`-A PATH` and a flake fragment walk the value through
`api::installable::find_along_attr_path`, the port of
`attr-path.cc:findAlongAttrPath`: every step auto-calls what it is
standing on, so a file whose top level is a function (nixpkgs'
`default.nix`) is called with the `--arg`/`--argstr` set before an
attribute is taken from it. A formal with a default that is not supplied
is fine; a required formal that is not supplied is the error; formals
that end in `...` are handed every argument, named or not
(`eval.cc:autoCallFunction`). The classic names, `search` and every
native command share that one walk.

## Naming roots

`plan`, `missing` and `build` take derivation paths. An argument is `DRV`
or `DRV!OUT[,OUT]`; naming no output means every output, as `!*` does in
the worker protocol. `--roots -` reads a previous op's stdout instead:
the `roots` of a `resolve`, or the `drvs` of a `plan`. Stdin with neither
key is the usage error `--roots -: no "roots" and no "drvs" on stdin`.

The three read the closure from `/nix/store` by default, so a root must
be there. `resolve` alone does not put it there — it evaluates and keeps
the derivations in memory, which is what lets `ietsp build` ship a
closure that was never written to disk. A root that is in no store is the
error `cannot read derivation 'PATH': No such file or directory`, exit 1.

`resolve --dump-drvs DIR` is how that closure crosses a process boundary.
`plan`, `missing` and `build` read the derivations from `DIR` when
`--drvs DIR` names it, or when `--roots -` reads a `resolve` whose output
carries `drvDir`.

The dump carries derivations only. An input **source** that evaluation
produced and never wrote to the store is still read from the local store,
so a closure with such sources needs `ietsp build --no-build` or
`ietsp plumbing build --no-build` first.

## `resolve`

An installable becomes derivation roots.

```
ietsp plumbing resolve [EVAL FLAGS]
       [--prefixes package|dev-shell|package-set|none]
       [--want first|all] [--walk context|get-drvs|to-install]
       [--dump-drvs DIR] [--no-drvs]
```

| Flag | Effect |
|:---|:---|
| `--prefixes WHICH` | the attribute paths a flake fragment is tried at |
| `--want first\|all` | stop at the first path that is there, or take every one |
| `--walk HOW` | how the value that was found becomes roots |
| `--dump-drvs DIR` | write the ATerm of every derivation this evaluation made to `DIR`, and report `DIR` as `drvDir` |
| `--no-drvs` | report the attribute paths only, and stop after the value |

`--prefixes package` is the default: `#hello` is tried at
`packages.<system>.hello`, then `legacyPackages.<system>.hello`, then
`hello`, as `nix build` tries it. `dev-shell` and `package-set` are the
other two `getActualAttrPaths` tables, and `none` makes the fragment the
attribute path itself.

`--walk` picks one of the three walks Nix has:

| `--walk` | Nix | Used by |
|:---|:---|:---|
| `context` (default) | `drvPath`, or the derivations in a string's context | `ietsp build`, `diff`, `prefetch` |
| `get-drvs` | get-drvs.cc:getDerivations, recursing with `recurseForDerivations` | `nix-build`, `nix-shell`, `nix-instantiate` |
| `to-install` | get-drvs.cc:queryOutputs: `outputSpecified`, else `meta.outputsToInstall`, else every output (`out` for a flake) | `nix build` |

`get-drvs` walks an attribute set in lexicographic name order, takes a
derivation whole, and descends only where `recurseForDerivations = true`
is set; a list is always walked.

**Output.**

```json
{"attrPaths":["packages.x86_64-linux.hello"],
 "roots":[{"attr":"hello","drv":"/nix/store/…-hello-2.12.1.drv","output":"out"}]}
```

| Key | Value |
|:---|:---|
| `attrPaths` | the candidate attribute paths that were tried, in order, so a caller can report `does not provide attribute` itself; empty for a file or an expression |
| `roots` | one object per derivation root |
| `roots[].drv` | the derivation path |
| `roots[].output` | the output name, or `null` when the value named the derivation and not one output |
| `roots[].attr` | the attribute path the walk reached the derivation by, or `null` |
| `drvDir` | with `--dump-drvs DIR` only: `DIR` |

Exit: 0; 1 on an evaluation error; 2 on a usage error.

## `plan`

Derivation roots become an upload plan.

```
ietsp plumbing plan [--drvs DIR] [--roots -] DRV...
```

```json
{"drvs":["/nix/store/…-dep.drv","/nix/store/…-top.drv"],
 "sources":["/nix/store/…-source"],"storeDrvs":0}
```

| Key | Value |
|:---|:---|
| `drvs` | the derivations of the closure, in dependency order, references first, as the upload needs them |
| `sources` | the input source paths of the closure |
| `storeDrvs` | how many of the derivations were read from `/nix/store` rather than from memory or from a `--drvs` directory |

Exit: 0; 1 when a derivation cannot be read; 2 on a usage error.

## `missing`

A plan and a builder become what will be built and what will be fetched.

```
ietsp plumbing missing [--builder URL] [--check-cache-status]
                       [--drvs DIR] [--roots -] DRV...
```

| Flag | Effect |
|:---|:---|
| `--builder URL` | where the builds would go; the `builder` setting by default |
| `--check-cache-status` | add a `roots` array |

```json
{"downloadSize":1553696,"narSize":6180152,"unknown":[],
 "willBuild":["/nix/store/…-dep.drv","/nix/store/…-top.drv"],
 "willSubstitute":["/nix/store/…-figlet-2.2.5"]}
```

| Key | Value |
|:---|:---|
| `willBuild` | the derivations to build, references first |
| `willSubstitute` | the paths to fetch from a substituter, sorted by store-path name |
| `unknown` | the paths nothing can build or fetch |
| `downloadSize` | bytes to download for `willSubstitute` |
| `narSize` | bytes of those paths unpacked |

The names are the fields of Nix's `MissingPaths`. This object is byte for
byte what `ietsp build --dry-run --json` prints.

`--check-cache-status` adds one object per root, from the
cache-status-resolver.cc algorithm — the same keys `ietsp eval-jobs
--check-cache-status` reports:

```json
{"roots":[{"cacheStatus":"notBuilt","drv":"/nix/store/…-top.drv",
  "neededBuilds":["/nix/store/…-dep.drv","/nix/store/…-top.drv"],
  "neededSubstitutes":[]}]}
```

| Key | Value |
|:---|:---|
| `drv` | the root's derivation path |
| `cacheStatus` | `local`, `cached` or `notBuilt` |
| `neededBuilds` | the derivations that would be built, in post-order |
| `neededSubstitutes` | the paths that would be substituted, sorted by name then path |

It reads the roots' derivations from `/nix/store`, as `missing` does.

Exit: 0; 1 when the builder cannot be reached or a derivation cannot be
read; 2 on a usage error. A `grpc://` builder has no `QueryMissing`, so
it is the usage error `--dry-run needs a worker-protocol builder`.

## `build`

Derivation roots are built; the outcomes come back.

```
ietsp plumbing build [--builder URL] [--out NAME]... [--fetch-to DIR]
                     [--source STOREPATH=LOCAL]... [--gc-root DIR]
                     [--no-build] [--drvs DIR] [--roots -] DRV...
```

| Flag | Effect |
|:---|:---|
| `--builder URL` | where the builds go; the `builder` setting by default |
| `--out NAME` | an output to build besides the roots' own; repeatable |
| `--source STOREPATH=LOCAL` | upload `STOREPATH` from the local tree `LOCAL` |
| `--fetch-to DIR` | restore every output into `DIR` |
| `--gc-root DIR` | link each output at `DIR/<output base name>` on the **local** daemon whatever the builder is, which brings the closure back first, and report the links as `gcRoots` |
| `--no-build` | register the closure and stop; the object is `{"outcomes":[]}` |

```json
{"outcomes":[{"drv":"/nix/store/…-ok.drv","logTail":[],
  "outputs":{"out":"/nix/store/…-ok"},"reason":null,"success":true}]}
```

| Key | Value |
|:---|:---|
| `outcomes` | one object per root |
| `outcomes[].drv` | the derivation path |
| `outcomes[].success` | true when the build produced its outputs |
| `outcomes[].outputs` | output name to store path; `{}` on a failure |
| `outcomes[].reason` | `status N: MESSAGE` on a failure, else `null` |
| `outcomes[].logTail` | the last lines of the failed build's log; `[]` on success |
| `gcRoots` | with `--gc-root DIR` only: the links that were made |

Build progress goes to stderr through the reporter: one line per state
change and the log block of a failure. Under `--log-format internal-json`
the activity records carry the build instead
(`docs/reference/errors.md`).

Exit: 0; 100, 101, 102 or 104 when a build failed, following
`failingExitStatus`; 1 on any other error; 2 on a usage error. The codes
are in `docs/reference/errors.md`.

## `fetch`

A URL, or the fixed-output derivation an installable names, becomes a
hash. Nothing is added to any store.

```
ietsp plumbing fetch [--unpack] [--hash-type ALGO] URL
ietsp plumbing fetch [EVAL FLAGS] (-E EXPR | FILE | FLAKEREF[#FRAGMENT])
```

| Flag | Effect |
|:---|:---|
| `--unpack` | hash the NAR of the unpacked tarball, as `fetchzip` does; a URL only |
| `--hash-type ALGO` | `md5`, `sha1`, `sha256` (the default) or `sha512` |

```json
{"hash":"sha256-…","url":"https://…"}
{"hash":"sha256-…","drvPath":"/nix/store/….drv"}
```

`drvPath` is the rewritten derivation that was built, not the one the
expression evaluated to. `ietsp prefetch` is this op plus the choice
between the object and the bare hash; how the rewrite works is under
`prefetch` in `docs/reference/commands.md`.

Exit: 0 with a hash; 1 when the download or the build failed; 2 on a
usage error, which includes no argument at all
(`nothing to fetch: give a URL, -E EXPR, FILE or FLAKEREF#ATTR`) and
`--unpack` with an installable
(`--unpack applies to a URL; a derivation says how it is hashed`).

## Composition

`resolve` feeds `plan`, `missing` and `build`. `plan` feeds `missing` and
`build`. Every op that takes roots accepts `--roots -`.

A closure that is in `/nix/store` already:

```console
$ ietsp plumbing resolve nixpkgs#hello | ietsp plumbing missing --roots -
```

A closure that is in no store at all, carried across the process boundary
by the ATerm dump:

```console
$ ietsp plumbing resolve --dump-drvs d ./chain.nix
{"attrPaths":[],"drvDir":"d","roots":[{"attr":null,"drv":"/nix/store/…-top.drv","output":null}]}

$ ietsp plumbing resolve --dump-drvs d ./chain.nix | ietsp plumbing plan --roots -
{"drvs":["/nix/store/…-dep.drv","/nix/store/…-top.drv"],"sources":[],"storeDrvs":0}
```

`storeDrvs` is 0: no derivation was read from `/nix/store`. Without the
dump the same pipe fails, because the root is in no store:

```console
$ ietsp plumbing resolve ./chain.nix | ietsp plumbing plan --roots -
error: cannot read derivation '/nix/store/…-top.drv': No such file or directory
```

Resolve, build and root the outputs in one pipeline:

```console
$ ietsp plumbing resolve --dump-drvs d ./ok.nix \
    | ietsp plumbing build --gc-root gcr --roots -
{"gcRoots":["gcr/…-ok"],"outcomes":[{"drv":"/nix/store/…-ok.drv","logTail":[],
 "outputs":{"out":"/nix/store/…-ok"},"reason":null,"success":true}]}
```

Read a plan back with `--drvs`, without a pipe:

```console
$ ietsp plumbing plan --drvs d /nix/store/…-ok.drv
{"drvs":["/nix/store/…-ok.drv"],"sources":[],"storeDrvs":0}
```

## What each command is written against

The operation layer is `crate::api`. A row naming an op that has no
command — `index`, `jobs`, `dev-env`, `drv-show`, `drv-diff`,
`closure-diff`, `flake-info`, `flake-tree` — means the command calls the
**capability** (`api::installable`, `api::flake`, `crate::jobs`) with no
op in between.

| Command | The operations it uses | What is the command's own |
|:---|:---|:---|
| `ietsp eval` | `eval` | the human value printer, `--timings` |
| `ietsp build` | `plan`, `missing`, `build`, through `api::build::Dispatch` | the `--dry-run` text, `render_size`, the `fix-hashes` prompts, the `.drv` legacy input |
| `ietsp search` | `index` | regex compile and match, `--exclude`, colour, `--pretty`, the no-results hint |
| `ietsp diff` | `resolve`, `drv-diff`, `closure-diff` | the text rendering, the `diff(1)` exit codes |
| `ietsp eval-jobs` | `jobs` | the worker/collector process model, `--max-memory-size`, the summary line |
| `ietsp fast-build` | `jobs`, `build` | the terminal report, `--result-file`, the step summary |
| `ietsp print-dev-env` | `resolve`, `dev-env` | the bash rendering, the direnv cache, the GC root |
| `ietsp prefetch` | `fetch` | the `--json` shape choice |
| `ietsp lint`, `ietsp parse`, `ietsp cache` | none | they are already thin over the library |
| `nix-instantiate` | `resolve` (get-drvs walk), `plan` | Nix argv parsing, `--add-root`, out-links, the exec fallback |
| `nix-build` | `resolve`, `plan`, `build` | `result` symlinks, `--add-root`, Nix's exit codes, the fallback |
| `nix-shell` | `resolve`, `plan`, `build` | the env, `execve` of the shell, `--run`, `--pure` |
| `nix-eval-jobs` | `jobs` | argv compatibility |
| `nix-fast-build` | `jobs`, `build` | argv compatibility, the report |
| `nix eval` | `eval` | Nix's printer and argv |
| `nix build` | `resolve`, `plan`, `build` | `result` symlinks, the `--json` Nix shape, the fallback |
| `nix search` | `index` | Nix's output format and argv |
| `nix develop` | `resolve`, `dev-env` | the shell it execs |
| `nix print-dev-env` | `dev-env` | the bash rendering |
| `nix flake metadata` | `flake-info` | Nix's output format |

## What the operation layer does not expose

Five capabilities a command still reaches for itself. Each is a gap, not
a design.

1. **The sources of an ATerm dump.** `resolve --dump-drvs DIR` writes the
   run's derivations, not its sources, so a closure whose sources are not
   in the local store still needs an instantiation first. The shape it
   wants is the batch form of `build --source STOREPATH=LOCAL`.
   `ietsp diff` keeps its in-memory table, because it evaluates and
   compares in one process.
2. **Hash mismatch capture.** `ietsp prefetch` drives a build that is
   meant to fail and reads the mismatch out of the log
   (`log::want_mismatch`, `log::take_mismatches`); `ietsp build`'s
   `fix-hashes` reads the same channel. `build` should report a mismatch
   as structured output, not as a log line the caller greps.
3. **The eval-store split.** `eval-jobs --eval-store` instantiates on one
   store while building on another. `plan` and `build` take one builder.
4. **A second connection for a shell.** `nix-shell` and `print-dev-env`
   upload a plan and then speak the worker protocol themselves — build the
   derivation's *inputs*, `execve` a shell, read the env output back. They
   use `api::build::plan` and `api::build::locate_store` and drive
   `PlanUpload` on that connection, because routing them through
   `Dispatch` would open a second one.
5. **Flake metadata as an op.** `api::flake::info` and `api::flake::tree`
   are the capability; there is no `flake-info` or `flake-tree` op,
   because no caller needs the process boundary.

## The source resolver

`crate::source` is what turns a reference into a tree; the fetchers and
the input store are behind it, and `crate::flake` is `pub(crate)`.

```rust
/// Where a tree comes from: a directory, a git working tree, or a locked input.
pub enum SourceRef { … }

/// A tree that is there: a readable path, and the input attributes it
/// resolved to (`nar_hash`, `last_modified`, `rev` are accessors over them).
pub struct Tree { pub path: PathBuf, … }

/// What a flake locked, as data. No evaluator, no fetching.
pub struct Lock { pub root: NodeId, pub nodes: BTreeMap<NodeId, LockedInput> }

pub fn tree(ev: &mut Evaluator, r: &SourceRef) -> Result<Tree>;
```

`source::tree` is the one call `builtins.fetchTree`, the flake root and
`nix flake prefetch-inputs` share. Three neighbouring jobs are **not** on
it: parsing a flake reference is flake syntax and stays in
`crate::flake`; reading one `flake.lock` is `api::flake::info`; and
reading bytes out of a tree goes through `treestore::StoredTree`.

`api::flake` is what a command calls: `is_flake_ref`, `parse_attr_path`,
`select`, `outputs`, `info` and `tree`.

## Tests

`scripts/plumbing-e2e.nu` has one case per op, checked against the JSON
shapes on this page. The shape is the contract; a change to it is a
change to this page.

`tests/layering.rs` checks that no file under `src/cli` reaches past
`crate::api` into a layer below it, outside an explicit allow-list
(`docs/explanation/plumbing-and-porcelain.md`).

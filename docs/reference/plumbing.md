# Plumbing operations

`iets plumbing` gives you the steps of `iets build` one at a time, as JSON,
so you can script them. This page lists every operation: its flags, the
exact JSON it writes, and its exit codes.

```console
$ iets plumbing resolve nixpkgs#hello | iets plumbing missing --roots -
```

```
iets plumbing OP [FLAGS] ...
```

Each op reads flags, and maybe JSON on stdin, and writes one JSON object
on stdout. Nothing else goes to stdout. Errors go to stderr and set the
exit code (`docs/reference/errors.md`). To learn why these operations
exist, read `docs/explanation/plumbing-and-porcelain.md`.

## The operations

| Op | Contract |
|:---|:---|
| `resolve` | an installable becomes derivation roots |
| `plan` | derivation roots become an upload plan |
| `missing` | a plan and a builder become what will be built and fetched |
| `build` | derivation roots are built; outcomes come back |
| `fetch` | a URL or a fixed-output derivation becomes a hash |

`iets plumbing --help` lists these five. An unknown op is the usage error
`unknown subcommand 'X'`, with a suggestion when the name is close to one
of the five.

The JSON shapes on this page are the contract. An end-to-end test checks
each one, so a change to a shape is a change to this page.

## Shared input

The ops that evaluate (`resolve`, and `fetch` with an expression) take the
same evaluation flags as the other commands (`docs/reference/commands.md`):
`-E EXPR`, `FILE` (or `-f FILE`, the same thing), `FLAKEREF[#FRAGMENT]`,
`-A PATH`, `--arg`, `--argstr`, `-I`, `--impure`/`--pure-eval`,
`--option`, `--workers`, `--no-tracing-cache`, `--no-ir-cache`,
`--no-cache`, `--cache-dir`, `--log-format`, `--show-trace`,
`--dump-drvs`.

`-A PATH` and a flake fragment walk the value as Nix does
(`attr-path.cc:findAlongAttrPath`). Each step calls the function it stands
on, so a file whose top level is a function (nixpkgs' `default.nix`) is
called with the `--arg`/`--argstr` set before an attribute is taken from
it. A formal with a default may be left out. A required formal that is
left out is an error. A function whose formals end in `...` gets every
argument, named or not (`eval.cc:autoCallFunction`). The classic names,
`search` and every native command walk the same way.

## Naming roots

`plan`, `missing` and `build` take derivation paths. Write each one as
`DRV` or `DRV!OUT[,OUT]`. With no output named, you get every output, as
`!*` does in the worker protocol. `--roots -` reads the stdout of an
earlier op instead: the `roots` of a `resolve`, or the `drvs` of a `plan`.
Stdin with neither key is the usage error
`--roots -: no "roots" and no "drvs" on stdin`.

By default, these three read the closure from `/nix/store`, so each root
must be there. `resolve` does not put it there: it keeps the derivations
in memory, which is how `iets build` can ship a closure that was never
written to disk. A root that is in no store is the error
`cannot read derivation 'PATH': No such file or directory`, exit 1.

To carry that closure to another process, use `resolve --dump-drvs DIR`.
`plan`, `missing` and `build` then read the derivations from `DIR` when
`--drvs DIR` names it, or when `--roots -` reads a `resolve` whose output
has `drvDir`.

The dump holds derivations only. An input **source** that evaluation made
and never wrote to the store is still read from the local store. For a
closure with such sources, run `iets build --no-build` or
`iets plumbing build --no-build` first.

## `resolve`

An installable becomes derivation roots.

```
iets plumbing resolve [EVAL FLAGS]
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
other two `getActualAttrPaths` tables of Nix. `none` uses the fragment as
the attribute path itself.

`--walk` picks one of the three walks Nix has:

| `--walk` | Nix | Used by |
|:---|:---|:---|
| `context` (default) | `drvPath`, or the derivations in a string's context | `iets build`, `diff`, `prefetch` |
| `get-drvs` | get-drvs.cc:getDerivations, recursing with `recurseForDerivations` | `nix-build`, `nix-shell`, `nix-instantiate` |
| `to-install` | get-drvs.cc:queryOutputs: `outputSpecified`, else `meta.outputsToInstall`, else every output (`out` for a flake) | `nix build` |

`get-drvs` walks an attribute set in lexicographic name order, takes a
derivation whole, and goes deeper only where `recurseForDerivations = true`
is set. It always walks a list.

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
iets plumbing plan [--drvs DIR] [--roots -] DRV...
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
iets plumbing missing [--builder URL] [--check-cache-status]
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
byte what `iets build --dry-run --json` prints.

`--check-cache-status` adds one object per root. It follows Nix's
cache-status-resolver.cc, and has the same keys that
`iets eval-jobs --check-cache-status` reports:

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

Like `missing`, it reads the roots' derivations from `/nix/store`.

Exit: 0; 1 when the builder cannot be reached or a derivation cannot be
read; 2 on a usage error. A `grpc://` builder has no `QueryMissing`, so
it is the usage error `--dry-run needs a worker-protocol builder`.

## `build`

Derivation roots are built, and the outcomes come back.

```
iets plumbing build [--builder URL] [--out NAME]... [--fetch-to DIR]
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
{"outcomes":[{"attr":"ok","drv":"/nix/store/…-ok.drv","logTail":[],
  "outputs":{"out":"/nix/store/…-ok"},"reason":null,"success":true}]}
```

| Key | Value |
|:---|:---|
| `outcomes` | one object per root |
| `outcomes[].attr` | what the root is called: the derivation name here, the attribute path in `iets build --json` |
| `outcomes[].drv` | the derivation path |
| `outcomes[].success` | true when the build produced its outputs |
| `outcomes[].outputs` | output name to store path; `{}` on a failure |
| `outcomes[].reason` | `status N: MESSAGE` on a failure, else `null` |
| `outcomes[].logTail` | the last lines of the failed build's log; `[]` on success |
| `gcRoots` | with `--gc-root DIR` only: the links that were made |

Build progress goes to stderr: one line per state change, and the log of a
failed build. Under `--log-format internal-json`, activity records carry
the build instead (`docs/reference/errors.md`).

A hash mismatch shows up only in the build log, not as a field of the
JSON.

Exit: 0; 100, 101, 102 or 104 when a build failed, as Nix's
`failingExitStatus` gives them; 1 on any other error; 2 on a usage error.
`docs/reference/errors.md` lists the codes.

## `fetch`

A URL, or the fixed-output derivation an installable names, becomes a
hash. Nothing is added to any store.

```
iets plumbing fetch [--unpack] [--hash-type ALGO] URL
iets plumbing fetch [EVAL FLAGS] (-E EXPR | FILE | FLAKEREF[#FRAGMENT])
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
expression evaluated to. `iets prefetch` is this op, plus the choice
between the object and the bare hash. `prefetch` in
`docs/reference/commands.md` says how the rewrite works.

Exit: 0 with a hash; 1 when the download or the build failed; 2 on a
usage error. These are usage errors: no argument at all
(`nothing to fetch: give a URL, -E EXPR, FILE or FLAKEREF#ATTR`), and
`--unpack` with an installable
(`--unpack applies to a URL; a derivation says how it is hashed`).

## Composition

`resolve` feeds `plan`, `missing` and `build`. `plan` feeds `missing` and
`build`. Every op that takes roots accepts `--roots -`.

A closure that is in `/nix/store` already:

```console
$ iets plumbing resolve nixpkgs#hello | iets plumbing missing --roots -
```

A closure that is in no store at all, carried to the next process by the
ATerm dump:

```console
$ iets plumbing resolve --dump-drvs d ./chain.nix
{"attrPaths":[],"drvDir":"d","roots":[{"attr":null,"drv":"/nix/store/…-top.drv","output":null}]}

$ iets plumbing resolve --dump-drvs d ./chain.nix | iets plumbing plan --roots -
{"drvs":["/nix/store/…-dep.drv","/nix/store/…-top.drv"],"sources":[],"storeDrvs":0}
```

`storeDrvs` is 0: no derivation was read from `/nix/store`. Without the
dump, the same pipe fails, because the root is in no store:

```console
$ iets plumbing resolve ./chain.nix | iets plumbing plan --roots -
error: cannot read derivation '/nix/store/…-top.drv': No such file or directory
```

Resolve, build and root the outputs in one pipeline:

```console
$ iets plumbing resolve --dump-drvs d ./ok.nix \
    | iets plumbing build --gc-root gcr --roots -
{"gcRoots":["gcr/…-ok"],"outcomes":[{"drv":"/nix/store/…-ok.drv","logTail":[],
 "outputs":{"out":"/nix/store/…-ok"},"reason":null,"success":true}]}
```

Read a plan back with `--drvs`, without a pipe:

```console
$ iets plumbing plan --drvs d /nix/store/…-ok.drv
{"drvs":["/nix/store/…-ok.drv"],"sources":[],"storeDrvs":0}
```

## Limits

- `resolve --dump-drvs` writes derivations, not sources (see
  [Naming roots](#naming-roots)).
- `plan` and `build` take one builder. There is no way to instantiate on
  one store and build on another, as `iets eval-jobs --eval-store` does.
- There is no op for flake metadata.

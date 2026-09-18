# Classic mode

The same `ietsp` binary installed under Nix's own command names. Each name
handles the subset ietsp implements and execs the real binary for the rest,
so it is a drop-in only where the real nix is also on `PATH`.

```
nix build .#classic     # bin/{nix-instantiate,nix-build,nix-eval-jobs,nix}
```

Every symlink points at `bin/ietsp`; the basename of `argv[0]` picks the
command. Without symlinks the same commands work as subcommands:
`ietsp nix-instantiate ...`, `ietsp nix-build ...`.

## The NixOS module

`nix/module.nix` installs `packages.default` and the direnv hook.
`ietsp.classic.enable` (default `false`) adds the farm as well:

```nix
{
  imports = [ ietsp.nixosModules.default ];
  ietsp.classic.enable = true;
}
```

The farm goes into `environment.systemPackages` under `lib.hiPrio`.
`system.path` is a `buildEnv`, which resolves a collision between two
`bin/nix-instantiate` by `meta.priority` — lower wins, `hiPrio` is -10
against nix's default 5 — so the farm's symlinks are the ones in the
system path. No `PATH` juggling in `environment.extraInit`, which shells
that skip it would ignore.

The loser of a `buildEnv` collision is not in the system path at all, so
the fallback would find no second `nix` on `PATH`. The option therefore also
sets `IETSP_NIX_BIN_DIR` to `${config.nix.package}/bin`, which is where the
fallback looks first. Outside a login shell (`environment.variables` is
`/etc/set-environment`), set it yourself or put the real nix's `bin` on
`PATH`.

To check a configuration, build its `system.path` and look at a symlink:

```
readlink $(nix build --no-link --print-out-paths \
  --impure --expr '(...nixosSystem {...}).config.system.path')/bin/nix-instantiate
```

## What each name handles

### `nix-instantiate`

`[FILE...]` (default `./default.nix`, a directory resolves to its
`default.nix`, `<name>` goes through the search path), `-E`/`--expr`,
`-A`/`--attr` (repeatable, `"quoted"` components and list indices),
`--arg`/`--argstr`, `-I`/`--include`, `--eval` with `--strict`, `--json`,
`--raw`, `--add-root PATH` (`--indirect` accepted), `--readonly-mode`,
`--read-write-mode`, `--dry-run`, `--show-trace`, `--no-gc-warning`,
`--option`, `--store`, `-j`/`--max-jobs`, `--cores`, `-k`, `-K`, `-Q`,
`--quiet`, `-v`.

Default mode evaluates, walks the value like `getDerivations`, computes the
drv closure, registers it through the daemon (`QueryValidPaths` then one
`AddMultipleToStore`, as `ietsp build --no-build` does) and prints one root
per line: the drv path, plus `!<output>` when the derivation's `outputName`
is not `out`. `--add-root` creates the symlink, `AddIndirectRoot`s it, and
prints the link instead of the drv path, numbering the second and later
roots `-2`, `-3`, … `--readonly-mode` (and `--eval` without
`--read-write-mode`) registers nothing.

Falls back on: `--parse`, `--parse-only`, `--xml`, `--find-file`, `-`
(stdin), `--add-root` together with a read-only store, any other flag.

### `nix-build`

`[FILE...]` (default `.`), `-E`, `-A`, `--arg`/`--argstr`, `-I`,
`-o`/`--out-link`, `--no-out-link`/`--no-link`, `-j`/`--max-jobs`,
`--cores`, `-k`, `-K`, `-Q`/`--no-build-output`, `--quiet`, `-v`,
`--show-trace`, `--option`, `--store`, `--add-drv-link` (obsolete, ignored).

Uploads the closure once, builds every root in one
`BuildPathsWithResults`, then links: `result` for the first drv's `out`,
`result-<output>` for another output of the same drv, `result-<n>` for the
n-th distinct drv from 2 on (`nix-build.cc`), each link registered as an
indirect GC root. Prints the output paths in root order. stderr carries
`building '<drv>'...` per build and the daemon's log lines.

Exit codes: 0, or 1 for an evaluation error, a daemon error or a build
failure — what the real `nix-build` returns here, where the daemon
delegates builds to a remote builder.

Falls back on: `--dry-run`, `--drv-link`, `--run-env`, `--command`,
`--run`, `--repair`, `--check`, `--pure`, `--exclude`, `--keep`,
`-p`/`--packages`, `-`, any other flag.

### `nix-eval-jobs`

The existing `ietsp eval-jobs`, which already matches nix-eval-jobs 2.35
flag for flag. See `docs/eval-jobs.md`.

### `nix`

Three subcommands; every other one execs the real `nix`.

`nix eval [INSTALLABLE]`: `--json`, `--raw`, `--expr E`, `-f`/`--file FILE`,
`--impure` (accepted; ietsp is impure already), `-I`, `--arg`/`--argstr`,
`--option`, `--store`, `--show-trace`, `--experimental-features` and
`--extra-experimental-features` (accepted and ignored),
`--accept-flake-config` (ignored).

The output is `nix eval`'s, which is not `nix-instantiate --eval`'s
(`print.cc:Printer` with `force`, `derivationPaths` and `trackRepeated`):
every value is forced, a derivation prints as
`«derivation /nix/store/….drv»`, a function as `«lambda f @ FILE:L:C»`,
`«primop add»` or `«partially applied primop add»` (`eval.cc:addPrimOp`
drops a leading `__` from the name), a value seen twice as `«repeated»`,
and a value that fails to evaluate as `«error: …»` — only an error at the
top level is reported and exits 1. `--json` is `printValueAsJSON` with
`strict`, so a derivation becomes its output path. `--raw` coerces without
quoting.

An INSTALLABLE is `FLAKEREF[#ATTR]` on a locked local flake (`.`, `path:`,
a directory inside a git repository — see `docs/flakes.md`), or the
attribute path of the `-f`/`--expr` value. `.` is the whole value. The
flake attr-path candidates are `installable-flake.cc:getActualAttrPaths`
with `installables.cc`'s defaults:

| fragment | candidates, in order |
|---|---|
| none | `packages.<system>.default`, `defaultPackage.<system>` |
| `.foo` | `foo` |
| `foo` | `packages.<system>.foo`, `legacyPackages.<system>.foo`, `foo` |

`nix build [INSTALLABLE...]` (default `.`): `-o`/`--out-link PATH`,
`--no-link`, `--print-out-paths`, `-L`/`--print-build-logs`, `-f`,
`--expr`, plus the same global flags. It evaluates like `nix eval`, then
builds through the daemon the way `nix-build` does. The outputs it installs
are `get-drvs.cc:queryOutputs`: `outputSpecified` picks one, else
`meta.outputsToInstall`, else every output of an `-f`/`--expr` installable
and `out` of a flake one. The links are `build.cc:createOutLinks`:
`result`, `result-<n>` for the n-th installable from 1 on, and
`-<output>` appended for an output other than `out`. stdout is empty
without `--print-out-paths`. Exit 1 on an evaluation, daemon or build
error.

`nix print-dev-env [FLAKEREF[#attr]] [--json]` is `ietsp print-dev-env`
(`docs/direnv.md`).

Falls back on: `nix eval --apply`, `--write-to` or more than one
installable, `nix build --rebuild`, `--dry-run` or `--json`, an installable
that is a store path (`/nix/store/…`, or an `^outputs` suffix), a
non-local flake reference such as `nixpkgs#hello`, an installable that is
not a derivation under `nix build`, `--arg`/`--argstr` with a flake (nix
refuses that), `nix develop`, `nix flake …`, `nix run`, `nix shell`,
`nix profile`, `nix store`, `nix copy`, `nix repl`, `nix log`,
`nix why-depends`, `nix fmt`, every other subcommand, and any flag not
listed above.

## Fallback rules

Two triggers:

1. A flag or subcommand the parser does not implement, seen before any
   work starts.
2. An evaluation that hits `ErrorKind::Unsupported`: the network fetchers
   (`fetchurl`, `fetchTarball`, `fetchGit`, `fetchMercurial`,
   `fetchClosure`), the flake primops (`getFlake`, `parseFlakeRef`,
   `flakeRefToString`), `outputOf`, import-from-derivation, and a flake
   whose `flake.lock` is missing or incomplete. Every other evaluation
   error is reported and exits 1, because the real nix would fail the same
   way and re-running it would double the cost.

The fallback prints one line on stderr and then `execve`s the real binary
with the same `argv`:

```
ietsp: falling back to nix-instantiate: '--parse' is not implemented
```

| variable | effect |
|---|---|
| `IETSP_NIX_BIN_DIR` | the directory the real binary is taken from |
| `IETSP_NO_FALLBACK=1` | print the reason and exit 1 instead of exec'ing |

Without `IETSP_NIX_BIN_DIR`, the first `<name>` on `PATH` whose canonical
path is not `/proc/self/exe` wins, so the farm can sit ahead of the real
nix. Nothing found at all:
`ietsp: cannot fall back to <name>: not found on PATH`, exit 1.

## Config

`src/config.rs` reads the same files Nix reads (`docs/config.md`). Order,
later wins: `$NIX_CONF_DIR/nix.conf`, the user files, `$NIX_CONFIG`, then
the command line, where `--option name value` and the flag aliases
(`--store`, `-j`/`--max-jobs`, `--cores`, `-k`, `-K`) are overrides in
command-line order.

What is used:

| setting | effect |
|---|---|
| `store` | the worker transport: `auto`, empty and `daemon` are the daemon socket; `unix://PATH` and `ssh-ng://HOST` are honoured; anything else falls back |
| `system` | the evaluator's `builtins.currentSystem` |
| `max-jobs`, `cores`, `keep-going`, `keep-failed`, `substitute` | `SetOptions` on the connection |

## Known divergences

`docs/divergences.md` covers the evaluator. Classic mode adds:

- `store = auto` is the daemon socket. The real nix would use the local
  store directly when it is writable.
- `-Q`/`--no-build-output` drops the daemon's log lines. In Nix 2.34 it
  only switches the log format to `raw`.
- `--show-trace` is accepted and ignored: ietsp always prints the full
  trace it has.
- `--no-out-link` creates no symlink at all. `nix-build` links into a
  temporary directory and lets it be deleted, which roots the output for
  the length of the run.
- `getDerivations` recursion covers derivations, attrsets (lexicographic
  order, `recurseForDerivations`) and lists. `_combineChannels` and
  `ignoreAssertionFailures` are not implemented.
- `--add-root` does not refuse a link that already exists and points
  outside the store; it replaces it.
- The tracing cache (`docs/tracing-cache.md`) is off in classic mode.
- `nix eval` and `nix build` are always impure. The real `nix eval --expr`
  keeps `pure-eval` on, so it refuses to copy a path to the store, and
  `nix eval --raw ./.` fails where ours prints the store path.
- `nix eval` does not register the derivations it evaluates. The real one
  writes the `.drv` files while computing `drvPath`.
- `nix build` prints the log lines only with `-L`. The real one shows the
  last line in the progress bar and the whole log when a build fails.
- Two installables that name the same derivation print one output path
  each. The real `nix build` prints one per pair, so twice as many.
- An evaluation error inside a value `nix eval` prints becomes
  `«error: …»`, except `ErrorKind::Unsupported`, which must escape so the
  run can fall back to the real nix.

## Tests

`scripts/classic-e2e.nu` runs the real `nix-instantiate`/`nix-build`/`nix`
and ours for every case and compares stdout byte for byte, the exit code and
the `result*` symlink targets. The `nix` cases use
`tests/fixtures/flake`, a flake with no inputs and a complete `flake.lock`,
copied out of the repository so both sides see a `path:` flake. It writes to the store and needs a live
`nix-daemon`, so it is not part of `nix flake check`; run it from the dev
shell after `cargo build --release`.

# Classic mode

Every classic Nix name ietsp answers to, the flags each one handles, and
what makes it fall back to the real `nix`.

The same `ietsp` binary installed under Nix's own command names. Each name
handles the subset ietsp implements and execs the real binary for the rest,
so it is a drop-in only where the real nix is also on `PATH`.

```
nix build .#classic     # bin/{nix-instantiate,nix-build,nix-eval-jobs,nix-shell,nix-fast-build,nix}
```

Every symlink points at `bin/ietsp`; the basename of `argv[0]` picks the
command. Without symlinks the same commands work as subcommands:
`ietsp nix-instantiate ...`, `ietsp nix-build ...`.

## Where the builds go

None of the classic names has a `--builder` flag, so the build target is the
`builder` setting in `ietsp.conf`, and nix.conf's `store` when that is unset.
`--store URL` and `--option store URL` on the command line win over both.
`--ietsp-option name value` sets any ietsp setting for one run, and is stripped
from the argv before a fallback execs the real nix (`docs/reference/settings.md`).

Registration stays local whatever `builder` says:

- `nix-instantiate` registers the drvs and their sources on the local daemon,
  and `--add-root` links them there. It builds nothing, so the builder is
  irrelevant to it.
- `nix-build`, `nix build` and `nix fmt` build on the builder. When that is not the local
  daemon, the outputs' closure is copied back to the local store
  (`QueryPathInfo` and `NarFromPath` there, one `AddMultipleToStore` here)
  before `result` is linked and `AddIndirectRoot`ed, so the link names a path
  this machine has. The same thing the `grpc://` path has always done.
- `nix-shell`, `nix develop` and `nix print-dev-env` skip the `builder`
  setting: the shell runs here, so its inputs belong in the local store, and
  the local daemon builds or substitutes them. `--store` still wins.

## The NixOS module

`nix/module.nix` installs `packages.default` and the direnv hook.
`ietsp.classic.enable` (default `false`) adds the farm as well, and
`ietsp.settings` writes `/etc/ietsp/ietsp.conf` (`docs/reference/settings.md`):

```nix
{
  imports = [ ietsp.nixosModules.default ];
  ietsp.classic.enable = true;
  ietsp.settings.builder = "ssh-ng://bld1";
}
```

The farm goes into `environment.systemPackages` under `lib.hiPrio`.
`system.path` is a `buildEnv`, which resolves a collision between two
`bin/nix-instantiate` by `meta.priority` — lower wins, `hiPrio` is -10
against nix's default 5 — so the farm's symlinks are the ones in the
system path. No `PATH` juggling in `environment.extraInit`, which shells
that skip it would ignore.

The loser of a `buildEnv` collision is not in the system path at all, so
the fallback cannot find a second `nix` on `PATH`. The farm therefore
records the nix it was built for in `share/ietsp/real-nix-bin`, and the
fallback follows the symlink chain from the `PATH` entry to that file. It
works in every process, login shell or not. The module builds the farm for
`config.nix.package`; `IETSP_NIX_BIN_DIR` stays as an override.

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

### `nix-shell`

`[FILE]` (a directory, or nothing, resolves to `shell.nix` and then
`default.nix`; `<name>` goes through the search path), `-E`/`--expr`, `-A`,
`--arg`/`--argstr`, `-I`, `--run CMD`, `--command CMD`, `--pure`, `--impure`,
`--keep NAME`, `--option`, `--store`, `-j`/`--max-jobs`, `--cores`, `-k`, `-K`,
`-Q`/`--no-build-output`, `--quiet`, `-v`, `--show-trace`, `--add-drv-link`,
`--indirect`, `--no-gc-warning` (the last three obsolete, ignored).

This is `nix-build.cc`'s `runEnv` path, not `print-dev-env`: nothing builds the
`-env` derivation. It

1. evaluates to exactly one derivation (`nix-shell requires a single
   derivation`), with `inNixShell = true` passed to a function that has that
   formal;
2. realises the derivation's **inputs** — every `inputDrvs` output and every
   `inputSrcs` path — through the daemon, never the derivation itself;
3. builds the environment: the current environment (filtered to nix's
   `keepVars` with `--pure`), then `NIX_BUILD_TOP` and the `TMP*` family, the
   per-run temporary directory, `NIX_STORE`, `NIX_BUILD_CORES`, then every
   `drv.env` pair, with `passAsFile` names written to `.attr-<n>` files and
   exposed as `<name>Path`;
4. writes the rc file `nix-build.cc` writes — the tmpdir cleanup trap,
   `~/.bashrc` when impure, `unset PATH`, the structured attributes as
   `declare` lines, `source $stdenv/setup`, `PATH`/`SHELL`/`BASH`, `PS1`,
   `runHook shellHook`, `shopt -s execfail`, then the `--run`/`--command`
   command with `exit` appended;
5. execs `bash --rcfile <rc>` when the shell will be interactive (stdin and
   stderr are both a tty, and `--run` was not used), else `bash <rc>`.

bash is `$NIX_BUILD_SHELL`, else `(import <nixpkgs> {}).bashInteractive` built
through the daemon, else — when that expression does not evaluate — `bash` from
the environment, with nix's `uses bash from your environment` notice.

Falls back on: `-p`/`--packages`, a `#!` script (the first argument is a file
starting with `#!`, which is `-i INTERPRETER` mode), `--exclude`, `--dry-run`,
`--check`, `--repair`, `-o`/`--out-link`, `--no-out-link`/`--no-link`, `-`
(stdin), a derivation that nix would have to resolve first (any output that is
not input-addressed), `exportReferencesGraph` in a structured-attributes
derivation, any other flag.

### `nix-eval-jobs`

The existing `ietsp eval-jobs`, which already matches nix-eval-jobs 2.35
flag for flag. See `docs/reference/commands.md`.

### `nix-fast-build`

The existing `ietsp fast-build`, with nix-fast-build's flags and result
files. See `docs/reference/commands.md`.

### `nix`

Seven subcommands; every other one execs the real `nix`.

`nix eval [INSTALLABLE]`: `--json`, `--raw`, `--expr E`, `-f`/`--file FILE`,
`--impure`, `--pure-eval`, `--no-pure-eval` (`docs/explanation/evaluation.md`: `nix`
evaluates purely unless told otherwise, `-f` is impure), `-I`, `--arg`/`--argstr`,
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
a directory inside a git repository — see `docs/explanation/flakes-and-sources.md`), or the
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

`--store grpc://host:port[?…]` builds on a nix-grpc-store service instead,
natively: no plugin, no `nix` on the other side. `--eval-store auto` (or
`daemon`, or empty) is accepted and ignored, so the invocation the `nix-farm`
wrapper makes — `nix build --store 'grpc://farm.numtide.com:50051?token-file=…'
--eval-store auto .#pkg` — works as it stands; any other `--eval-store` value
falls back, since ietsp always evaluates in its own process. A build farm
publishes its outputs to a binary cache instead of a local store, so an
out-link needs them copied in first: the local daemon substitutes them from
its caches, and ietsp fetches the rest with `FetchNars` and registers it with
their signatures over the worker protocol before it makes the symlink. With
`--store` given, `--no-link` skips all of that. Without it the `builder`
setting stands in for the local store, as a build hook does, so the outputs
come back even under `--no-link`: `nix build --no-link` followed by
`nix copy` works. See `docs/how-to/build-remotely.md`.

`nix search INSTALLABLE REGEX...`: `-e`/`--exclude REGEX`, `--json`,
`--pretty`, `--no-pretty`, `-f`/`--file`, `--expr`, plus the same global
flags. The output is `search.cc`'s, byte for byte, escape sequences
included; the index of the walk is cached, so a second search over the
same package set answers without evaluating (`docs/reference/commands.md`). An
installable ietsp cannot evaluate — a flake registry reference such as
`nixpkgs`, an unlocked flake — falls back, as does any other flag.

`nix print-dev-env [FLAKEREF[#attr]] [--json]` is `ietsp print-dev-env`
(`docs/reference/commands.md`).

`nix develop [INSTALLABLE]`: `-c`/`--command CMD ARGS...` (like nix, it takes
every remaining argument), `-i`/`--ignore-env`/`--ignore-environment`,
`-k`/`--keep`/`--keep-env-var NAME`, `-u`/`--unset`/`--unset-env-var NAME`, plus
the same global flags. The environment is the one `nix print-dev-env` prints,
computed by the same code (`docs/reference/commands.md`), so the shell derivation's `-env`
derivation is built through the daemon. On top of `makeRcScript` it appends what
`CmdDevelop::run` appends: the rc file deletes itself, `--command` becomes
`exec`, an interactive shell is wrapped in the `~/.bashrc` and
`shopt -u expand_aliases` pair, the `bash-prompt`, `bash-prompt-prefix` and
`bash-prompt-suffix` settings become `PS1` lines, and `SHELL` plus the `PATH`
prefix of the bash directory close the script. `NIX_GCROOT` holds the `-env`
output so the shell keeps it alive. `--ignore-env` clears the environment except
the `--keep` names; `--unset` removes names; both are
`command.cc:MixEnvironment::setEnviron`, so `NIX_GCROOT` and `SHELL` are set
afterwards and always survive.

The attribute paths are `develop.cc`'s: `devShells.<system>.default`,
`devShell.<system>`, `packages.<system>.default`, `defaultPackage.<system>` with
no fragment; `devShells.<system>.<f>`, `packages.<system>.<f>`,
`legacyPackages.<system>.<f>`, `<f>` for a fragment.

Falls back on: `--phase`, `--unpack`, `--configure`, `--build`, `--check`,
`--install`, `--installcheck`, `--profile`, `--redirect`, more than one
installable, any other flag.

`nix fmt [ARG...]` (nix's alias for `nix formatter run`) is `formatter.cc`.
It evaluates `formatter.<system>` of the flake in the working directory —
the installable is always `.`, never an argument — builds it and execs it.
`app.cc:toApp` turns that derivation into a program: `<output>/bin/<main>`,
where `main` is `meta.mainProgram`, else `pname`, else `name` without its
version (`names.cc:DrvName`). Every positional argument is forwarded to the
formatter. The first `--` is dropped and everything after it is an argument,
flags included, which is how a flag reaches the formatter. `PRJ_ROOT` is set
to the flake directory (prj-spec), so a formatter run from a subdirectory
still sees the whole tree. A flake with no `formatter` output fails with
nix's own message:

```
error: flake 'path:/home/you/proj' does not provide attribute 'formatter.x86_64-linux'
```

The formatter's closure comes to the local store, because the formatter runs
here whatever `builder` built it, and the output is linked in ietsp's roots
store at `~/.cache/ietsp/roots/<key>`, keyed on the flake directory and
`formatter.<system>`. That link is registered as an indirect GC root on the
local daemon, so the formatter is not swept and rebuilt between runs. The
idea is Mic92's flake-fmt. A root is a claim on the store and not part of
the answer, so a daemon that will not take one is a warning and the
formatter still runs. `ietsp cache gc` drops the roots nothing has used for
its TTL (`docs/explanation/caching.md`).

The run also goes through the tracing cache, so a second `nix fmt` with
nothing changed neither evaluates nor builds; a formatter that rewrote a
file to the same bytes is still a hit, because the read set holds content
hashes.

There is no native `ietsp fmt`. The classic name is the one users reach
for, and a second spelling would add nothing.

Falls back on: `--json`, `nix formatter build`, `nix formatter run`, a
working directory with no `flake.nix`, and any other flag.

`nix flake prefetch-inputs [FLAKEREF]` (default `.`), plus the global flags,
is `flake-prefetch-inputs.cc`: it fetches every locked input that the root
of `flake.lock` reaches, on up to 25 threads (the default of
`http-connections`). Each input takes the path an evaluation takes
(`docs/explanation/flakes-and-sources.md`): `/nix/store`, then the input store, then a substitute,
then ietsp's own download for `github`, `gitlab`, `sourcehut` and `tarball`,
then the real nix for `git` and `hg`. So a tarball-shaped input lands in the
input store, not in `/nix/store`. A `path:` input is read in place and has
nothing to fetch. An input that fails prints its error, the others go on, and
the exit code is 1. Falls back on: a missing or incomplete `flake.lock`
(the real one locks the flake first), an input of another type, a non-local
flake reference, any other flag.

`fast-build` and `eval-jobs --flake` do not prefetch. An evaluation often
uses only some of the inputs, so a prefetch can download trees that nothing
reads, and parallel workers already share one download per tree
(`docs/explanation/caching.md`).

Falls back on: `nix eval --apply`, `--write-to` or more than one
installable, `nix build --rebuild`, `--dry-run` or `--json`, an installable
that is a store path (`/nix/store/…`, or an `^outputs` suffix), a
non-local flake reference such as `nixpkgs#hello`, an installable that is
not a derivation under `nix build`, `--arg`/`--argstr` with a flake (nix
refuses that), `nix flake …` but `prefetch-inputs`, `nix run`, `nix shell`,
`nix profile`, `nix store`, `nix copy`, `nix repl`, `nix log`,
`nix why-depends`, every other subcommand, and any flag not
listed above.

`nix store prefetch-file` and `nix-prefetch-url` keep falling back on
purpose. Both add the file to the store and print its path, and ietsp has no
store to add it to. `ietsp prefetch` prints the same hash and nothing else
(`docs/reference/commands.md`).

## Fallback rules

Two triggers:

1. A flag or subcommand the parser does not implement, seen before any
   work starts.
2. An evaluation that hits `ErrorKind::Unsupported`: the network fetchers
   (`fetchurl`, `fetchTarball` without a `sha256`, `fetchGit`, `fetchMercurial`,
   `fetchClosure`), `getFlake` on anything but a local path, a flake
   reference outside the subset `src/flake/flakeref.rs` parses (`hg+…`,
   `sourcehut:`, an IP-literal host), `outputOf`, import-from-derivation (`import`,
   `scopedImport`, `readFile`, `readDir`, `pathExists`, `readFileType` or
   `hashFile` on a derivation output), a second copy of a `toFile` text or
   `.drv`, a read of a `.drv` this run made under `--no-tracing-cache`, and a flake
   whose `flake.lock` is
   missing or incomplete. Every other evaluation error is reported and
   exits 1, because the real nix would fail the same way and re-running it
   would double the cost.

The fallback prints one line on stderr and then `execve`s the real binary
with the same `argv`:

```
ietsp: falling back to nix-instantiate: '--parse' is not implemented
```

| variable | effect |
|---|---|
| `IETSP_NIX_BIN_DIR` | the directory the real binary is taken from |
| `IETSP_NO_FALLBACK=1` | print the reason and exit 1 instead of exec'ing |

Lookup order: `IETSP_NIX_BIN_DIR/<name>` when it exists there; else the
directory named by `share/ietsp/real-nix-bin` next to the farm entry on
`PATH` that resolves to ietsp; else the first `<name>` on `PATH` that is not
ietsp. Nothing found at all:
`ietsp: cannot fall back to <name>: not found on PATH`, exit 1.

A candidate is ietsp when its canonical path is `/proc/self/exe` **or** when
that path's file name is `ietsp`. A host can hold two builds of ietsp, and
the farm one of them made points at its own `bin/ietsp`, not at the binary
that is running. Such a candidate is skipped like ourselves: its marker is
read, and the walk over `PATH` goes on. Without the file-name rule the other
build looked like a foreign nix, so a flake-input fetch spawned it and the
child stopped at the `IETSP_FETCHING` guard
(`ietsp: recursive fetch ...`, `docs/explanation/flakes-and-sources.md`).

## Config

The classic names read nix.conf as Nix does, and what reaches the daemon is
in `docs/reference/settings.md`. On the command line, `--option name value` and the flag
aliases (`--store`, `-j`/`--max-jobs`, `--cores`, `-k`, `-K`) are overrides
applied in command-line order. `store = auto`, empty or `daemon` is the
daemon socket; `unix://PATH`, `ssh-ng://HOST` and `grpc://HOST:PORT[?…]` are
honoured; any other store falls back.

## Known divergences

`docs/reference/divergences.md` covers the evaluator. Classic mode adds:

- `store = auto` is the daemon socket. The real nix would use the local
  store directly when it is writable.
- `-Q`/`--no-build-output` drops the daemon's log lines. In Nix 2.34 it
  only switches the log format to `raw`.
- `--show-trace` only puts the trace into the `--log-format internal-json`
  error records (`docs/reference/errors.md`): ietsp always prints the full
  trace it has.
- `--no-out-link` creates no symlink at all. `nix-build` links into a
  temporary directory and lets it be deleted, which roots the output for
  the length of the run.
- `getDerivations` recursion covers derivations, attrsets (lexicographic
  order, `recurseForDerivations`) and lists. `_combineChannels` and
  `ignoreAssertionFailures` are not implemented.
- `--add-root` does not refuse a link that already exists and points
  outside the store; it replaces it.
- The tracing cache (`docs/explanation/caching.md`) is off in classic mode.
- `nix eval` does not register the derivations it evaluates. The real one
  writes the `.drv` files while computing `drvPath`.
- `nix build` prints the log lines only with `-L`. The real one shows the
  last line in the progress bar and the whole log when a build fails.
- Two installables that name the same derivation print one output path
  each. The real `nix build` prints one per pair, so twice as many.
- An evaluation error inside a value `nix eval` prints becomes
  `«error: …»`, except `ErrorKind::Unsupported`, which must escape so the
  run can fall back to the real nix.
- **`nix develop` takes bash from the shell derivation's own builder.**
  `develop.cc` builds `nixpkgs#bashInteractive` from the flake's `nixpkgs`
  input (or the registry), which needs a fetcher ietsp does not have. The
  builder of a derivation `nix develop` accepts is a `bin/bash` in the store
  by definition, and in a nixpkgs dev shell it is the same store path nix
  picks, so `SHELL` and the `PATH` prefix come out the same. With no such
  file, `bash` from `PATH`, and then the `PATH` prefix line is omitted, as in
  nix when `bashInteractive` fails.
- `nix develop` does not use the `print-dev-env` tracing cache, so it always
  evaluates. It sets no profile: `--profile` falls back.
- `nix-shell --pure` keeps the lower-case proxy variables (`http_proxy` …)
  and not the upper-case ones. `network-proxy.cc:getAllVariables` means to add
  the upper-case names but writes into an empty `std::string`, so nix inserts
  an empty name instead; ietsp keeps the set nix keeps, not the set nix means
  to keep.
- `nix-shell` computes `NIX_BUILD_CORES` from the `cores` setting, else the
  cgroup CPU quota, else the CPU count — `globals.cc:getDefaultCores`, with
  the cgroup2 `cpu.max` of the process's own cgroup and no `/proc/mounts`
  check.
- `nix-shell` leaves the derivation's `.drv` in the store. Nix registers it
  too (`evalStore` is the store here), so this is only visible with a
  separate eval store.

## Tests

`scripts/classic-e2e.nu` runs the real
`nix-instantiate`/`nix-build`/`nix-shell`/`nix` and ours for every case and
compares stdout byte for byte, the exit code and the `result*` symlink targets.
The `nix` cases use `tests/fixtures/flake`, a flake with no inputs and a
complete `flake.lock`, copied out of the repository so both sides see a `path:`
flake. It writes to the store and needs a live `nix-daemon`, so it is not part
of `nix flake check`; run it from the dev shell after `cargo build --release`.

The shell cases compare an `env` dump, which is only comparable without the
variables that legitimately differ: `NIX_BUILD_TOP`, the `TMP*` family,
`NIX_ATTRS_*_FILE`, `__*`, `SHLVL`, `_`, `PWD`, `OLDPWD` and `PS1`. The rest is
compared line by line after `sort`. `nix-shell` runs on
`<nixpkgs>/default.nix -A hello`, a structured-attributes derivation, not on
`'<nixpkgs>' -A hello`: that resolves to `nixpkgs/shell.nix`, which fetches a
pinned tree and therefore falls back. `nix develop` runs on this repository's
own flake, the only `mkShell`-shaped shell around — the flake fixture has no
nixpkgs. The fallback cases use `IETSP_NO_FALLBACK=1`, so no real `nix-shell -p`
or `nix develop --phase build` build ever runs.

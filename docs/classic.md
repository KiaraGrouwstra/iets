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

`nix/module.nix` does not install this package: where the farm sits on
`PATH` relative to the real nix is a separate decision.

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

Pure fallback: every subcommand execs the real `nix`. The name exists so
the symlink set is complete.

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

## Tests

`scripts/classic-e2e.nu` runs the real `nix-instantiate`/`nix-build` and
ours for every case and compares stdout byte for byte, the exit code and
the `result*` symlink targets. It writes to the store and needs a live
`nix-daemon`, so it is not part of `nix flake check`; run it from the dev
shell after `cargo build --release`.

# direnv: `ietsp print-dev-env`

`ietsp print-dev-env [FLAKEREF[#attr]]` prints the bash that
`nix print-dev-env --option eval-cache false FLAKEREF` prints, byte for byte,
and replaces nix-direnv's `use flake` in `.envrc`. Code: `src/devenv/`,
`cmd_print_dev_env` in `src/main.rs`. Judge: `scripts/devenv-test.sh`.

```
ietsp print-dev-env [--builder URL] [--gc-root PATH] [--json] [--workers N]
                    [--no-cache] [--cache-dir DIR] [--timings] [FLAKEREF[#attr]]
```

- `FLAKEREF` defaults to `.`; the same local references as `ietsp eval`
  (`docs/flakes.md`). Without `#attr` the shell is the first of
  `devShells.<system>.default`, `devShell.<system>`,
  `packages.<system>.default`, `defaultPackage.<system>`; with `#x` the
  first of `devShells.<system>.x`, `packages.<system>.x`,
  `legacyPackages.<system>.x`, `x` (develop.cc, installable-flake.cc).
- `--builder` is a Nix daemon: `daemon` (default), `unix:///path`,
  `ssh-ng://[user@]host`. The daemon substitutes or builds the shell's inputs.
- `--gc-root PATH` makes `PATH` a symlink to the environment's store path
  and registers it as an indirect GC root (`AddIndirectRoot`), what
  nix-direnv's `--profile` does with a profile.
- `--json` prints what `nix print-dev-env --json` prints.

## `.envrc`

```bash
use_ietsp() {
  local root; root="$(direnv_layout_dir)/ietsp-env"
  watch_file flake.nix flake.lock
  eval "$(ietsp print-dev-env --gc-root "$root" "${1:-.}")"
}
use_ietsp
```

The printed bash is what `nix develop` sources: variable assignments,
`export`s, `declare -a`/`declare -A`, function definitions, the
`PATH`/`XDG_DATA_DIRS` prepend, `NIX_BUILD_TOP` from `mktemp`, and
`eval "${shellHook:-}"`. No `exit`, no `set -e`, no `exec`. Errors go to
stderr with a non-zero status and nothing on stdout, so `eval` of an empty
string leaves the environment alone.

## How it works (develop.cc)

1. Evaluate the shell derivation through the normal flake path, so the
   tracing cache (`docs/tracing-cache.md`) applies.
2. Derive the `-env` derivation in memory as `getDerivationEnvironment`
   does: same inputs, `args = [get-env.sh]` (Nix's `src/nix/get-env.sh`,
   embedded verbatim, added to the store as a text path), same `bash`
   builder, `name` with `-env` appended, `outputChecks` /
   `allowedReferences`-style checks removed, output paths recomputed with
   `finalize_input_addressed_outputs`. The result is the same `.drv` Nix
   writes (`devenv::tests::env_derivation_matches_nix`), so a shell
   `nix develop` already built is not built again.
3. `QueryValidPaths` on the env output. If the daemon lacks it: plan the
   closure from the in-memory drv table (`ietsp build`'s planner),
   `AddMultipleToStore` what the daemon lacks, `BuildPathsWithResults`.
   Build log lines are kept and printed with an `ietsp: ` prefix only on
   failure, or streamed with `--timings`.
4. Read the first non-empty output: from disk for a local daemon,
   `NarFromPath` over `ssh-ng://`. Parse it as `BuildEnvironment::fromJSON`.
5. Print `makeRcScript`: the same header, `toBash` with the same
   `ignoreVars`, the same footer, and the same rewrite of every output path
   to `<cwd>/outputs/<name>`. A `__structuredAttrs` shell gets its
   `.attrs.sh` and `.attrs.json` written next to the GC root
   (`PATH-attrs/`), or under `$TMPDIR/ietsp-dev-env-<pid>/` without one;
   Nix writes them to a temporary directory it deletes on exit.

## Cache

The tracing cache stores the printed bytes, the env output path and the
attrs files in the entry's `result`. A hit is: validate the read set, one
`stat` of the output (and a `QueryValidPaths` for a remote daemon), a
`readlink` of the GC root, and the print. Nothing talks to the daemon
unless the root must be (re)created. When the output was garbage
collected, the run falls back to the full path and rebuilds it.

## Not done

- Shells whose builder is not `bash`, and content-addressed shells (error).
- `--gc-root` with `ssh-ng://` (error: the root would name a remote path).
- `--redirect`, `--profile`, `--command`, `--phase` of `nix develop`.
- The `nix develop` interactive extras (`bashInteractive`, prompt settings).

## Measurements

Same machine as `docs/flakes.md`, 2026-09-16, local daemon, run from the
flake directory:

| run | wall |
|---|---|
| `nix print-dev-env --option eval-cache false ~/home` | 3.48 s |
| `ietsp print-dev-env ~/home`, cache miss, env output valid on the daemon | 0.67-0.83 s (eval 0.61-0.77 s, daemon 0.05 s) |
| `ietsp print-dev-env ~/home`, cache hit | 21-35 ms (2,535 observations validated in 18 ms) |
| `ietsp print-dev-env ~/iets-prime`, cache hit | 57-60 ms |
| scratch mkShell flake, env output deleted, cache hit: fallback, plan, build | 0.65 s |

`scripts/devenv-test.sh` prints the warm time (case c) on every run.

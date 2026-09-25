# How to use ietsp with direnv

`ietsp print-dev-env` prints the bash that `nix print-dev-env` prints, byte
for byte. It replaces nix-direnv's `use flake` in an `.envrc`, and it
answers from the cache in tens of milliseconds instead of seconds.

The flags are in [reference/commands.md](../reference/commands.md).

## Write the `.envrc`

Add this to your `.envrc`:

```bash
use_ietsp() {
  local root; root="$(direnv_layout_dir)/ietsp-env"
  watch_file flake.nix flake.lock
  eval "$(ietsp print-dev-env --builder daemon --gc-root "$root" "${1:-.}")"
}
use_ietsp
```

Then `direnv allow`.

Three parts matter:

- `--builder daemon` builds or substitutes the shell's inputs on the local
  daemon. The shell runs here, so its inputs belong in the local store.
- `--gc-root PATH` makes `PATH` a symlink to the environment's store path
  and registers it as an indirect garbage-collection root. That is what
  nix-direnv's `--profile` does with a profile. Without it, the next
  `nix-collect-garbage` deletes the environment and the following `direnv
  reload` rebuilds it.
- `watch_file flake.nix flake.lock` is direnv's own reload trigger, not
  ietsp's.

The printed bash is what `nix develop` sources: variable assignments,
exports, `declare -a` and `declare -A`, function definitions, the `PATH`
and `XDG_DATA_DIRS` prepend, `NIX_BUILD_TOP` from `mktemp`, and
`eval "${shellHook:-}"`. There is no `exit`, no `set -e` and no `exec`. On
an error nothing goes to stdout and the status is non-zero, so `eval` of an
empty string leaves your environment alone.

## Use the NixOS module instead

If you run NixOS, import `nixosModules.default`. It installs ietsp and
replaces nix-direnv's `use flake` with the repository's own `direnvrc`, for
local flake references with an optional `#attr`. Keep `use flake` in your
`.envrc` and change nothing else.

## Pick a different shell

Without a fragment, `print-dev-env` takes the first of
`devShells.<system>.default`, `devShell.<system>`,
`packages.<system>.default` and `defaultPackage.<system>`:

```sh
ietsp print-dev-env .
```

With `#x` it takes the first of `devShells.<system>.x`,
`packages.<system>.x`, `legacyPackages.<system>.x` and `x`:

```sh
ietsp print-dev-env .#ci
```

## Check what it costs

`--timings` reports the cache and the total on stderr, leaving stdout for
the bash:

```sh
ietsp print-dev-env --timings . > /dev/null
```

A cold run on this repository:

```
total: 0.703s
```

The same command again:

```
cache: hit, 2106 entries validated, 0 rehashed, load 0.001s, validate 0.017s
total: 0.027s
```

A hit validates the read set, stats the output, reads the garbage-collection
root's link, and prints. Nothing talks to the daemon unless the root has to
be created again. If the environment was garbage collected, the run falls
back to the full path and rebuilds it.

For scale, on a NixOS flake with 31 locked inputs and a devShell:
`nix print-dev-env --option eval-cache false` took 3.48 s, a cold
`ietsp print-dev-env` took 0.67-0.83 s, and a cached one 21-35 ms.

## Inspect the environment as JSON

```sh
ietsp print-dev-env --json .
```

This is what `nix print-dev-env --json` prints.

## What it does under the hood

Five steps, following Nix's own `develop.cc`, so you can predict when it
will be slow:

1. Evaluate the shell derivation through the normal flake path, so the
   tracing cache applies.
2. Derive the `-env` derivation in memory, as `getDerivationEnvironment`
   does: same inputs, the arguments replaced by Nix's own `get-env.sh`
   (embedded verbatim and added to the store as a text path), the same
   `bash` builder, `-env` appended to the name, the output checks removed,
   and the output paths recomputed. The result is the same `.drv` Nix
   writes, which is checked by a unit test, so a shell `nix develop`
   already built is not built again.
3. Ask the daemon whether it has the environment's output. If not, plan the
   closure from the in-memory derivation table, upload what the daemon
   lacks, and build.
4. Read the first non-empty output — from disk for a local daemon, over the
   protocol for `ssh-ng://` — and parse it.
5. Print the same script Nix prints: the same header, the same ignored
   variables, the same footer, and the same rewrite of every output path to
   `<cwd>/outputs/<name>`. A `__structuredAttrs` shell gets its `.attrs.sh`
   and `.attrs.json` written next to the garbage-collection root, or under
   a temporary directory when there is none. Nix writes them to a temporary
   directory it deletes on exit.

## What is not supported

- A shell whose builder is not `bash`, and a content-addressed shell. Both
  are errors.
- `--gc-root` together with `ssh-ng://`: the root would name a path this
  machine has not got.
- `nix develop`'s `--redirect`, `--profile`, `--command` and `--phase`.
- The `nix develop` interactive extras (`bashInteractive`, prompt
  settings).

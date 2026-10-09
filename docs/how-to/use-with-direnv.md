# How to use iets with direnv

`iets print-dev-env` prints the bash that `nix print-dev-env` prints, byte
for byte. It replaces nix-direnv's `use flake` in your `.envrc`, and a
cached run takes tens of milliseconds instead of seconds.

The flags are in [reference/commands.md](../reference/commands.md). How it
makes the shell is in [explanation/dev-shells.md](../explanation/dev-shells.md).

## Write the `.envrc`

Add this to your `.envrc`:

```bash
use_iets() {
  local root entry script
  local -a paths
  root="$(direnv_layout_dir)/iets-env"
  entry="$(direnv_layout_dir)/iets-entry"
  watch_file flake.nix flake.lock
  script="$(iets print-dev-env --builder daemon --gc-root "$root" --entry-to "$entry" "${1:-.}")" || true
  if [[ -s $entry && -e $(<"$entry") ]]; then
    mapfile -d '' -t paths < <(iets cache paths "$(<"$entry")" 2>/dev/null)
    ((${#paths[@]})) && watch_file "${paths[@]}"
  fi
  eval "$script"
}
use_iets
```

Then run `direnv allow`.

Here is what each part does:

- `--builder daemon` builds or substitutes the shell's inputs on your local
  daemon. The shell runs here, so its inputs belong in the local store.
- `--gc-root PATH` makes `PATH` a symlink to the environment and registers
  it as a garbage-collection root, like nix-direnv's `--profile`. Without
  it, the next `nix-collect-garbage` deletes the environment, and the next
  `direnv reload` builds it again.
- `watch_file flake.nix flake.lock` is the minimum. nix-direnv watches the
  same two files, and it is all direnv has before the first cache entry.
- `--entry-to FILE` writes the name of the cache entry the run used.
  `iets cache paths ENTRY` prints the paths to watch, separated by NUL
  bytes. `mapfile` turns them into one `watch_file` call. Do this before
  `eval "$script"`, so a failing shell hook does not lose the watches.
- **Use one `watch_file` call, not one per path.** Each call starts the
  direnv binary again, and one call takes as many paths as you give it. For
  500 paths, that is 25 ms in one call against 7.3 s one at a time. The NUL
  separator lets `mapfile` keep a path with spaces in one piece.
- Passing the entry name, rather than working it out again, means the two
  calls can use different flags without silently finding nothing.

The printed bash is what `nix develop` sources: variable assignments,
exports, `declare -a` and `declare -A`, function definitions, the `PATH`
and `XDG_DATA_DIRS` prepend, `NIX_BUILD_TOP` from `mktemp`, and
`eval "${shellHook:-}"`. There is no `exit`, no `set -e` and no `exec`. On
an error, nothing goes to stdout and the status is non-zero, so `eval` of
the empty string leaves your environment alone.

## What direnv reloads on

nix-direnv watches `flake.nix` and `flake.lock`, and leaves the rest to
`direnv reload`. iets does not have to guess. Its cache records everything
the evaluation looked at
([explanation/caching.md](../explanation/caching.md)), and
`iets cache paths` turns that into direnv watches:

| the evaluation | direnv watches |
|---|---|
| read a file: `import ./nix/shell.nix`, `readFile`, `importJSON`, `flake.nix`, `flake.lock`, a file of a `path:` input | the file |
| listed a directory: `readDir ./conf.d` | the directory, whose mtime changes when an entry comes or goes |
| asked for the kind of a path that is **missing**: `pathExists ./local.nix`, the nixpkgs `config.nix` probes | the path, because direnv sees it appear |
| asked for the kind of a path that **exists** | nothing: existence was the observation, and an mtime is not that (a git index refresh moves `.git`) |
| copied a path into the shell: `"${./hook.sh}"`, `./scripts` | the path and everything the copy took below it |
| asked the source control system about a working tree | the file that the system names; for git, the index, which `git add`, a commit and a checkout rewrite |

In a git flake, an untracked file is not part of the flake. `git add` makes
it one, the index changes, and direnv reloads.

Some things are left out on purpose:

- Everything under `/nix/store` and in iets's own stores. Those paths never
  change.
- The flake's source tree as a whole. Every edit of any file changes it,
  and few shells depend on it, so watching it would cost an evaluation for
  every file you save. direnv watches the files the evaluation read and the
  paths the shell copied, and no more. If your shell uses `self` or `./.`,
  run `direnv reload` yourself, or add your own `watch_file`.
- Environment variables (`getEnv` under `--impure`). direnv cannot watch a
  variable. The cache still checks them, so the next reload for any other
  reason evaluates again.
- More than 500 paths. direnv checks every watched path at every prompt,
  so 500 watches add about 3 ms to a prompt. iets keeps the first 500, in
  the order the evaluation read them, and tells you on stderr. A usual
  flake shell has 10 to 20.

`iets cache paths` checks nothing and evaluates nothing: it reads one cache
entry and prints. If the evaluation fails, the entry of the last good run
is still there, so direnv still watches the file you need to fix.

If the very first load fails, there is no entry yet. `--entry-to` leaves
the file empty, and the `[[ -s $entry ]]` check skips the loop. direnv then
watches only `flake.nix` and `flake.lock`, which is right: iets knows
nothing yet about a shell that never evaluated. Fix the error on stderr.

## Use the NixOS module instead

On NixOS, import `nixosModules.default`. It installs iets, and replaces
nix-direnv's `use flake` with this repository's own `direnvrc`, for local
flake references with an optional `#attr`. Keep `use flake` in your
`.envrc` and change nothing else.

## Pick a different shell

Without a fragment, `print-dev-env` takes the first of
`devShells.<system>.default`, `devShell.<system>`,
`packages.<system>.default` and `defaultPackage.<system>`:

```sh
iets print-dev-env .
```

With `#x`, it takes the first of `devShells.<system>.x`,
`packages.<system>.x`, `legacyPackages.<system>.x` and `x`:

```sh
iets print-dev-env .#ci
```

## Check what it costs

`--timings` shows the cache and the total time on stderr, and leaves stdout
for the bash:

```sh
iets print-dev-env --timings . > /dev/null
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

For a larger example, on a NixOS flake with 31 locked inputs and a
devShell: `nix print-dev-env --option eval-cache false` took 3.48 s, a cold
`iets print-dev-env` took 0.67-0.83 s, and a cached one 21-35 ms.

## Inspect the environment as JSON

```sh
iets print-dev-env --json .
```

This prints what `nix print-dev-env --json` prints.

## What is not supported

- A shell whose builder is not `bash`, and a content-addressed shell. Both
  are errors.
- `--gc-root` together with `ssh-ng://`: the root would name a path this
  machine does not have.
- `nix develop`'s `--redirect`, `--profile`, `--command` and `--phase`.
- The `nix develop` interactive extras (`bashInteractive`, prompt
  settings).

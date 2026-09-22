# Pure evaluation

`pure-eval` as Nix 2.34.8 has it (`eval-settings.hh`, `eval.cc`,
`primops.cc`, `fetchTree.cc`, `flake-primops.cc`). An evaluation in this
mode depends only on its locked inputs. The answers and error messages
are Nix's; the test `eval::tests::pure_eval_answers_like_nix`
holds them, and `scripts/classic-e2e.nu` compares the defaults below
against the real nix.

## When it is on

The same rules as Nix, per command:

| Command | Default | Changed by |
|---|---|---|
| `nix-instantiate`, `nix-build`, `nix-shell` | nix.conf `pure-eval`, else off | `--pure-eval`, `--no-pure-eval`, `--impure`, `--option pure-eval`; the last one wins |
| `nix eval`, `nix build`, `nix develop`, `nix print-dev-env` | on (main.cc); nix.conf does not change it | the same flags. `-f`/`--file` turns it off, and is a usage error when `pure-eval` was set anywhere, nix.conf included (installables.cc); ietsp falls back on that error |
| `nix-eval-jobs` | `--flake`: on. Otherwise nix.conf, else off | `--impure` beats `--flake`; without `--flake`, `--pure-eval` and `--option pure-eval` |
| `nix-fast-build` | flake mode: on. `--file` mode: off | `--impure`; `--file --pure` leaves it to nix.conf, as nix-eval-jobs does |
| `ietsp eval`, `ietsp build`, `ietsp eval-jobs` | off | `--pure-eval`, `--no-pure-eval`, `--impure`, `--option pure-eval`; `eval-jobs --flake` as nix-eval-jobs. nix.conf is read by `eval-jobs` only |

`nix-shell --impure` is the shell's environment flag (nix-build.cc takes
it), not `pure-eval`.

## What it changes

- `builtins.currentSystem` and `builtins.currentTime` do not exist
  (`impureOnly` constants): `builtins ? currentSystem` is false and
  `__currentSystem` is an undefined variable.
- `builtins.getEnv` returns `""`.
- The search path is empty, `-I` and `NIX_PATH` included, so
  `builtins.nixPath` is `[ ]` and `<x>` fails with `cannot look up '<x>'
  in pure evaluation mode`. `<nix/fetchurl.nix>` still works.
- A file is readable only under a store path this evaluation allowed:
  one it copied (`"${./x}"` of an allowed path, `builtins.path`,
  `filterSource`), a `toFile` text, the flake's own tree and its inputs,
  or a tree a fetch produced (`fetchTree`, `fetchTarball`). Everything
  else is `access to absolute path '...' is forbidden in pure evaluation
  mode (use '--impure' to override)`: `import` (the file on the command
  line too), `readFile`, `readDir`, `readFileType`, `hashFile`,
  `findFile` entries, string coercion and `builtins.path` of a local
  path. `pathExists` answers `false` instead. A `.drv` this evaluation
  wrote is not readable either, but `import drv.drvPath` works: Nix reads
  it through the store, not the accessor.
- `builtins.storePath` is refused.
- `fetchTarball` needs a `sha256`; `fetchTree` refuses an unlocked input
  (no `narHash` for `path` and `tarball`, no `rev` for the rest);
  `getFlake` refuses an unlocked reference, and prints `at «none»:0`
  because Nix does.

`builtins.path` with a `sha256` whose store path already exists allows
it without reading the source, as `allowAndSetStorePathString` does.

## What stays unsupported

Classic mode falls back to the real nix on these, so the answer is still
Nix's:

- an unlocked `git` or `hg` input: Nix names it with query parameters
  (`?shallow=1`) that ietsp does not reproduce;
- `getFlake` on a reference that may be locked (a `?query` or a 40-digit
  rev);
- `restrict-eval`, which is a separate setting: ignored, as before.

## Caches

`pure-eval` is part of the tracing-cache key. The IR cache needs nothing:
an entry that names `__currentSystem` finds no such global in a pure run,
is dropped and recompiled, and the compile reports the undefined
variable.

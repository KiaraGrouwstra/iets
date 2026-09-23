# Package search

`nix search` and `ietsp search` print the packages of an installable whose
attribute path, name or description match every regular expression given.
The output is Nix 2.34.8's, byte for byte. The walk that costs is
evaluation, so the result of the walk — the index — goes into the tracing
cache: a second search over the same package set answers without
evaluating, whatever the regexes are.

Code: `src/cli/search.rs`.

```
nix search INSTALLABLE [FLAGS] REGEX...
ietsp search [INSTALLABLE] [FLAGS] REGEX...
```

`^` matches every package. Several regexes must all match. `-e`/`--exclude
REGEX` hides a package whose attribute path, name or description match.
The regular expressions are POSIX extended and case insensitive, as
`std::regex::extended | std::regex::icase` in `search.cc`.

## The walk

`src/nix/search.cc`, attribute by attribute, in name order:

- A derivation (an attribute set whose `type` is `"derivation"`) is one
  result: its attribute path, `name` split into pname and version
  (`names.cc:DrvName`: the name runs up to the first `-` that is not
  followed by a letter), and `meta.description` with its newlines turned
  into spaces. A `meta` or a `meta.description` that is not there, or a
  `meta` that is not an attribute set, means no description.
- The walk descends into the value the installable named, into the two
  levels under `packages` and `legacyPackages`, and, under
  `legacyPackages` only (`ietsp search`: anywhere), into an attribute set with
  `recurseForDerivations = true`. Nothing else is opened: under a
  `--file` or `--expr` installable only the top level is searched.
- An evaluation error under `legacyPackages` (`ietsp search`: anywhere)
  skips that attribute, as `catch (EvalError &)` does in `search.cc`.
  Anywhere else it stops the search. A feature ietsp does not have is not
  an evaluation error: under a classic name it falls back to the real
  `nix`.

A flake installable searches `packages.<system>` and
`legacyPackages.<system>`; a fragment (`FLAKE#gnome3`) searches
`packages.<system>.gnome3`, `legacyPackages.<system>.gnome3` and `gnome3`,
each of them that exists.

## Output

Text, one block per result, separated by empty lines:

```
* legacyPackages.x86_64-linux.hello (2.12.3)
  Program that produces a familiar, friendly greeting
```

The matched substrings are highlighted. Nix writes the escape sequences to
stdout whether or not stdout is a terminal (`logger->cout` does not filter
them, only `log()` does), so ietsp does the same: piping the output keeps
the colours. Use `--json` for a program.

`--json` prints one object per result, keyed by attribute path, with
`pname`, `version` and `description`, as `nlohmann::json` dumps it: keys
sorted, indented with `--pretty`, on one line with `--no-pretty`. The
default is to indent when stdout is a terminal. No result and no `--json`
is the error `no-results` and exit 1 (`docs/cli.md`); with `--json` it is
`{}` and exit 0.

Nix prints each result as it finds it and an `evaluating '<attr>'` line on
stderr while it walks. ietsp builds the whole index first — the cache
needs it — and prints after, without the progress lines.

## The installable

Under the classic name, the installable is Nix's: the first argument, a
flake reference or the attribute path inside `--file`/`--expr`. ietsp has
no flake registry, so `nix search nixpkgs ^` falls back to the real `nix`;
`nix search path:/some/flake ^` does not (`docs/flakes.md`).

`ietsp search` takes the installable only when the first argument names
one: a flake reference, `<name>`, or a path that is there. Everything else
is a regex, and the default installable is `<nixpkgs>` from `-I` or
`NIX_PATH`, evaluated impurely as `--file` is. Two things the native
command does that `nix search` does not, because its installable is
usually a package set and not a flake output:

- it auto-calls the root value with the `--arg`s, so a package set that
  is a function — nixpkgs' own `default.nix` — is searched as it is;
- it honours `recurseForDerivations` and skips evaluation errors at every
  attribute path, not only under `legacyPackages`. Over the pinned
  nixpkgs that finds the same 112,215 packages, with the same pnames,
  versions and descriptions, as `nix search` finds under
  `legacyPackages.<system>`.



```
ietsp search hello                     # <nixpkgs>, impure
ietsp search ./pkgs.nix '^'            # a file
ietsp search path:/flake ripgrep       # a locked flake
ietsp search --expr 'import <nixpkgs> {}' git gui
```

## The index cache

The index is the list of `(attribute path, pname, version, description)`
the walk produced. It is the `result` of a tracing-cache entry
(`docs/tracing-cache.md`), keyed on the installable, the `--arg`s, the
search path, `pure-eval` and the build id — not on the regexes, so any
search over an unchanged package set is a hit. A hit validates the read
set and answers from the index: for the pinned nixpkgs that is 56,170
observations in 0.08 s against 6.5 s of evaluation, and an index of
4 MB.

`IETSP_CACHE=0` evaluates again, `IETSP_CACHE_DIR` moves the store
(`docs/settings.md`), and `ietsp search --timings` reports the hit or
miss. The classic name takes no `--timings`; the environment variables
work under both names.

## Numbers

The pinned nixpkgs (`/nix/store/ah3pxz4xa52hd6vhbx17vcsci0b8bmbr-source`,
30,000 packages) as `{ legacyPackages.${builtins.currentSystem} = import
<pin> {}; }`, on a 16-core EPYC shared with other work, release build,
2026-09-23. Wall time, median of three:

| run | `^hello$` | all packages (`^`) |
|---|---|---|
| ietsp, no index yet | 6.5 s | 6.8 s |
| ietsp, index cached | 0.15 s | 0.47 s |
| real `nix search`, the same installable | 21 s | 21 s |
| real `nix search`, the same tree as a flake, its cache warm | 1.8 s | 2.6 s |
| real `nix search`, the same tree as a flake, its cache cold | 37 s | — |

Real nix keeps its own eval cache for a flake installable only; with
`--expr` or `--file` it evaluates every time. ietsp caches both alike.
The whole `^ --json` output, 18.8 MB, is byte for byte the same on both
sides.

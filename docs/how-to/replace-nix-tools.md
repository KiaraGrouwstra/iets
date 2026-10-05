# How to replace your Nix tools with iets

One binary covers seven separate tools, and they share one evaluator and
one cache. This page gives a worked example of each replacement.

| you used | use instead |
|---|---|
| `nix search` | `iets search` |
| `nix-diff` | `iets diff` |
| `nvd` | `iets diff --closure` |
| `nurl`, `nix-prefetch`, `nix-prefetch-url` | `iets prefetch` |
| `nix-fast-build` | `iets build .#checks` |
| `nix-eval-jobs` | `iets eval-jobs` |
| nix-direnv's `use flake` | `iets print-dev-env` |

Every flag is in [reference/commands.md](../reference/commands.md). Where
behaviour differs from the tool being replaced on purpose, it is recorded
in [reference/divergences.md](../reference/divergences.md).

Under the classic names — `nix`, `nix-build`, `nix-instantiate`,
`nix-shell`, `nix-eval-jobs` — the same binary answers
as that command and runs the real `nix` for what it does not implement.
See [reference/classic.md](../reference/classic.md).

## `nix search`

```sh
iets search -I nixpkgs=$nixpkgs '^ripgrep$'
```

```
* ripgrep (15.2.0)
  Utility that combines the usability of The Silver Searcher with the raw speed of grep
```

The output is Nix 2.34.8's, byte for byte, escape sequences included. The
matched substrings are highlighted, and — as in Nix — the colours are
written whether or not stdout is a terminal. Use `--json` for a program.

The first search over a package set evaluates it. The *index* that the walk
produced then goes into the cache, keyed on the installable and not on the
regular expressions, so any later search over the same set is a hit:

```sh
iets search --timings -I nixpkgs=$nixpkgs fd find
```

```
cache: hit, 56568 entries validated, 0 rehashed, load 0.017s, validate 0.038s
```

For the pinned nixpkgs: 6.5 s the first time, 0.15 s thereafter, against
21 s for real `nix search` on the same installable. Real Nix keeps an
evaluation cache for a flake installable only; with `--file` or `--expr` it
evaluates every time.

Without an installable, `iets search` searches `<nixpkgs>` from `-I` or
`NIX_PATH`. Give one explicitly when you want another:

```sh
iets search ./pkgs.nix '^'
iets search path:/some/flake ripgrep
iets search --expr 'import <nixpkgs> {}' git gui
```

`iets search` has no flake registry, so `nix search nixpkgs ^` under the
classic name falls back to the real `nix`.

## `nix-diff`

```sh
iets diff -A a -A b demo.nix
```

```
- /nix/store/623kpc3y80b1cmfpp41ndkx8y2ila1az-demo.drv
+ /nix/store/sx7zfirysrh620gfmv0ycy5p3jwfzpzz-demo.drv
  args:
    @@ line 2
    - "echo one > $out"
    + "echo two > $out"
  env GREETING:
    - hello
    + goodbye
```

`-` is the first side and `+` the second. The `-A X -A Y FILE` form
evaluates one file twice and selects a different attribute each time. The
other form takes two installables — a `.drv` path, a flake reference or a
Nix file — each of which must evaluate to exactly one derivation:

```sh
iets diff /nix/store/…-a.drv /nix/store/…-b.drv
```

Exit codes follow `diff(1)`: 0 when the two are equal, 1 when they differ,
2 for a usage error or any other failure.

Nothing is built and nothing is written to the store. A derivation this
evaluation produced is read from memory; any other is read from
`/nix/store`.

The walk descends two levels of input derivations by default. Use
`--depth N` for more, or `--all` for every level. A stop is always printed,
so nothing goes missing silently.

`--json` prints one object in which every difference is an entry of
`changes`, whatever part of the derivation it came from, so a program
filters on one key and never needs a shape per part:

```sh
iets diff --json -A a -A b demo.nix
```

```json
{"a":"/nix/store/623k…-demo.drv","b":"/nix/store/sx7z…-demo.drv",
 "changes":[{"a":"\"-c\"\n\"echo one > $out\"\n","b":"\"-c\"\n\"echo two > $out\"\n","key":"","what":"args"},
            {"a":"hello","b":"goodbye","key":"GREETING","what":"env"}],
 "equal":false}
```

## `nvd`

`--closure` compares the two derivations' output closures by package and
version instead of comparing the derivations:

```sh
iets diff --closure -A a -A b demo.nix
```

```
the two closures hold the same packages
```

When they differ, each line is one package:

```
removed libfoo 1.2
added   libbar 0.9
changed hello 2.12 -> 2.12.1
```

Both outputs must be in the store, because the daemon answers with
`QueryPathInfo`. A path it has not got is an error that names the
`iets build` which makes it. `--builder URL` picks the store to read.

## `nurl`, `nix-prefetch`, `nix-prefetch-url`

The hash of something not fetched yet, on one line of stdout and nothing
else, so `$(iets prefetch …)` is usable directly.

A URL, hashed flat, as `nix store prefetch-file` hashes it:

```sh
iets prefetch https://github.com/numtide/treefmt/archive/v2.0.0.tar.gz
```

```
sha256-/Sb+qZOLHhmr1D63ANxTxkhA4uTOpn0IcUO2UmQyRuo=
```

`--unpack` gives the NAR hash of the unpacked tarball, which is what
`fetchTarball`, `fetchzip` and `fetchFromGitHub` want:

```sh
iets prefetch --unpack https://github.com/numtide/treefmt/archive/v2.0.0.tar.gz
```

```
sha256-Ckvpb2at7lg7AB0XNtIxeo8lWXX+04MVHHKoUs876dg=
```

gzip, xz, zstd and bzip2 are handled. Zip files are not. `--hash-type ALGO`
picks `md5`, `sha1`, `sha256` (the default) or `sha512`.

For anything a `fetch*` function makes, give an expression, a file with
`-A`, or a flake fragment instead of a URL:

```sh
iets prefetch -E '(import <nixpkgs> {}).hello.src'
iets prefetch -E 'with import <nixpkgs> {}; fetchzip {
    url = "https://github.com/numtide/treefmt/archive/v2.0.0.tar.gz";
    hash = ""; }'
iets prefetch default.nix -A mypkg.cargoDeps
```

The hash the expression names is ignored, so `hash = ""`, `lib.fakeHash`
and last month's hash all work. Nothing in your files is edited: iets
rewrites the *derivation* — the fixed output hash becomes the all-zero hash
of the same algorithm and the paths are recomputed — builds it, and reads
the hash out of the daemon's mismatch report. The fetcher runs exactly as
it would have.

There is no `-f FILE`. The file is a positional, as it is for `iets eval`
and `iets build`.

Unlike `nix store prefetch-file`, nothing is added to any store, which is
also why there is no `--name`, `--executable` or `--expected-hash`.

## `nix-fast-build`

There is no separate command. `iets build` takes a set of derivations,
so the checks of a flake are one installable:

```sh
iets build .#checks
```

```
▶ building checks.x86_64-linux.one (/home/you/fb/flake.nix:4:7)
▶ building checks.x86_64-linux.two (/home/you/fb/flake.nix:10:7)
✔  checks.x86_64-linux.one (/home/you/fb/flake.nix:4:7) (0s)
✔  checks.x86_64-linux.two (/home/you/fb/flake.nix:10:7) (0s)
2 built, 0 failed
/nix/store/…-one
/nix/store/…-two
```

It evaluates in this process and builds on one connection. The checks of
the current system are selected, every check gets a verdict, and what the
builder already has is not built again. None of that needs a flag. The
exit code is Nix's: 100 when a build failed.

```sh
iets build --builder ssh-ng://builder .#checks
iets build release.nix -A ci --argstr system x86_64-linux
iets build --json .#checks > outcomes.json
```

`--json` is the report: `{"outcomes":[{attr, drv, success, outputs,
reason, logTail}]}`. There is no JUnit file; convert the JSON where a
consumer needs one. Outputs stay where they were built, and nothing is
pushed to a binary cache: that is the builder's job.
[How to build on a remote builder](build-remotely.md) has the details.

## `nix-eval-jobs`

```sh
iets eval-jobs --no-instantiate demo.nix
```

```json
{"attr":"a","attrPath":["a"],"constituents":[],"drvPath":"/nix/store/623k…-demo.drv","globConstituents":false,"name":"demo","namedConstituents":[],"outputs":{"out":"/nix/store/9ymw…-demo"},"storeDir":"/nix/store","system":"x86_64-linux"}
{"attr":"b","attrPath":["b"],…}
```

These are the JSON lines nix-eval-jobs 2.35 prints. The comparison suite
runs both tools over 41 flag combinations and diffs the sorted output; all
41 are identical, exit codes included.

For Hydra and nixpkgs CI, the flags you already pass work:
`--force-recurse`, `--meta`, `--constituents`, `--check-cache-status`,
`--gc-roots-dir`, `--apply`, `--select`, `--flake`, `--max-memory-size`.

Three things to know before you swap it in:

- `--workers N` means worker *processes* here, and the default is 4, not 1.
  With `--max-memory-size 0` it means fibers of one process instead.
- Line order is completion order, as it is for nix-eval-jobs. Sort by
  `attrPath` when you compare two runs.
- `--override-input`, `--override-flake` and `--repair` are errors. iets
  evaluates a flake from a complete lock file only. `--reference-lock-file`
  works: that file is the lock, and it must be complete too.

Two flags are iets's own. `--provenance` adds a `pos` key per job, naming
the file, line and column where the derivation was defined; it is off by
default so the output stays identical to nix-eval-jobs. `--chunks M --chunk
N` splits a jobset, which is how nixpkgs CI shards its evaluation.

## nix-direnv

See [use-with-direnv.md](use-with-direnv.md).

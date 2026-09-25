# How to replace your Nix tools with ietsp

One binary covers seven separate tools, and they share one evaluator and
one cache. This page gives a worked example of each replacement.

| you used | use instead |
|---|---|
| `nix search` | `ietsp search` |
| `nix-diff` | `ietsp diff` |
| `nvd` | `ietsp diff --closure` |
| `nurl`, `nix-prefetch`, `nix-prefetch-url` | `ietsp prefetch` |
| `nix-fast-build` | `ietsp fast-build` |
| `nix-eval-jobs` | `ietsp eval-jobs` |
| nix-direnv's `use flake` | `ietsp print-dev-env` |

Every flag is in [reference/commands.md](../reference/commands.md). Where
behaviour differs from the tool being replaced on purpose, it is recorded
in [reference/divergences.md](../reference/divergences.md).

Under the classic names — `nix`, `nix-build`, `nix-instantiate`,
`nix-shell`, `nix-eval-jobs`, `nix-fast-build` — the same binary answers
as that command and runs the real `nix` for what it does not implement.
See [reference/classic.md](../reference/classic.md).

## `nix search`

```sh
ietsp search -I nixpkgs=$nixpkgs '^ripgrep$'
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
ietsp search --timings -I nixpkgs=$nixpkgs fd find
```

```
cache: hit, 56568 entries validated, 0 rehashed, load 0.017s, validate 0.038s
```

For the pinned nixpkgs: 6.5 s the first time, 0.15 s thereafter, against
21 s for real `nix search` on the same installable. Real Nix keeps an
evaluation cache for a flake installable only; with `--file` or `--expr` it
evaluates every time.

Without an installable, `ietsp search` searches `<nixpkgs>` from `-I` or
`NIX_PATH`. Give one explicitly when you want another:

```sh
ietsp search ./pkgs.nix '^'
ietsp search path:/some/flake ripgrep
ietsp search --expr 'import <nixpkgs> {}' git gui
```

`ietsp search` has no flake registry, so `nix search nixpkgs ^` under the
classic name falls back to the real `nix`.

## `nix-diff`

```sh
ietsp diff -A a -A b demo.nix
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
ietsp diff /nix/store/…-a.drv /nix/store/…-b.drv
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
ietsp diff --json -A a -A b demo.nix
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
ietsp diff --closure -A a -A b demo.nix
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
`ietsp build` which makes it. `--builder URL` picks the store to read.

## `nurl`, `nix-prefetch`, `nix-prefetch-url`

The hash of something not fetched yet, on one line of stdout and nothing
else, so `$(ietsp prefetch …)` is usable directly.

A URL, hashed flat, as `nix store prefetch-file` hashes it:

```sh
ietsp prefetch https://github.com/numtide/treefmt/archive/v2.0.0.tar.gz
```

```
sha256-/Sb+qZOLHhmr1D63ANxTxkhA4uTOpn0IcUO2UmQyRuo=
```

`--unpack` gives the NAR hash of the unpacked tarball, which is what
`fetchTarball`, `fetchzip` and `fetchFromGitHub` want:

```sh
ietsp prefetch --unpack https://github.com/numtide/treefmt/archive/v2.0.0.tar.gz
```

```
sha256-Ckvpb2at7lg7AB0XNtIxeo8lWXX+04MVHHKoUs876dg=
```

gzip, xz, zstd and bzip2 are handled. Zip files are not. `--hash-type ALGO`
picks `md5`, `sha1`, `sha256` (the default) or `sha512`.

For anything a `fetch*` function makes, give an expression, a file with
`-A`, or a flake fragment instead of a URL:

```sh
ietsp prefetch -E '(import <nixpkgs> {}).hello.src'
ietsp prefetch -E 'with import <nixpkgs> {}; fetchzip {
    url = "https://github.com/numtide/treefmt/archive/v2.0.0.tar.gz";
    hash = ""; }'
ietsp prefetch default.nix -A mypkg.cargoDeps
```

The hash the expression names is ignored, so `hash = ""`, `lib.fakeHash`
and last month's hash all work. Nothing in your files is edited: ietsp
rewrites the *derivation* — the fixed output hash becomes the all-zero hash
of the same algorithm and the paths are recomputed — builds it, and reads
the hash out of the daemon's mismatch report. The fetcher runs exactly as
it would have.

There is no `-f FILE`. The file is a positional, as it is for `ietsp eval`
and `ietsp build`.

Unlike `nix store prefetch-file`, nothing is added to any store, which is
also why there is no `--name`, `--executable` or `--expected-hash`.

## `nix-fast-build`

`ietsp fast-build` takes nix-fast-build's flags and writes its result
formats, so a wrapper that execs `nix-fast-build --store … <flags>` can
exec `ietsp fast-build --store … <flags>` instead:

```sh
ietsp fast-build --store daemon --flake .#checks
```

```
▶ building x86_64-linux.one (/home/you/fb/flake.nix:4:7)
▶ building x86_64-linux.two (/home/you/fb/flake.nix:10:7)
✔  x86_64-linux.one (/home/you/fb/flake.nix:4:7) (0s)
✔  x86_64-linux.two (/home/you/fb/flake.nix:10:7) (0s)
fast-build: 2 built, 0 cached, 0 for another system, 1 build ops, 2 paths uploaded (0 MB), first build at 1.228s, total 1.228s
fast-build: 2 succeeded, 0 failed
```

Common variants:

```sh
ietsp fast-build --flake .#checks.x86_64-linux --skip-cached
ietsp fast-build --file release.nix -A ci --argstr system x86_64-linux
ietsp fast-build --store ssh-ng://builder --result-format junit --result-file r.xml
```

nix-fast-build orchestrates processes: `nix-eval-jobs` for evaluation and
one `nix build` per attribute, with `nix copy` and `nix log` beside them.
ietsp does all of it in one process against one connection, so there is no
per-attribute process spawn and no second client re-evaluating the flake.

Three differences you will notice:

- **`--remote` is an error, not a no-op.** `--store ssh-ng://host` already
  does the job better: the flake is never copied and the evaluation never
  runs twice.
- **Uploading to a binary cache is an error.** `--copy-to`,
  `--cachix-cache`, `--attic-cache`, `--niks3-server` and
  `--push-build-closure` all refuse. Pushing what a build produced is the
  farm's job; a client that also pushes doubles the bytes on the slowest
  link in the system.
- **Outputs stay where they were built.** `--no-download` is accepted and
  is the effective behaviour. Copying outputs back registers foreign
  input-addressed paths locally, which needs trusted-user status that CI
  rarely has.

Under the classic name, an unknown flag is not an error: it falls back to
the real `nix-fast-build` if one is installed, so a missing feature
degrades instead of breaking a pipeline.

## `nix-eval-jobs`

```sh
ietsp eval-jobs --no-instantiate demo.nix
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
- `--override-input`, `--override-flake`, `--reference-lock-file` and
  `--repair` are errors. ietsp evaluates a flake from its own complete
  `flake.lock` only.

Two flags are ietsp's own. `--provenance` adds a `pos` key per job, naming
the file, line and column where the derivation was defined; it is off by
default so the output stays identical to nix-eval-jobs. `--chunks M --chunk
N` splits a jobset, which is how nixpkgs CI shards its evaluation.

## nix-direnv

See [use-with-direnv.md](use-with-direnv.md).

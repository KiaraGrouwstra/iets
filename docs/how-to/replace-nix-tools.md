# How to replace your Nix tools with iets

One binary does the job of seven separate tools, with one evaluator and one
cache. This page shows you an example of each.

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
iets differs from the old tool on purpose, you find it in
[reference/divergences.md](../reference/divergences.md).

The same binary also answers to the classic names `nix`, `nix-build`,
`nix-instantiate`, `nix-shell` and `nix-eval-jobs`. It acts like that
command, and runs the real `nix` for what it does not implement (see
[reference/classic.md](../reference/classic.md)).

The examples use `$nixpkgs` from step 3 of the
[tutorial](../tutorial/getting-started.md). By default, iets evaluates
purely, with no `<nixpkgs>` search path and no `builtins.currentSystem`. An
expression that reads either needs `--impure`.

## `nix search`

```sh
iets search -I nixpkgs=$nixpkgs '^ripgrep$'
```

```
* ripgrep (15.2.0)
  Utility that combines the usability of The Silver Searcher with the raw speed of grep
```

The output is Nix 2.34.8's, byte for byte, escape sequences included. The
matches are highlighted, and, as in Nix, the colours are written even when
stdout is not a terminal. Use `--json` for a program.

The first search over a package set evaluates it. iets then caches the
index of packages, not your search terms. So any later search over the
same set is a hit, whatever you search for:

```sh
iets search --timings -I nixpkgs=$nixpkgs fd find
```

```
cache: hit, 56568 entries validated, 0 rehashed, load 0.017s, validate 0.038s
```

For the pinned nixpkgs, that is 6.5 s the first time and 0.15 s after
that. Real `nix search` takes 21 s on the same installable. Nix caches the
evaluation of a flake installable only; with `--file` or `--expr` it
evaluates every time.

Without an installable, `iets search` searches `<nixpkgs>` from `-I` or
`NIX_PATH`. To search something else, name it:

```sh
iets search ./pkgs.nix '^'
iets search path:/some/flake ripgrep
iets search --impure --expr 'import <nixpkgs> {}' git gui
```

`iets search` has no flake registry, so `nix search nixpkgs ^` under the
classic name falls back to the real `nix`.

## `nix-diff`

The examples from here on use this `demo.nix`:

```nix
let
  mk = word: greeting: derivation {
    name = "demo";
    system = "x86_64-linux";
    builder = "/bin/sh";
    args = [ "-c" "echo ${word} > $out" ];
    GREETING = greeting;
  };
in
{
  a = mk "one" "hello";
  b = mk "two" "goodbye";
}
```

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

`-` is the first side and `+` the second. `-A X -A Y FILE` compares two
attributes of one file. You can also give two installables (a `.drv` path,
a flake reference or a Nix file), as long as each one evaluates to exactly
one derivation:

```sh
iets diff /nix/store/…-a.drv /nix/store/…-b.drv
```

Exit codes follow `diff(1)`: 0 when the two are equal, 1 when they differ,
2 for a usage error or any other failure.

Nothing is built and nothing is written to the store. A derivation this
evaluation made is read from memory; any other is read from `/nix/store`.

By default, iets looks two levels deep into input derivations. Use
`--depth N` for more, or `--all` for every level. Where it stops, it says
so, so nothing goes missing silently.

`--json` prints one object. Every difference is an entry of `changes`,
whatever part of the derivation it came from, so a program can filter on
one key:

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

`--closure` compares the packages and versions in the two output closures,
not the derivations:

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

Both outputs must be in the store, because iets asks the daemon about
them. If one is missing, iets stops with an error that names the missing
path.
`--builder URL` picks the store to read.

## `nurl`, `nix-prefetch`, `nix-prefetch-url`

`iets prefetch` prints the hash of something you have not fetched yet. It
prints one line on stdout and nothing else, so `$(iets prefetch …)` just
works.

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
`-A`, or a flake fragment in place of a URL:

```sh
iets prefetch --impure -E '(import <nixpkgs> {}).hello.src'
iets prefetch --impure -E 'with import <nixpkgs> {}; fetchzip {
    url = "https://github.com/numtide/treefmt/archive/v2.0.0.tar.gz";
    hash = ""; }'
iets prefetch default.nix -A mypkg.cargoDeps
```

iets ignores the hash in the expression, so `hash = ""`, `lib.fakeHash`
and last month's hash all work. Your files are not edited. iets sets the
derivation's output hash to all zeros, builds it, and reads the real hash
from the daemon's mismatch report. The fetcher runs exactly as it would
have.

There is no `-f FILE`. Give the file as an argument, as for `iets eval` and
`iets build`.

Unlike `nix store prefetch-file`, nothing is added to any store. That is
also why there is no `--name`, `--executable` or `--expected-hash`.

## `nix-fast-build`

You need no separate command. `iets build` takes a whole set of
derivations, so you can give it the checks of a flake:

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

It evaluates in this process and builds over one connection. It picks the
checks of the current system, gives every check its own result, and skips
what the builder already has. None of that needs a flag. The exit code is
Nix's: 100 when a build failed.

```sh
iets build --builder ssh-ng://builder .#checks
iets build release.nix -A ci --argstr system x86_64-linux
iets build --json .#checks > outcomes.json
```

`--json` gives you the report: `{"outcomes":[{attr, drv, success, outputs,
reason, logTail}]}`. There is no JUnit file; convert the JSON if you need
one. Outputs stay where they were built, and nothing is pushed to a binary
cache: that is the builder's job. See
[How to build on a remote builder](build-remotely.md) for more.

## `nix-eval-jobs`

```sh
iets eval-jobs --no-instantiate demo.nix
```

```json
{"attr":"a","attrPath":["a"],"constituents":[],"drvPath":"/nix/store/623k…-demo.drv","globConstituents":false,"name":"demo","namedConstituents":[],"outputs":{"out":"/nix/store/9ymw…-demo"},"storeDir":"/nix/store","system":"x86_64-linux"}
{"attr":"b","attrPath":["b"],…}
```

These are the JSON lines nix-eval-jobs 2.35 prints. The test suite runs
both tools with 41 sets of flags and compares the sorted output. All 41
match, exit codes included.

For Hydra and nixpkgs CI, the flags you already pass work:
`--force-recurse`, `--meta`, `--constituents`, `--check-cache-status`,
`--gc-roots-dir`, `--apply`, `--select`, `--flake`, `--max-memory-size`.

Three things to know before you swap it in:

- `--workers N` means worker *processes* here. The default is the CPU
  count, at most 4, not 1.
  With `--max-memory-size 0` it means fibers of one process instead.
- Line order is completion order, as it is for nix-eval-jobs. Sort by
  `attrPath` when you compare two runs.
- `--override-input`, `--override-flake` and `--repair` are errors. iets
  evaluates a flake from a complete lock file only. `--reference-lock-file`
  works: that file is the lock, and it must be complete too.

Two flags are iets's own. `--provenance` adds a `pos` key to each job,
with the file, line and column where the derivation was defined. It is off
by default, so the output stays the same as nix-eval-jobs. `--chunks M
--chunk N` splits a jobset, which is how nixpkgs CI shards its evaluation.

## nix-direnv

See [use-with-direnv.md](use-with-direnv.md).

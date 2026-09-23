# `ietsp diff`: what differs between two derivations

`ietsp diff A B` prints why two derivations are not the same, in place of
`nix-diff`. It compares the two derivations, then the input derivations
that differ, paired by name, down a bounded number of levels.

Nothing is built and nothing is written to the store. A derivation that an
evaluation produced is read from that evaluation's memory; every other
derivation is read from `/nix/store`.

## The two sides

```
ietsp diff [FLAGS] A B
ietsp diff [FLAGS] -A X -A Y (-E EXPR | FILE | FLAKEREF)
```

The first form takes two installables. Each one is a `.drv` store path, a
flake reference (`.#hello`, `nixpkgs#hello`) or a Nix file. Each must
evaluate to exactly one derivation.

The second form evaluates one expression, file or flake twice and selects a
different attribute each time. Use it when the two sides differ only by
attribute: `ietsp diff -A hello -A figlet '<nixpkgs>'`.

The common evaluation flags work as they do for `ietsp eval`: `-I`,
`--arg`, `--argstr`, `--option`, `--impure`, `--log-format`,
`--show-trace`. They apply to both sides.

| Flag | What it does |
| --- | --- |
| `--depth N` | walk N levels of input derivations (default 2) |
| `--all` | walk every level |
| `--json` | print one JSON object instead of text |

## Exit codes

| Code | Meaning |
| --- | --- |
| 0 | The two are equal. |
| 1 | They differ. |
| 2 | A usage error, or any other failure. |

This follows `diff(1)`: 1 means only "they differ", so 2 covers a bad
command line, a failed evaluation and a derivation that cannot be read.
Every error still goes through the reporter of docs/cli.md, with its code
and, under `--log-format internal-json`, its record.

## What it compares

At each pair of derivations:

- the derivation name, the platform (`system`), the builder and its args;
- the output names one side has and the other has not, and, for a
  fixed-output or content-addressed output, its hash or method. An
  input-addressed output path is not compared: it changes whenever
  anything else does, so it says nothing;
- the environment: the variables one side has and the other has not, and
  for a variable both sides set, its value. A value of several lines (a
  build script) is shown as a compact line diff, with the common head and
  tail left out;
- the structured attributes (`__json`), as one value;
- the input sources added and removed;
- the input derivations. The ones both sides share are skipped. The rest
  are paired by derivation name, so `hello` on one side pairs with `hello`
  on the other, and the pair is compared the same way, one level deeper.
  What is left over is added or removed. When the same input path is on
  both sides but the wanted outputs differ, the pair carries a `wanted`
  field.

The walk stops at `--depth` levels (default 2) and at a pair it has
already shown. Each stop prints the input's name and `(stopped: …; use
--all)`, so nothing goes missing silently.

## The text

```
$ ietsp diff -A a -A b ./demo.nix
- /nix/store/v6h4…-demo.drv
+ /nix/store/2c7p…-demo.drv
  input source removed: /nix/store/3kji…-one.txt
  input source added: /nix/store/n3da…-two.txt
  env GREETING:
    - hello
    + goodbye
  input drv changed: dep
    - /nix/store/qz5r…-dep.drv
    + /nix/store/8n1w…-dep.drv
      args:
        @@ line 2
        - "echo one > $out"
        + "echo two > $out"
```

`-` is the first side, `+` the second. The lines under a pair are its own
differences; an indented `- …`/`+ …` pair is an input derivation, with its
differences under it.

## The JSON

`--json` prints one object. Every difference is one entry of `changes`,
whatever part it is in, so a program filters on `what` and never has to
know a shape per part.

```json
{"a":"/nix/store/v6h4…-demo.drv","b":"/nix/store/2c7p…-demo.drv","equal":false,
 "changes":[{"what":"env","key":"GREETING","a":"hello","b":"goodbye"},
            {"what":"input source","key":"","a":"/nix/store/3kji…-one.txt","b":null},
            {"what":"input source","key":"","a":null,"b":"/nix/store/n3da…-two.txt"}],
 "inputDrvs":[{"a":"/nix/store/qz5r…-dep.drv","b":"/nix/store/8n1w…-dep.drv",
   "changes":[{"what":"args","key":"","a":"\"-c\"\n\"echo one > $out\"\n",
                                    "b":"\"-c\"\n\"echo two > $out\"\n"}]}]}
```

| Key | Value |
| --- | --- |
| `a`, `b` | the two derivation paths |
| `equal` | top level only: true when nothing differs |
| `changes` | every local difference, in order |
| `inputDrvs` | the input derivations that differ, each the same object again, one level deeper |
| `cut` | why the walk stopped here: `depth` or `shown above` |

An entry of `changes` is `{what, key, a, b}`:

| Key | Value |
| --- | --- |
| `what` | `name`, `platform`, `builder`, `args`, `structured attrs`, `output`, `env`, `input source`, `input drv` or `wanted` |
| `key` | the entry inside that part (the variable, the output name), or `""` |
| `a`, `b` | the value on each side; `null` is a side that has not got it |

`args` is one quoted argument per line, so a long command line diffs line
by line.


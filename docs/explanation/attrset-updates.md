# What `//` sees in a nixpkgs evaluation

This page records the shape of every attribute-set update in three
workloads. It answers one question: does the data support the union that
`//` builds (`evaluation.md`, "`//` builds a union, not a copy"), or does it
argue for a different representation?

## What was measured

The layer is the primitive `//` operator in the evaluator: `Ir::Update`,
which calls `Evaluator::update_attrs`. Each `//` that the evaluator runs is
one count, wherever it is written.

These are not counted, because they are builtins and not `//`: `mapAttrs`,
`listToAttrs`, `zipAttrsWith`, `intersectAttrs` and `removeAttrs`.
`lib.recursiveUpdate` and most of the module system's option merging are
built on those builtins. Thus, the counts below include only the `//` that
the module system writes itself, and they are less than its full merge
work.

The file of a `//` is the file where the operator is written. A `//` in
`lib/customisation.nix` counts there, also when a package caused it.

The counts come from `IETS_STATS=2` on a binary built with
`--features stats2` (`src/runtime/census.rs`). The census is per worker
arena, so every run uses one worker, and `eval-jobs` runs as one process
(`--max-memory-size 0`). Thus, each figure is for the whole run. The counts
are deterministic: two runs gave the same numbers.

| | |
|:---|:---|
| iets | commit 22d599d4 (the census code); the default build is the same code without `stats2` |
| nixpkgs | ef34387ddd751e1ab8857adf4676492d32eb24ec, the `nixpkgs` input of `flake.lock` |
| reference | Nix 2.34.8, `src/libexpr/eval.cc` and `src/libexpr/primops/derivation.nix` |

The three workloads, all at `--workers 1`:

| name | command |
|:---|:---|
| hello | `iets eval --impure -I nixpkgs=$NP -E '(import <nixpkgs> { system = "x86_64-linux"; }).hello.drvPath'` |
| desktop | `iets eval --impure --strict -I nixpkgs=$NP bench/workloads/nixos-desktop.nix` |
| outpaths | `iets eval-jobs --impure --no-instantiate --max-memory-size 0 --chunks 8 --chunk 0 -I nixpkgs=$NP outpaths.nix`, with the `outpaths.nix` of `bench/perf.nu`: 9,466 jobs |

`hello` is the evaluation that `nixpkgs#hello` does, through `<nixpkgs>`:
the nixpkgs tree has no `flake.lock`, and iets refuses a flake without one.

## How many, and what they cost

| | hello | desktop | outpaths |
|:---|---:|---:|---:|
| `//` evaluated | 9,840 | 421,216 | 7,948,342 |
| bytes `//` allocated | 8.4 MB | 53.8 MB | 1,108 MB |
| share of all arena bytes | 30 % | 8.5 % | 15.5 % |
| instructions, whole run | 0.55 G | 10.4 G | 184 G |
| `update_attrs` and `Arena::flat`, share of cycles | 0.62 % | 0.89 % | 0.81 % |
| `AttrsObj::get`, share of cycles | 2.6 % | 2.9 % | 3.8 % |
| `AttrIter::next`, share of cycles | 4.9 % | 2.2 % | 3.0 % |

The cost rows come from `bld1.ztm.io` (Ryzen 9 7950X3D), on the default
build pinned to one core, with `perf record -F 4999` and the self time per
symbol. `hello` is 20 runs in one profile. `alloc_union` is inlined into
`update_attrs`. `AttrsObj::get` and `AttrIter::next` serve every set, flat or
union; the section on lookups estimates the part that unions cause.

## Where they are written

| | hello | desktop | outpaths |
|:---|:---|:---|:---|
| 1 | derivation.nix (27 %) | `lib/modules.nix` (44 %) | `lib/customisation.nix` (25 %) |
| 2 | `lib/customisation.nix` (26 %) | `lib/options.nix` (13 %) | `make-derivation.nix` (22 %) |
| 3 | `make-derivation.nix` (12 %) | `lib/customisation.nix` (11 %) | derivation.nix (14 %) |
| 4 | `minimal-bootstrap/utils.nix` (10 %) | `make-derivation.nix` (9.5 %) | haskell `generic-builder.nix` (14 %) |
| module system | 2.9 % | 57 % | 3.4 % |

derivation.nix is the `derivation` wrapper, the same file as Nix's
`src/libexpr/primops/derivation.nix`. "Module system" is `lib/modules.nix`,
`lib/options.nix` and `lib/types.nix` together. On desktop, the module system
writes more than half of all `//`. In the package set, `//` is mostly
package construction: `makeOverridable`, `mkDerivation` and the
`derivation` wrapper.

## Operand and result sizes

Counts of updates by the entry count of each side. "Overwritten" is the
right-hand keys that the left side already has; "added" is the rest of the
right side.

| outpaths | 0 | 1 | 2 | 3-4 | 5-8 | 9-16 | 17-32 | 33-64 | 65-128 | 129-256 | 257+ |
|:---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| left | 1445784 | 707756 | 760966 | 1028688 | 799158 | 481501 | 776572 | 1783782 | 150811 | 1234 | 12090 |
| right | 1900500 | 861616 | 1658870 | 1453290 | 691338 | 643231 | 721177 | 10553 | 1159 | 393 | 6215 |
| result | 1048750 | 325238 | 420788 | 793895 | 1010500 | 1016639 | 991250 | 2100683 | 223169 | 1465 | 15965 |
| overwritten | 6212302 | 564075 | 180911 | 549172 | 412787 | 23818 | 2402 | 715 | 446 | 6 | 1708 |
| added | 2237189 | 893941 | 1756920 | 1275360 | 597857 | 587672 | 584598 | 8482 | 642 | 404 | 5277 |

| desktop | 0 | 1 | 2 | 3-4 | 5-8 | 9-16 | 17-32 | 33-64 | 65-128 | 129-256 | 257+ |
|:---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| left | 34961 | 17071 | 48335 | 153121 | 67230 | 36820 | 18418 | 40610 | 3943 | 94 | 613 |
| right | 88295 | 107526 | 44942 | 102678 | 47152 | 12682 | 16722 | 580 | 132 | 100 | 407 |
| result | 25856 | 8420 | 28655 | 128535 | 74150 | 62850 | 37987 | 47980 | 5777 | 147 | 859 |
| overwritten | 298016 | 53177 | 15435 | 43124 | 10438 | 855 | 85 | 12 | 7 | 0 | 67 |
| added | 168740 | 79656 | 41530 | 61655 | 43361 | 11331 | 13825 | 513 | 122 | 100 | 383 |

| hello | 0 | 1 | 2 | 3-4 | 5-8 | 9-16 | 17-32 | 33-64 | 65-128 | 129-256 | 257+ |
|:---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| left | 476 | 1012 | 1544 | 1106 | 930 | 2189 | 646 | 1330 | 381 | 74 | 152 |
| right | 871 | 910 | 2097 | 2880 | 1525 | 545 | 572 | 130 | 112 | 86 | 112 |
| result | 32 | 163 | 439 | 1656 | 1679 | 2133 | 1318 | 1520 | 534 | 125 | 241 |
| overwritten | 7924 | 686 | 168 | 553 | 367 | 138 | 4 | 0 | 0 | 0 | 0 |
| added | 1119 | 1051 | 2460 | 2802 | 1033 | 436 | 523 | 106 | 112 | 86 | 112 |

The same data as shares:

| | hello | desktop | outpaths |
|:---|---:|---:|---:|
| right side of 4 entries or fewer | 69 % | 82 % | 74 % |
| left side of 9 entries or more | 48 % | 24 % | 40 % |
| left side of 33 entries or more | 20 % | 11 % | 25 % |
| one side empty (no work) | 13 % | 23 % | 29 % |
| no right-hand key overwrites | 81 % | 71 % | 78 % |
| every right-hand key overwrites | 2.5 % | 19 % | 4.2 % |

## The branch `update_attrs` takes

| | hello | desktop | outpaths |
|:---|---:|---:|---:|
| empty side, an operand returned | 1315 | 97400 | 2297534 |
| flat copy, left side under 8 entries | 3715 | 212968 | 2579620 |
| union | 4736 | 109556 | 3013404 |
| left side flattened, then union | 57 | 1265 | 57473 |
| flat copy, more than 8 leaves | 17 | 27 | 311 |

Nix 2.34.8 makes the same choice in another order. It layers the right side
over the left when the right side has 16 entries or fewer and the left has
fewer than 8 layers, and copies both sides otherwise (`ExprOpUpdate::eval`
in `src/libexpr/eval.cc`; `bindingsUpdateLayerRhsSizeThreshold`, and
`Bindings::maxLayers` in `attr-set.hh`).

## How results are used

Each new result is followed to the end of the run. A use is a lookup, an
iteration, or a turn as an operand of a later `//`. The census does not
count its own reads, nor the flatten that a later `//` does on an operand.

| | hello | desktop | outpaths |
|:---|---:|---:|---:|
| results | 8525 | 323816 | 5650808 |
| never used | 11 | 35 | 6259 |
| once: one lookup | 262 | 11757 | 240878 |
| once: one iteration | 115 | 1057 | 78683 |
| once: left operand of a `//` | 1293 | 18243 | 718131 |
| once: right operand of a `//` | 2753 | 62523 | 1397979 |
| 2 or 3 uses | 1778 | 134412 | 1565103 |
| 4 uses or more | 2313 | 95789 | 1643775 |

Thus, 52 %, 29 % and 43 % of results are used once and then dropped. Most of
these are operands of one more `//`.

`//` is right-associative (`%right UPDATE` in Nix's `parser.y`), so
`a // b // c` is `a // (b // c)`. Of all updates, 26 %, 29 % and 34 % have a
right operand that is written as a `//` expression. The derivation wrapper
is one example: `drvAttrs // (listToAttrs outputsList) // { ... }`. iets
makes the inner result and then merges it again. Nix 2.34.8 does not make it:
`ExprOpUpdate::evalForUpdate` puts every operand of the chain in one queue,
and `ExprOpUpdate::eval` merges the queue from the left.

Left-nested chains, `((a // b) // c) // d`, are short: 54 % of results in
outpaths end a chain of one, and 13 % end a chain of five or more.

## Lookups on unions

| | hello | desktop | outpaths |
|:---|---:|---:|---:|
| lookups, all sets | 123523 | 6097936 | 81891170 |
| on a flat set | 52307 | 4906495 | 42425211 |
| on a union, through its cached flat copy | 77 | 87668 | 643102 |
| on a union, over its leaves | 71139 | 1103773 | 38822857 |
| of those, misses | 46884 | 539922 | 19543681 |
| leaves probed on unions | 288554 | 3085676 | 112064273 |
| leaves per union lookup | 4.1 | 2.8 | 2.9 |
| union leaf searches, share of all set searches | 85 % | 38 % | 72 % |

In outpaths, 47 % of all lookups go to a union, and half of those miss. A
miss probes every leaf. The misses are spread over many names. The most
frequent are `__toString` (string coercion), then `outputSpecified`,
`__spliced`, `dev` and the `or` defaults of `make-derivation.nix`. Thus, a
fix for one name does not help.

If each leaf search costs the same as a flat search, unions cause about
72 % of `AttrsObj::get`: 2.7 % of the outpaths cycles, 1.1 % on desktop and
2.2 % on hello. This is an estimate: a leaf of 8 entries or fewer is a
linear scan, and a larger leaf is a binary search.

## What the data argues for

The data supports the union. The right side is small (74 % of outpaths
updates have 4 entries or fewer), 78 % of updates add keys and overwrite
none, and a quarter of left sides have 33 entries or more. A copy on each
update would copy those left sides; the union does not, and `//` itself is
now below 1 % of cycles. The data argues against a persistent tree such as a
HAMT: lookups are 10 to 15 times more frequent than updates, and a tree makes
each lookup a chain of pointer reads. It argues against in-place update of a
left side that has one owner, because only 13 % of results are used once as
a left operand. It argues against more than 8 leaves, because the cost has
moved to the read side: unions take 47 % of lookups and half of those miss
on every leaf. The one change worth a prototype is a membership filter per
leaf: a short bit mask of the symbols in a flat set, tested before each leaf
search of a union lookup. This is the short-tag filter of SwissTable, put on
sorted sets. Measure it against outpaths chunk 0/8 and nixos-desktop at one
worker on bld1: instructions, wall time and arena bytes, with the leaves
probed from this census as the second counter. Its ceiling is the 2.7 %
above. The second candidate is a fold of the `a // b // c` chain from one
queue, as Nix does; it removes the inner results, but its ceiling is a part
of the 0.8 % that `//` costs.

## Cost of the census

The `stats2` hooks are behind `cfg(feature = "stats2")`, so the default
build has none of them. On a `stats2` build with `IETS_STATS=2`, the census
code from origin/main costs 21 % more instructions on desktop (10.4 G to
12.7 G) and 20 % on outpaths (184 G to 221 G). The update and lookup
counters on this page add 6.7 % (to 13.5 G) and 8.3 % (to 239 G) to that.
These figures come from bld1, as the second and third of three runs; the
first run of each binary filled the caches.

# What iets lints, and what it does not

Why some lints are in iets and others never will be, and how you can write
your own rules with the data iets gives you.

## The rule

> iets reports what only an evaluator can know. It has no opinion about
> how code looks.

A lint belongs in the registry when both of these hold:

1. It needs evaluation to decide. Its stage in `iets lint list` is
   `evaluator`.
2. It tells you about what the code does (correctness, performance or
   purity), not about how the code reads.

Take `eq-type-mismatch`. `x == "a"` is fine syntax. The comparison is
always false only when `x` is a number, and only the evaluator knows the
value of `x`. Or take `git-in-source`: only the evaluator knows which
directory a coercion copies to the store.

The full list is in `docs/reference/lints.md`.

## Why

Style is the project's call, not the tool's. nixpkgs has its own style, and
it changes with the kind of file and the path. A lint about `with`, an
unused binding or `a = a;` asks you to change code that is correct. iets
should not do that.

Tools that read only the syntax already exist: statix and deadnix. Copying
their rules into iets makes the binary bigger and tells you nothing new.
iets wants to make the Nix stack smaller, with good abstractions and good
defaults. One big binary that does everything is the opposite.

## The lints the rule does not explain

Four lints do not need evaluation. They stay for other reasons.

| Lint | Reason |
|---|---|
| `absolute-path-literal`, `short-path-literal`, `url-literal` | Nix 2.34 has a setting for each (`lint-absolute-path-literals`, `lint-short-path-literals`, `lint-url-literals`). They are Nix's behaviour, not iets's opinion. |
| `unknown-allow` | It checks that a `# iets: allow` comment names a lint in the registry. Without it, a typo silences nothing and tells nobody. |

`impure-builtin` comes in two parts. The evaluator reports a `getEnv`
call. The compiler reports a reference to `currentTime`, because that
value sits in the table of globals and has no position of its own.

## Where a finding is reported

A lint finds a site, and the site's location tells iets whether the
finding is yours. A site in your repository is reported. A site in an
input stays quiet. `docs/reference/lints.md` ("Scope") has the rule.

Two lints break it: `tree-meta-in-drv` and `rev-count`.

`tree-meta-in-drv` breaks it on purpose. It reports source tree metadata
(`self.rev`, `self.lastModifiedDate`) that ends up in a derivation. That
value changes with every commit, so every commit rebuilds the derivation
and everything that depends on it.

Here the site's location does not say whose finding it is. Two examples
show why:

- You set `system.configurationRevision = self.rev`. nixpkgs writes the
  value into the `nixos-version` derivation. The site is in nixpkgs, but
  the value is yours, and your commits cause the rebuild.
- nixpkgs writes its own `lastModifiedDate` and `shortRev` into the NixOS
  version. That value changes only when you update the lock file, and then
  a rebuild is right.

So the tree the value comes from sets the level. The root tree gets the
lint's level, `warn` by default. A locked input stays quiet unless
`[lint.inputs.<node>]` gives it a level.

`rev-count` splits the same way, for the same reason. It reports a read of
`revCount`, which is deprecated. The root tree's count changes with every
commit, and iets has to compute it. A locked input's count is a number in
the lock file. iets knows the tree because `revCount` is a lazy value that
carries its tree's name.

A `rev-count` finding points at the selection that reads the attribute,
in your repository or in an input. The compiler marks every selection
named `revCount`, and the evaluator then looks at the value it reads. A
read that is not a selection has no site, so the finding names only the
tree. `docs/reference/lints.md` has the list ("The tree that a value comes
from").

iets knows the tree because each metadata string carries a mark. The mark
is a string context element that Nix does not have. It travels with the
string through `+`, interpolation, `substring` and `concatStringsSep`, just
like a store path reference. Nix never sees it: `getContext`,
`hasContext`, `==`, `toJSON` and the `.drv` files stay the same.

## Your own rules

iets gives you the data, and a rule is a program you write that reads it.
`iets parse --json` prints the syntax tree with the byte span of every
node, and iets's own findings carry the same spans. iets never applies an
edit itself. The program that reads the findings decides.
`docs/how-to/write-lint-rules.md` shows how.

## What iets does not have

- A language for rules.
- A plug-in loader.
- A syntax tree that keeps every byte of the file. Comments and
  parentheses are not in the tree.
- A command that applies the edits of the findings.

## A name that is not in the registry

Your `iets.toml` can name a lint that iets does not have, for example after
you update iets. iets warns once for each such name, with the code
`unknown-lint`, and carries on. `--lint` on the command line gives an
error instead. `docs/reference/lints.md` says why.

## What the lints find on nixpkgs

The byte-exact gates (`scripts/difftest.nu` on nixos-desktop,
`scripts/langtests.nu`, `scripts/corpus.nu`, `scripts/evaljobs-flags.nu`)
compare stdout, and a lint at `off` or `warn` never changes stdout. The
defaults follow a stricter rule: a lint that fires on nixpkgs or on the
language tests defaults to `off`; only lints that fire nowhere may default
to `warn`. `url-literal`, `absolute-path-literal` and `short-path-literal`
keep Nix's default (`off`).

Warnings printed per lint with `--lint all=warn --lint-scope all`
(the default scope reports nothing from a store tree), pinned nixpkgs
(`/nix/store/ah3pxz4x…-source`), release build, 2026-09-19. Desktop is
`bench/workloads/nixos-desktop.nix` at `--workers 1`; outpaths is
`eval-jobs --no-instantiate --workers 4 --max-memory-size 0 --chunks 8
--chunk 0` of `ci/eval/outpaths.nix` (9,411 lines, 167 errors like
without lints; with worker processes each process deduplicates on its
own, so the counts multiply); lang is every `tests/lang/eval-*.nix` and
`tests/corpus/*.nix`. Stdout was identical with and without lints, and
at 1 and 4 workers. `eq-type-mismatch` (null exempt), `drv-bool-null`
(Nix's Boolean attributes exempt) and `drv-list` were counted again on
the build with those exemptions (outpaths then had 168 errors);
`--lint-scope all` does not apply the `derivation`-call rule, and
`dir-copy-flake` needs a flake entry, which none of these workloads is.

| lint | desktop | outpaths 0/8 | lang + corpus |
|---|---:|---:|---:|
| `path-copy` | 133 | 381 | 12 |
| `dir-copy-flake` | 0 | 0 | 0 |
| `store-path-recopy` | 142 | 395 | 0 |
| `read-through-copy` | 0 | 580 | 0 |
| `drv-bool-null` | 195 | 1515 | 0 |
| `drv-list` | 533 | 9767 | 0 |
| `eq-type-mismatch` | 26 | 30 | 0 |
| `eq-function` | 4 | 5 | 3 |
| `or-non-attrs` | 25 | 42 | 2 |
| `url-literal` | 0 | 0 | 3 |
| `impure-builtin` | 3 | 7 | 5 |
| `unhashed-fetch` | 0 | 0 | 5 |
| `ifd` | 0 | 0 | 0 |
| `fod` | 4 | 4 | 6 |
| `large-source-copy` | 1 | 2 | 0 |
| `git-in-source` | 0 | 0 | 0 |
| `dir-copy-unfiltered` | 0 | 0 | 3 |
| `impure-path-in-drv` | 0 | 0 | 0 |
| `absolute-path-in-drv` | 0 | 0 | 0 |
| `tree-meta-in-drv` | 0 | 0 | 0 |
| `rev-count` | 0 | 0 | 0 |
| `absolute-path-literal` | 2 | 9 | 26 |
| `short-path-literal` | 0 | 1 | 6 |
| `unknown-allow` | 0 | 0 | 0 |

The pinned nixpkgs lives in the store, so every copy of a nixpkgs path is
a `store-path-recopy` and never a `dir-copy-unfiltered`; the latter needs
a checkout outside the store. `ifd` never fires on these workloads
because iets cannot build (an IFD is an `Unsupported` error), `git-in-
source` because a git flake root is a filtered tree.

`impure-path-in-drv` and `absolute-path-in-drv` are zero above for the
same reason: the text of a store path is not a checkout path. Measured
on a plain copy of the pinned tree under `~/tmp` (`cp -r`, so no `.git`),
with `-I nixpkgs=<copy> --lint-scope all --lint impure-path-in-drv=warn
--lint absolute-path-in-drv=warn`, same workloads, 2026-09-19:

| lint | desktop | outpaths 0/8 |
|---|---:|---:|
| `impure-path-in-drv` | 0 | 0 |
| `absolute-path-in-drv` | 3 | 3 |

`tree-meta-in-drv` and `rev-count` are zero above because none of these
workloads is a flake: no tree has a name, so iets makes no mark and no
lazy count. The row of `rev-count` is not measured; it follows from that.
The pinned nixpkgs reads `revCount` of no flake tree. The count for the
outpaths column is not measured; it follows from that. Measured on a
scratch git flake with the pinned tree as a `path:` input, the `minimal`
profile of NixOS and `system.configurationRevision = self.rev`,
2026-09-27:

| configuration | findings | derivations that hold the value |
|---|---:|---|
| default | 1 | `nixos-version` (`self.rev`) |
| `[lint.inputs.nixpkgs] tree-meta-in-drv = "warn"` | 3 | `nixos-version`, `etc-os-release`, `nixos-system-scratch-…` (`inputs.nixpkgs.lastModifiedDate`) |

The copy holds nixpkgs's `flake.nix`, so every path under it is under a
flake root and the sites are all `absolute-path-in-drv`; the three, on
both workloads:

- `pkgs/tools/package-management/nix/modular/packaging/components.nix:97:7`
- `pkgs/build-support/trivial-builders/default.nix:84:17`
- `nixos/modules/misc/documentation.nix:139:11`

## What the lints cost

At the defaults the lints cost nothing measurable: `bench/ab.nu` on the
commit before the registry against this one gave −3.4 % (desktop, 1
worker), +2.3 % (desktop, 4 workers) and −0.9 % (minimal), inside the
run-to-run noise of that box. An `off` lint is one byte compare at the
site.

The numbers below are from 2026-09-19. The registry then held 18 more
lints, which the parser and the compiler reported
(they left under "The rule" above). The cost of the present registry is
not higher.

`IETS_LINT=all=warn` against the defaults, same box (16-core EPYC with
other builds running), `bench/ab.nu --runs 9 --skip-outpaths`, IR and
tracing caches as the benchmark uses them (`--no-tracing-cache` keeps the IR
cache):

| workload | default | all=warn | change |
|---|---|---|---|
| nixos-desktop, 1 worker | 1.686 s | 1.667 s | −1.1 % |
| nixos-desktop, 4 workers | 1.302 s | 1.383 s | +6 % (σ 0.14 s) |
| nixos-minimal, 1 worker | 1.209 s | 1.234 s | +2.1 % |

The 4-worker row is a four-way hyperfine run: `all=off` 1.302 s, the
compile-time families only 1.342 s, the evaluator families only 1.250 s,
`all=warn` 1.383 s; the spread between runs of one configuration is as
large as the differences. What kept the warn path cheap, in the order it
was measured:

- compile-time warnings travel in the IR cache entry (a bypass of the IR
  cache cost 9 %);
- a helper fiber reports a site itself instead of bailing like `trace`
  (bails cost 15 % at 4 workers: every import that compiled in a helper
  was compiled again on the demand path);
- each fiber keeps its own set of sites it saw reported, so a site that
  fires a million times takes the shared lock once per fiber;
- warnings are buffered and written in 16 KiB chunks (one syscall per
  line cost 7 % at 4 workers);
- the position of a site inside a store copy is resolved through the
  source table directly; rebuilding `provenance::SourceMap` per warning
  cost 10 %, and `provenance::describe_pos` cannot be used because it
  freezes its map on first use and an error report needs the map of the
  whole run.

The `# iets: allow` table costs the parse nothing measurable: `iets
parse -q` over the 44,303 files of the pinned nixpkgs (hyperfine, 30
runs, 2026-09-19) took 1.128 s ± 0.034 before and 1.122 s ± 0.025 after;
the instruction count (`perf stat`) rose 0.5 %, from 6.728 G to 6.763 G.
The `#` arm of the lexer's trivia loop is a function of its own: inlined,
it grew `next_token` past what its callers inline, which cost 1.5 % of
the instructions. At a site the check is one branch on an empty table.

`tree-meta-in-drv` is `warn` by default. Thus `derivationStrict` examines
the context of each attribute for a mark in each run, and the context
element has one more level in its type. Measured with `perf stat -e
instructions:u` on nixos-desktop (one worker, `--no-tracing-cache`, a
warm compiled-file cache, 7 runs for each binary, 2026-09-27): 9.3915 G
instructions before and 9.3942 G after, which is +0.03 %. Two runs of
one binary are up to 0.02 % apart. nixos-desktop is not a flake, so it
has no mark and the number is the cost of the test alone.

`rev-count` adds nothing to an evaluation that has no source tree.
Measured in the same way (5 runs for each binary, 2026-09-28): 9.3940 G
instructions before and 9.3941 G after, which is +0.001 %. Two runs of
one binary were up to 0.03 % apart.

The site of `rev-count` costs nothing on that workload. The compiler
gives a selection that has the name `revCount` an operation of its own,
so no other selection tests the name. The pinned nixpkgs has 5 such
selections, and none is in a file that nixos-desktop reads. Measured in
the same way (7 runs for each binary, 2026-09-28): 9.3944 G instructions
before and 9.3930 G after, which is -0.015 %. With an empty compiled-file
cache (3 runs for each binary, each file compiles): 12.1825 G before and
12.1842 G after, which is +0.014 %. Two runs of one binary were up to
0.03 % apart.


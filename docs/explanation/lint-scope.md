# What iets lints

iets warns you that `x == "a"` can never be true when `x` is a number. It
won't tell you that a `let` binding is unused, or that you should have
written `inherit`. This page explains where that line is and why, and how to
write the style rules you want yourself. The lints themselves are in
[Lints](../reference/lints.md).

## No style opinions

> iets reports what only an evaluator can know. It has no opinion about
> how code looks.

A lint belongs in the registry when it needs evaluation to decide (its
stage in `iets lint list` is `evaluator`) and tells you what the code does,
in correctness, performance or purity.

`x == "a"` is fine syntax. The comparison is always false only when `x` is a
number, and only the evaluator knows the value of `x`. Likewise only the
evaluator knows which directory a coercion copies to the store, for
`git-in-source`.

## Why not style lints?

Style is the project's call. nixpkgs has its own style, and it changes with
the kind of file and the path. A lint about `with`, an unused binding or
`a = a;` asks you to change code that is correct.

Tools that read only the syntax already exist: statix and deadnix. Copying
their rules into iets makes the binary bigger and tells you nothing new.

## Lints that don't need evaluation

A few lints don't need evaluation. Each stays for its own reason.

| Lint | Reason |
|---|---|
| `absolute-path-literal`, `short-path-literal`, `url-literal` | Nix 2.34 has a setting for each (`lint-absolute-path-literals`, `lint-short-path-literals`, `lint-url-literals`). iets follows Nix here. |
| `unknown-allow` | It checks that a `# iets: allow` comment names a lint in the registry. Without it, a typo silences nothing and tells nobody. |

## Where a finding is reported

A site in your repository is reported, and a site in an input stays quiet
([Lints](../reference/lints.md#scope), under "Scope").

`tree-meta-in-drv` and `rev-count` break it, on purpose.
`tree-meta-in-drv` reports source tree metadata (`self.rev`,
`self.lastModifiedDate`) that ends up in a derivation. That value changes
with every commit, so every commit rebuilds the derivation and everything
that depends on it.

Consider these cases:

- You set `system.configurationRevision = self.rev`. nixpkgs writes the
  value into the `nixos-version` derivation. The site is in nixpkgs, but
  the value is yours, and your commits cause the rebuild.
- nixpkgs writes its own `lastModifiedDate` and `shortRev` into the NixOS
  version. That value changes only when you update the lock file, and then
  a rebuild is right.

So the tree the value comes from sets the level. The root tree gets the
lint's level, `warn` by default. A locked input stays quiet unless
`[lint.inputs.<node>]` gives it a level.

`rev-count` splits the same way: the root tree's count changes with every
commit, and a locked input's count is a number in the lock file
([The tree that a value comes from](../reference/lints.md#the-tree-that-a-value-comes-from)).

iets knows the tree because each metadata string carries a mark, a string
context element that Nix doesn't have. It travels with the string through
`+`, interpolation, `substring` and `concatStringsSep`, like a store path
reference. Nix never sees it: `getContext`, `hasContext`, `==`, `toJSON` and
the `.drv` files stay the same.

## Your own rules

A rule is a program you write. `iets parse --json` prints the syntax tree
with the byte span of every node, and iets's own findings carry the same
spans. iets never applies an edit itself
([Write your own lint rules](../how-to/write-lint-rules.md)).

## What iets leaves out

- A language for rules.
- A plug-in loader.
- A syntax tree that keeps every byte of the file. Comments and
  parentheses are not in the tree.
- A command that applies the edits of the findings.

## Unknown lint names

Your `iets.toml` can name a lint that iets doesn't have, for example after
you downgrade iets. iets warns once for each such name, with the code
`unknown-lint`, and carries on. `--lint` on the command line gives an error
instead. [Lints](../reference/lints.md) says why.

## What the lints find on nixpkgs

A lint that fires on nixpkgs or on the language tests defaults to `off`, and
only a lint that fires nowhere may default to `warn`. `dir-copy-unfiltered`
is the exception. It fires on three language tests and is `warn`, because
outside a flake nothing else stops `"${./.}"` from taking your untracked
files with it.

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
because iets cannot build (an IFD is an `Unsupported` error), `git-in-source`
because a git flake root is a filtered tree.

`impure-path-in-drv` and `absolute-path-in-drv` are zero in the first table for the
same reason: the text of a store path is not a checkout path. Measured
on a plain copy of the pinned tree under `~/tmp` (`cp -r`, so no `.git`),
with `-I nixpkgs=<copy> --lint-scope all --lint impure-path-in-drv=warn
--lint absolute-path-in-drv=warn`, same workloads, 2026-09-19:

| lint | desktop | outpaths 0/8 |
|---|---:|---:|
| `impure-path-in-drv` | 0 | 0 |
| `absolute-path-in-drv` | 3 | 3 |

`tree-meta-in-drv` and `rev-count` are zero in the first table because none of these
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

The following numbers are from 2026-09-19, when the registry held 18 more
lints that the parser and the compiler reported
([What was tried and removed](history.md)). The cost of the current registry
is not higher.

`IETS_LINT=all=warn` against the defaults, same box, `bench/ab.nu --runs 9
--skip-outpaths`:

| workload | default | all=warn | change |
|---|---|---|---|
| nixos-desktop, 1 worker | 1.686 s | 1.667 s | −1.1 % |
| nixos-desktop, 4 workers | 1.302 s | 1.383 s | +6 % (σ 0.14 s) |
| nixos-minimal, 1 worker | 1.209 s | 1.234 s | +2.1 % |

The spread between runs of one configuration is as large as the
differences.

The `# iets: allow` table costs the parse nothing measurable: `iets
parse -q` over the 44,303 files of the pinned nixpkgs (hyperfine, 30
runs, 2026-09-19) took 1.128 s ± 0.034 before and 1.122 s ± 0.025 after;
the instruction count (`perf stat`) rose 0.5 %, from 6.728 G to 6.763 G.
The `#` arm of the lexer's trivia loop is a function of its own: inlined,
it grew `next_token` past what its callers inline, which cost 1.5 % of
the instructions. At a site the check is one branch on an empty table.

`tree-meta-in-drv` and `rev-count` cost less than 0.05 % of the
instructions of nixos-desktop (`perf stat -e instructions:u`, 5 to 7 runs for
each binary, 2026-09-27 and 2026-09-28), which is inside the spread of two
runs of one binary. That workload isn't a flake, so the number is the cost of
the test alone.

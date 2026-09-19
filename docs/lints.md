# Lints

ietsp has a registry of named lints for language corner cases. Each lint
is `off`, `warn` or `error`. Code: `src/lint/` (`mod.rs` holds the
registry and the emission, one file per family holds the triggers);
fixtures: `tests/lint/<name>.nix`, checked by `scripts/linttest.nu`.

## Levels and resolution

| level | effect |
|---|---|
| `off` | nothing |
| `warn` | one line per (lint, position) site on stderr, `warning: [name] message` then `  at file:line:col`; deduplicated for the run |
| `error` | an evaluation error of kind `Lint`, which `tryEval` does not catch |

Nix's spellings `ignore` and `fatal` are accepted for `off` and `error`.

The level of a lint resolves in this order, later wins:

1. the built-in default (the table below);
2. the `[lint]` table of the nearest `ietsp.toml`, found by walking up
   from the entry file's directory, the flake directory, or cwd for `-E`;
3. `IETSP_LINT="name=level,name=level"`;
4. `--lint SELECTOR=LEVEL`, repeatable, in order.

A selector is a lint name, `all`, `family:NAME` (`path`, `scope`,
`coerce`, `legacy`, `purity`, `style`), or one of Nix's setting names
(`lint-absolute-path-literals`, `lint-short-path-literals`,
`lint-url-literals`). The old flags `--lint-absolute-path-literals MODE`,
`--lint-short-path-literals MODE` and `--lint-url-literals MODE` still
work: each is `--lint <nix name>=MODE`.

`ietsp lint list [--lint ...] [--lint-root DIR]... [--lint-scope root|all]
[PATH]` prints the registry with the levels a run from `PATH` (default
cwd) gets, then the scope and its roots.

## Scope

A level says whether a lint runs; the scope says where its sites are
reported. The default scope is `root`: a site is reported only when it
lies inside a lint root. The first root is the entry's directory: the
flake directory when the entry is a flake reference (its tracked tree,
which the evaluation also serves as a virtual copy under `/nix/store`),
else the directory of the entry file, or cwd for `-E` (the expression
itself is in root). Everything else under `/nix/store` is silent: the
locked inputs, nixpkgs, corepkgs, `«derivation-internal»`. So `--lint
all=warn` on a NixOS configuration reports the sites in that repository
and nothing from its inputs.

- `--lint-root DIR` (repeatable) adds a root; `roots = ["dir", ...]` in
  `ietsp.toml` does the same, relative to the file.
- `--lint-scope all`, or `scope = "all"`, reports every site.
- `[lint.inputs.<node>]`, with the same `name = level` keys (selectors
  included), gives the tree of that locked input its own levels; `<node>`
  is the lock file's node name. A lint not named there follows the scope.

A site's file is "in root" when its path, resolved through the virtual
copies to the local tree it stands for, lies under a root directory. So
`/nix/store/<hash>-source/modules/x.nix` of the root flake is in root, a
`path:` input checked out beside the repository is not, and one inside
the flake directory is (containment wins). A relative `path:` input lives
under its parent's tree and follows it.

The scope resolves with the levels (default, `ietsp.toml`, `IETSP_LINT`,
`--lint`); `--lint-scope` and `--lint-root` come last. It is part of
both cache keys: a file is compiled under the levels its location gets,
so a store file under `scope = "root"` keys the IR cache like an unlinted
one and its entry holds no warnings, and the tracing-cache key holds the
scope, the roots and the input tables, because another scope is another
warning set.

For a compile-time lint the site is the node's position; for a runtime
lint it is the call's position. The derivation-attribute lints are the
exception: nixpkgs's `mkDerivation` and `fetchurl` bind the attributes
of every derivation they make, so the attribute's position is in nixpkgs
even when the value came from the user's file. Two rules:

- The value lints (`impure-path-in-drv`, `absolute-path-in-drv`, `fod`)
  report the user's value whatever the wrapper. When the attribute's
  position is out of scope the site falls back to the derivation's own
  position: the `derivationStrict` call when it is in a root (a direct
  call), else the `name` attribute, else the first attribute of the set
  (in the set's order) whose position is in a root, which `mkDerivation`
  keeps for the attributes it passes through (`pname`, `src`, ...). One
  site per (lint, position). The evaluator keeps no call trace: a
  derivation whose attribute set holds no position from a root, such as
  a nixpkgs fetcher call (`fetchurl { url; hash }`), stays silent under
  `scope = "root"`; `[lint.inputs.nixpkgs] fod = "warn"` reports those
  at nixpkgs's site.
- The encoding lints (`drv-bool-null`, `drv-list`) report a `derivation`
  call the scope sees, and nothing else: a list or Boolean passed to
  `mkDerivation` is nixpkgs's contract, and the `__structuredAttrs =
  false` it injects is nixpkgs's binding. `derivation` is a wrapper in
  `«derivation-internal»`, so the call is known by the `derivationStrict`
  call position when that is in scope (a direct `builtins.derivationStrict`
  call), else by the position of the `builder` attribute, which every
  wrapper binds itself. The site is the attribute's own position when
  the scope sees it, else the position the call was known by.

## Configuration file

```toml
# ietsp.toml, anywhere above the file being evaluated
[lint]
with = "warn"
unused-binding = "error"
large-source-copy = { level = "warn", max-bytes = 1048576 }
scope = "root"          # or "all"
roots = ["../shared"]   # relative to this file

[lint.inputs.nixpkgs]   # levels inside that locked input's tree
fod = "warn"
```

A value is a level string, or a table with `level` and the lint's knobs.
The only knob is `large-source-copy.max-bytes` (default 8 MiB).

A strict repository sets everything to `error` and opens what it needs:

```toml
[lint]
all = "error"
# the two lints nixpkgs' own idioms make unavoidable in a NixOS config
with = "off"
ellipsis-without-bind = "off"
# a tracked directory copied whole is worth a look, not a failed run
dir-copy-flake = "warn"
```

## Catalog

Hook: `parser` and `compiler` lints run once per file, so their levels
are part of the IR cache key and their warnings are stored in the entry;
`evaluator` lints fire when the value is computed.

| name | family | hook | default | trigger |
|---|---|---|---|---|
| `absolute-path-literal` | path | compiler | off | a path literal that starts with `/` or `~/` (Nix's `lint-absolute-path-literals`) |
| `short-path-literal` | path | compiler | off | a relative path literal without a leading `./` (Nix's `lint-short-path-literals`) |
| `url-literal` | legacy | parser | off | an unquoted URL (Nix's `lint-url-literals`) |
| `with` | scope | compiler | off | any `with` |
| `with-non-literal` | scope | compiler | off | `with e;` where `e` is neither an attrset literal nor a plain variable |
| `with-nested` | scope | compiler | off | `with` inside the body of another `with` |
| `shadow-builtin` | scope | compiler | off | a let, rec, formal or argument binding named like a global (`map`, `__add`, `builtins`, `true`, ...) |
| `shadow-binding` | scope | compiler | off | a binding that hides an outer lexical binding of the same name; `_`-prefixed names are exempt, `x: let x = ...` is not |
| `unused-binding` | scope | compiler | off | a `let` binding never referenced in its scope (`_`-prefixed exempt; a self-reference counts) |
| `unused-formal` | scope | compiler | off | a formal never referenced in the body or a default, unless the `@name` binding is referenced; no `_` exemption, a formal is the caller's contract and `{ _pkgs, ... }:` still demands the attribute, so remove it from the pattern |
| `rec-unused` | scope | compiler | off | `rec { }` where no binding references a sibling (`__overrides` counts as a reference) |
| `path-copy` | coerce | evaluator | off | a path coerced to a string with a copy to the store: interpolation, `+`, a derivation attribute; not `toString`. Under a flake root or a locked input `"${./file}"` is the normal reference: silent for a regular file, and a directory is `dir-copy-flake` |
| `dir-copy-flake` | coerce | evaluator | off | a directory under a flake root or a locked input coerced to a string: the copy is the tracked file set the flake machinery already filtered, so the message suggests `lib.fileset` or `builtins.path { filter }` only to narrow it further; its own lint so `family:coerce = "error"` can keep it at `warn` |
| `store-path-recopy` | coerce | evaluator | off | a path or `builtins.path`/`filterSource` argument already under the store dir copied again (`"${<nixpkgs>}"`, a `builtins.storePath` or pre-existing store path), which doubles the hash in the name; not for the flake root's or a locked input's virtual copy, whose paths already are store paths by construction |
| `read-through-copy` | coerce | evaluator | off | readFile, readDir, pathExists, readFileType, hashFile, import, `builtins.path` or `filterSource` on a store copy this evaluation made by coercion, `builtins.path` or `filterSource` (flake roots and inputs excepted) |
| `drv-bool-null` | coerce | evaluator | off | a Boolean or null coerced into a derivation attribute (`"1"`, `""`) by a `derivation` call the scope sees (Scope); null only outside `__ignoreNulls`; not with `__structuredAttrs`; never for Nix's own Boolean attributes (`__structuredAttrs`, `__ignoreNulls`, `__impureHostDeps`, `__propagatedImpureHostDeps`, `__sandboxProfile`, `__noChroot`, `__darwinAllowLocalNetworking`, `allowSubstitutes`, `preferLocalBuild`), whose encoding is the protocol |
| `drv-list` | coerce | evaluator | off | a list space-joined into a derivation attribute by a `derivation` call the scope sees (Scope); `args` and `outputs` excepted |
| `eq-type-mismatch` | coerce | evaluator | off | `==` or `!=` on values of different types, int and float excepted; a `null` on either side is the test for absence and is exempt |
| `eq-function` | coerce | evaluator | off | `==` or `!=` with a function on either side |
| `or-non-attrs` | coerce | evaluator | off | `x.a or d` where the subject of some element is not an attrset |
| `legacy-let` | legacy | parser | off | `let { body = ...; }` |
| `deprecated-builtin` | legacy | compiler | off | a reference to `isNull`, `toPath`, `unsafeDiscardOutputDependency`, or a `__` alias used directly (`__add`; `__curPos` excepted), bare or as `builtins.x` |
| `overrides` | legacy | compiler | off | `__overrides` in a rec set |
| `unsafe-builtin` | legacy | compiler | off | a reference to any `unsafe*` builtin, `unsafeGetAttrPos` included; the message names it |
| `impure-builtin` | purity | evaluator, compiler | off | a `getEnv` call (evaluator); a reference to `currentTime` (compiler: it is a constant) |
| `current-system` | purity | compiler | off | a reference to `currentSystem` |
| `unhashed-fetch` | purity | evaluator | off | `fetchurl`, `fetchTarball`, `fetchGit` or `fetchTree` called with a URL string, or an attrset without `sha256`, `hash`, `narHash`, `outputHash` or `rev`; fires before the call errors |
| `ifd` | purity | evaluator | off | `import` of a string whose context names a derivation |
| `fod` | purity | evaluator | off | `derivationStrict` producing a fixed-output derivation; the message names the derivation, the hash mode and algorithm |
| `large-source-copy` | purity | evaluator | off | a recursive path copy whose NAR (after filters) exceeds `max-bytes` |
| `git-in-source` | purity | evaluator | **warn** | a recursive path copy of a directory whose top level keeps a `.git` entry |
| `dir-copy-unfiltered` | purity | evaluator | off | a directory outside the store copied whole by a string coercion: no filter, and not a tree the flake machinery already turned into a tracked file set |
| `impure-path-in-drv` | purity | evaluator | off | a derivation attribute (env, `args`, structured attrs) holds the text of a path outside the store and not under a `flake.nix` directory (`toString ./x`), so the .drv depends on the checkout location |
| `absolute-path-in-drv` | purity | evaluator | off | as above, for a path under a `flake.nix` directory: an absolute path a flake evaluation would have copied |
| `if-bool-literal` | style | compiler | off | `if c then true else false` or `if c then false else true` |
| `eq-bool` | style | compiler | off | `x == true`, `x == false`, `x != true`, `x != false` |
| `manual-inherit` | style | compiler | off | `a = a;` in a non-recursive set, or `a = e.a;` in any binding set |
| `empty-let` | style | compiler | **warn** | `let in e` |
| `ellipsis-without-bind` | style | compiler | off | `{ a, ... }:` with `...` and no `@name` |

`impure-path-in-drv` works like Nix's store references: a coercion that
keeps the text of a path outside the store (`toString ./x`, `"/x" +
./y`) marks the string with an `Impure` context element that travels
through concatenation, interpolation, `concatStringsSep`,
`replaceStrings` and attrset coercion like the store elements do. It is
invisible to `getContext` and `hasContext`, `unsafeDiscardStringContext`
strips it, and it never reaches an ATerm. `derivationStrict` reads it per
attribute. The flake root of a path is the nearest ancestor holding a
`flake.nix`; the probes are `Stat` observations of the tracing cache.

## Defaults and parity

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
| `with` | 7134 | 34067 | 71 |
| `with-non-literal` | 3746 | 27100 | 17 |
| `with-nested` | 2611 | 2719 | 3 |
| `shadow-builtin` | 25 | 64 | 2 |
| `shadow-binding` | 763 | 2204 | 38 |
| `unused-binding` | 77 | 204 | 8 |
| `unused-formal` | 299 | 1422 | 22 |
| `rec-unused` | 0 | 0 | 2 |
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
| `legacy-let` | 0 | 0 | 15 |
| `deprecated-builtin` | 35 | 53 | 28 |
| `overrides` | 0 | 0 | 4 |
| `unsafe-builtin` | 47 | 76 | 20 |
| `impure-builtin` | 3 | 7 | 5 |
| `current-system` | 3 | 7 | 4 |
| `unhashed-fetch` | 0 | 0 | 5 |
| `ifd` | 0 | 0 | 0 |
| `fod` | 4 | 4 | 6 |
| `large-source-copy` | 1 | 2 | 0 |
| `git-in-source` | 0 | 0 | 0 |
| `dir-copy-unfiltered` | 0 | 0 | 3 |
| `impure-path-in-drv` | 0 | 0 | 0 |
| `absolute-path-in-drv` | 0 | 0 | 0 |
| `if-bool-literal` | 7 | 9 | 0 |
| `eq-bool` | 205 | 220 | 2 |
| `manual-inherit` | 1002 | 1539 | 19 |
| `empty-let` | 0 | 0 | 0 |
| `ellipsis-without-bind` | 2661 | 2781 | 2 |
| `absolute-path-literal` | 2 | 9 | 26 |
| `short-path-literal` | 0 | 1 | 6 |

The pinned nixpkgs lives in the store, so every copy of a nixpkgs path is
a `store-path-recopy` and never a `dir-copy-unfiltered`; the latter needs
a checkout outside the store. `ifd` never fires on these workloads
because ietsp cannot build (an IFD is an `Unsupported` error), `git-in-
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

The copy holds nixpkgs's `flake.nix`, so every path under it is under a
flake root and the sites are all `absolute-path-in-drv`; the three, on
both workloads:

- `pkgs/tools/package-management/nix/modular/packaging/components.nix:97:7`
- `pkgs/build-support/trivial-builders/default.nix:84:17`
- `nixos/modules/misc/documentation.nix:139:11`

## Cost

At the defaults the lints cost nothing measurable: `bench/ab.nu` on the
commit before the registry against this one gave −3.4 % (desktop, 1
worker), +2.3 % (desktop, 4 workers) and −0.9 % (minimal), inside the
run-to-run noise of that box. An `off` lint is one byte compare at the
site; the parser and compiler hooks add one branch per node kind.

`IETSP_LINT=all=warn` against the defaults, same box (16-core EPYC with
other builds running), `bench/ab.nu --runs 9 --skip-outpaths`, IR and
tracing caches as the benchmark uses them (`--no-cache` keeps the IR
cache), 2026-09-19:

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
- every global reference is prefiltered on its bytes before the
  deprecated/unsafe/impure messages are built;
- each fiber keeps its own set of sites it saw reported, so a site that
  fires a million times takes the shared lock once per fiber;
- warnings are buffered and written in 16 KiB chunks (one syscall per
  line cost 7 % at 4 workers);
- the position of a site inside a store copy is resolved through the
  source table directly; rebuilding `provenance::SourceMap` per warning
  cost 10 %, and `provenance::describe_pos` cannot be used because it
  freezes its map on first use and an error report needs the map of the
  whole run.

## Caches and parallelism

- A lint warning is an effect. `Cache::write` stores the printed lines in
  the entry's `warnings` file and a hit prints them again before the
  result (docs/tracing-cache.md). The lint configuration, scope and
  roots included, is part of the cache key. A lint at `error` fails the
  run, and failed runs are never cached.
- The IR cache keys an entry on the compile-time lint levels the file
  gets after the scope rule and stores the warnings the compile
  produced, so a file loaded from the cache reports them again. With
  every compile-time lint `off` for that file (every store file under
  `scope = "root"`) the key is the same as before the registry existed.
- A site the scope silences enters the shared site set like a reported
  one, so it is classified once per fiber; the classification of a file
  is cached per fiber.
- Warnings are buffered; the buffer is written before `builtins.trace`
  and `builtins.warn` print, before an error report, at the end of every
  command, and when the last evaluator handle drops.
- Any fiber reports a site it reaches first; the shared site set keeps
  each to one line. The parallel demand walk forces list elements a
  sequential `any`, `all` or `elem` would skip, so the set of sites at N
  workers can be a superset of the sequential one, and when two values
  share one site (an attribute position that several derivations reuse)
  the message shows the value that arrived first.
- Embedded sources (`derivation.nix`, `fetchurl.nix`, `call-flake.nix`,
  the imported-drv wrapper) are never linted.

## Limits

- The compiler lints on references see what the source names:
  `builtins.isNull`, `isNull`, `__unsafeGetAttrPos`. `lib.isNull`, `with
  builtins; isNull` (a `with` variable) and `builtins."isNull"` are not
  seen.
- `unused-formal` cannot see `builtins.functionArgs` introspection and
  does not report the formals of a lambda whose `@name` is referenced.
- `unused-binding` treats a self-reference (`let x = x + 1;`) as a use
  and exempts `_`-prefixed names: a let name is the author's, unlike a
  formal.
- The scope sees a site's file only. A derivation that nixpkgs builds
  from values the user passed is attributed through the positions of
  its attribute set (see Scope); a nixpkgs fetcher call keeps none from
  the user's file and stays silent under `scope = "root"`. `drv-list`
  and `drv-bool-null` know a `derivation` call by its `builder` binding,
  so a wrapper in the user's repository is the user's contract and is
  reported, and a list passed through such a wrapper from a file out of
  scope is reported at the wrapper's `builder` binding.
- `dir-copy-flake` needs a flake evaluation, so its fixture is the
  scratch flake of `scripts/linttest.nu`, not a `tests/lint/*.nix` file.
- `read-through-copy` only knows the copies this evaluation made through
  a coercion, `builtins.path` or `filterSource`; a flake root or input is
  a tracked tree and reading through it is the normal way.
- `dir-copy-unfiltered`, `large-source-copy` and `git-in-source` look at
  the copied tree after the filters of the tree it lies in.
- `impure-path-in-drv` sees the text of a path only through the marker:
  a string rebuilt from `substring` or `match` loses it, like store
  context. A structured-attrs value keeps the marker through
  `builtins.toJSON`.
- `unhashed-fetch` treats any of `sha256`, `hash`, `narHash`,
  `outputHash` or `rev` as pinning; it does not check the value.
- One warning per (lint, position): `fod` and the derivation attribute
  lints report at the position of the `name` or the attribute binding,
  and `fetchurl`'s `name` is one position for every fixed-output
  derivation it makes, so the message names the first derivation that
  reached it. The count of firing sites, not of derivations, is what the
  table above shows.
- `eval-jobs` with worker processes (`--max-memory-size`, the default)
  deduplicates per process.

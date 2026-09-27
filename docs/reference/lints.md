# Lints

Every lint: its name, family, hook, default level and trigger, plus the
level resolution, the scope rules and the `# ietsp: allow` comment.

ietsp reports what only an evaluator can know. It has no opinion about
how code looks. `docs/explanation/lint-scope.md` has the rule, and it
tells you how to make rules of your own with `ietsp parse --json`.

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

With `--log-format internal-json`, or in the `json-log-path` file, a
finding is a record with the code `lint`. The record has the byte span of
the site (`start`, `end`). It has the text that corrects the finding
(`replacement`) when the lint has one. `docs/reference/errors.md` has the
record ("A finding and its edit").

The level of a lint comes from these sources, in this order; a later
source wins over an earlier one:

1. the built-in default (the table below);
2. the `[lint]` table of the machine's `ietsp.toml`, then of the user's
   (`docs/reference/settings.md`);
3. the `[lint]` table of the project's `ietsp.toml`: the nearest one found
   by walking up from the entry file's directory, the flake directory, or
   cwd for `-E` (each `[lint.inputs.<node>]` table is a source of its own,
   see Scope); any key but `[lint]` in this file is an error
   (`docs/reference/settings.md`);
4. `IETSP_LINT="name=level,name=level"`;
5. `--lint SELECTOR=LEVEL`, repeatable.

A selector is a lint name, `all`, `family:NAME` (`path`, `coerce`,
`legacy`, `purity`, `meta`), or one of Nix's setting names
(`lint-absolute-path-literals`, `lint-short-path-literals`,
`lint-url-literals`), which count as lint names. The old flags
`--lint-absolute-path-literals MODE`, `--lint-short-path-literals MODE`
and `--lint-url-literals MODE` still work: each is `--lint <nix
name>=MODE`.

Inside one source the narrower selector wins, whatever the order: a lint
name beats `family:NAME`, which beats `all`. A TOML table has no order,
so this is the only rule that can hold there:

```toml
[lint]
"family:coerce" = "error"
dir-copy-flake = "warn"     # warn, in either order
```

The `--lint` flags of one command line are one source, and so are the
pairs of one `IETSP_LINT`: among settings of the same specificity the
later one wins, but a name given earlier still beats a family or `all`
given later. `--lint all=error --lint fod=off` and `--lint fod=off
--lint all=error` both give `fod=off` with everything else at `error`;
`--lint fod=off --lint fod=warn` gives `warn`. Between sources the
specificity does not count: `all = "off"` in `IETSP_LINT` turns off a
lint `ietsp.toml` names.

A selector that the registry does not have is not the same in each source:

| source | effect |
|---|---|
| an `ietsp.toml`, `IETSP_LINT` | one warning for each name, with the code `unknown-lint`; ietsp ignores the name and the run continues |
| `--lint` | a usage error, exit code 2 |

```
warning: /repo/ietsp.toml: ietsp does not know the lint 'nope' and ignores it; 'ietsp help lints' has the list (docs/reference/lints.md)
```

An error in a file stops each command in that repository. That includes
the command that installs the correction. Thus a file gets a warning.
The command line gets an error, because the user can correct it
immediately. An incorrect level (`fod = "loud"`) is an error in each
source.

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

## Silencing one site

A comment `# ietsp: allow NAME[, NAME...]` silences the named lints for
one source line, whatever their level (`warn` or `error`). The names
are selectors: lint names, Nix's setting names, `family:NAME`, or
`all`; commas or spaces separate them.

```nix
# ietsp: allow url-literal
src = http://example.com/src.tar.gz;

x == "a" # ietsp: allow eq-type-mismatch
```

- On a line of its own the comment covers the next line that is neither
  blank nor a comment. After code it covers its own line. Both forms
  on one line add up.
- The covered line is a set of positions: every site whose position
  falls on it is silent, a compile-time lint (`url-literal`) and a
  runtime lint (`eq-type-mismatch`, `fod`) alike. A site is the position
  where its expression starts: a string that opens on the covered line
  and spans ten more is silent; one that opens on the line after is not.
- A name the registry does not know is the `unknown-allow` lint (family
  `meta`, default `warn`), reported at the word's position, so a typo
  does not silently silence nothing. The known names of the same
  comment still count. `allow` is the only directive; another word
  after `# ietsp:` is `unknown-allow` too.
- The comment must start with exactly `# ietsp:`; `#ietsp:` and `/* ietsp:
  */` are plain comments.
- An allow that silences nothing is not reported: a runtime lint fires
  only when the value is computed, so an unused allow on one run may be
  the one that matters on the next.

The lexer notes the `# ietsp:` comments (other comments cost nothing
more), the parser resolves the names into a per-file table of covered
lines, and the compiler and the evaluator check that table before a
site's level. The table travels in the IR cache entry with the
warnings, and it is part of the file's bytes, so both cache keys cover
it: a run whose comment changed misses.

## Configuration file

```toml
# ietsp.toml, anywhere above the file being evaluated
[lint]
path-copy = "warn"
unhashed-fetch = "error"
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
# a fixed-output derivation is how a source is fetched
fod = "off"
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
| `path-copy` | coerce | evaluator | off | a path coerced to a string with a copy to the store: interpolation, `+`, a derivation attribute; not `toString`. Under a flake root or a locked input `"${./file}"` is the normal reference: silent for a regular file, and a directory is `dir-copy-flake` |
| `dir-copy-flake` | coerce | evaluator | off | a directory under a flake root or a locked input coerced to a string: the copy is the tracked file set the flake machinery already filtered, so the message suggests `lib.fileset` or `builtins.path { filter }` only to narrow it further; its own lint so `family:coerce = "error"` can keep it at `warn` |
| `store-path-recopy` | coerce | evaluator | off | a path or `builtins.path`/`filterSource` argument already under the store dir copied again (`"${<nixpkgs>}"`, a `builtins.storePath` or pre-existing store path), which doubles the hash in the name; not for the flake root's or a locked input's virtual copy, whose paths already are store paths by construction |
| `read-through-copy` | coerce | evaluator | off | readFile, readDir, pathExists, readFileType, hashFile, import, `builtins.path` or `filterSource` on a store copy this evaluation made by coercion, `builtins.path` or `filterSource` (flake roots and inputs excepted) |
| `drv-bool-null` | coerce | evaluator | off | a Boolean or null coerced into a derivation attribute (`"1"`, `""`) by a `derivation` call the scope sees (Scope); null only outside `__ignoreNulls`; not with `__structuredAttrs`; never for Nix's own Boolean attributes (`__structuredAttrs`, `__ignoreNulls`, `__impureHostDeps`, `__propagatedImpureHostDeps`, `__sandboxProfile`, `__noChroot`, `__darwinAllowLocalNetworking`, `allowSubstitutes`, `preferLocalBuild`), whose encoding is the protocol |
| `drv-list` | coerce | evaluator | off | a list space-joined into a derivation attribute by a `derivation` call the scope sees (Scope); `args` and `outputs` excepted |
| `eq-type-mismatch` | coerce | evaluator | off | `==` or `!=` on values of different types, int and float excepted; a `null` on either side is the test for absence and is exempt |
| `eq-function` | coerce | evaluator | off | `==` or `!=` with a function on either side |
| `or-non-attrs` | coerce | evaluator | off | `x.a or d` where the subject of some element is not an attrset |
| `impure-builtin` | purity | evaluator, compiler | off | a `getEnv` call (evaluator); a reference to `currentTime` (compiler: the value is a thunk of the globals, without a position of its own) |
| `unhashed-fetch` | purity | evaluator | off | `fetchurl`, `fetchTarball`, `fetchGit` or `fetchTree` called with a URL string, or an attrset without `sha256`, `hash`, `narHash`, `outputHash` or `rev`; fires before the call errors |
| `ifd` | purity | evaluator | off | `import` of a string whose context names a derivation |
| `fod` | purity | evaluator | off | `derivationStrict` producing a fixed-output derivation; the message names the derivation, the hash mode and algorithm |
| `large-source-copy` | purity | evaluator | off | a recursive path copy whose NAR (after filters) exceeds `max-bytes` |
| `git-in-source` | purity | evaluator | **warn** | a recursive path copy of a directory whose top level keeps a `.git` entry |
| `dir-copy-unfiltered` | purity | evaluator | off | a directory outside the store copied whole by a string coercion: no filter, and not a tree the flake machinery already turned into a tracked file set |
| `impure-path-in-drv` | purity | evaluator | off | a derivation attribute (env, `args`, structured attrs) holds the text of a path outside the store and not under a `flake.nix` directory (`toString ./x`), so the .drv depends on the checkout location |
| `absolute-path-in-drv` | purity | evaluator | off | as above, for a path under a `flake.nix` directory: an absolute path a flake evaluation would have copied |
| `unknown-allow` | meta | parser | **warn** | a word of a `# ietsp: allow` comment that is not a lint name, a family or `all`, or a `# ietsp:` directive other than `allow` (Silencing one site) |

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
| `absolute-path-literal` | 2 | 9 | 26 |
| `short-path-literal` | 0 | 1 | 6 |
| `unknown-allow` | 0 | 0 | 0 |

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
site.

The numbers below are from 2026-09-19. The registry then held 18 more
lints, which the parser and the compiler reported
(docs/explanation/lint-scope.md). The cost of the present registry is
not higher.

`IETSP_LINT=all=warn` against the defaults, same box (16-core EPYC with
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

The `# ietsp: allow` table costs the parse nothing measurable: `ietsp
parse -q` over the 44,303 files of the pinned nixpkgs (hyperfine, 30
runs, 2026-09-19) took 1.128 s ± 0.034 before and 1.122 s ± 0.025 after;
the instruction count (`perf stat`) rose 0.5 %, from 6.728 G to 6.763 G.
The `#` arm of the lexer's trivia loop is a function of its own: inlined,
it grew `next_token` past what its callers inline, which cost 1.5 % of
the instructions. At a site the check is one branch on an empty table.

## Caches and parallelism

- A lint warning is an effect. `Cache::write` stores the record of each
  warning in the entry, and a hit puts them out again before the
  result (docs/explanation/caching.md). The lint configuration, scope and
  roots included, is part of the cache key. A lint at `error` fails the
  run, and failed runs are never cached.
- The IR cache keys an entry on the compile-time lint levels the file
  gets after the scope rule and stores the warnings the compile
  produced, so a file loaded from the cache reports them again, and the
  file's `# ietsp: allow` table, so a runtime lint on a loaded file is
  silenced like on a compiled one. With every compile-time lint `off`
  for that file (every store file under `scope = "root"`) the key is
  the same as before the registry existed.
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

- The `currentTime` half of `impure-builtin` sees what the source names:
  `builtins.currentTime`, `currentTime`, `__currentTime`. `with builtins;
  currentTime` (a `with` variable) and `builtins."currentTime"` are not
  seen.
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
- `# ietsp: allow` covers positions on one line, so the derivation
  attribute lints, whose site can be the `name` binding or the
  `derivationStrict` call (Scope), need the comment at that site, not
  at the value.

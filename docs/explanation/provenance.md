# Provenance: from a store path back to the file that made it

A store path tells you nothing about the tree it came from. A `.drv` path
tells you nothing about the attribute that made it. ietsp keeps both while
it evaluates, and prints them.

So a failed build says this:

```
error: build of packages.x86_64-linux.boom failed (shcrns9…-prov-flake-boom.drv),
       defined at /home/u/repo/flake.nix:4:7: status 9: …
```

instead of this:

```
error: build of '/nix/store/shcrns9…-prov-flake-boom.drv' failed: status 9: …
```

and a line in the build log the daemon relays says `tree is /home/u/repo`
where it would have said `tree is /nix/store/3nma1…-source`.

## Why this is worth the machinery

A store path is a hash of everything that went into it. That is exactly
what makes it useless for navigation: it deliberately erases where it came
from. In normal use that is fine, because you have the expression in front
of you. It stops being fine at the two moments you most need to know:

- **A build failed.** The failure names a `.drv`. Finding the package that
  produced it means searching, and in a NixOS configuration with several
  thousand derivations that search is not trivial.
- **A build log mentions a path.** The path is a copy of a tree you are
  editing. The line tells you which file the builder was unhappy about, but
  under a name that does not exist in your editor.

Both are cheap to fix at the moment the information exists, and expensive
to reconstruct afterwards. So ietsp records it during evaluation.

## The source map

A source map is a list of prefixes, each mapping a store path prefix to a
local directory, ordered so the longest match comes first. One function
resolves a single path, another rewrites every store path in a line of
text. A mapping that points at another mapped tree is resolved when the map
is built, so `./.` inside a flake that is already in the store still lands
on the working copy.

Three things fill it:

1. **Every tree the evaluation copied.** `./path`, `filterSource`,
   `builtins.path`, the root flake's tree, and every locked flake input of
   type `path`. This is the case that matters: the repository you are
   working on.
2. **The `source-map` setting**, for trees this evaluation did not copy.
   A list of `"STOREPATH=LOCALDIR"` strings, in the user
   `ietsp.toml`. This
   is how a nixpkgs checkout is mapped onto the store path of the nixpkgs
   the lock names. It was a `nix.conf` setting first; Nix warns about a key
   it does not know, so it moved to ietsp's own file. Nothing checks that
   the directory holds the same tree as the store path.
3. **A revision**, for display only, when the root flake is a git working
   tree: the `HEAD` commit, with `-dirty` appended when the tree is dirty.

## Which position is "where it was defined"

This is the part that needed thought. A derivation could be attributed to
several positions, and only one of them is useful.

The position recorded is **the position of the `name` attribute's value**,
not of the `derivation` call. The reason is mechanical: `derivation` is a
Nix-level wrapper around the `derivationStrict` primop, so for every normal
call the primop's own call position is inside an internal file and says
nothing. (A direct `builtins.derivationStrict` call does keep its own
position.) This is also the position Nix itself names when it says "whose
name attribute is located at".

`stdenv.mkDerivation` computes `name` in `make-derivation.nix` but passes
the package's `pname` through, so the position of `pname` comes first —
which is what nixpkgs' own `meta.position` does. A package that sets `pname`
therefore points at its own file.

A package that sets only `name` through a nixpkgs builder — `runCommand`,
`writeShellApplication` — loses that binding to `make-derivation.nix`. The
fallback is the first attribute the set still holds from a lint root, which
is usually `src`, an attribute of `runCommand`'s environment. The same
fallback the value lints use. A bare `runCommand "x" { } "…"` holds none of
them and points into nixpkgs; there is nothing better to point at.

The flake root and `./.` cases build `derivation` directly or set `name` in
the package file, so they point where you want.

**The attribute path comes from the command line walk, not from the
evaluator.** Classic mode records the path it walked; `ietsp build` uses
`-A` or the flake fragment. The evaluator core is untouched by any of this.

## What is printed, and what is not

| place | with provenance | without |
|---|---|---|
| build started | `building <attr> (<file>:<line>:<col>)` | `building <drv path>` |
| build failed | `error: build of <attr> failed (<drv basename>), defined at <file>:<line>:<col>: <reason>` | `error: build of <drv path> failed: <reason>` |
| build log | every mapped store path replaced | the store path |
| evaluation errors | positions and messages mapped | the store path |
| `eval-jobs --provenance` | a `pos` key per job | no `pos` key |

Classic `nix-build` and `nix-instantiate` stdout is unchanged. Only stderr
carries provenance, so the byte comparison against the real `nix` — which
compares stdout and the exit code — is unaffected. `nix-eval-jobs` 2.35 has
no position field, which is why `pos` needs a flag: the default output must
stay flag-identical.

Some things are deliberately not mapped:

- **A derivation read from `/nix/store`** — an `import` of a `.drv`, a build
  of a `.drv` path, a run that hit the evaluation cache. There is no
  position and no attribute, because the cache keeps neither. A cache hit
  still rewrites store paths in text; it just names no derivation.
- **Outputs.** An output path is not a source. Whatever tree an output came
  from, the output is not that tree, so rewriting it would be a lie.
- **nixpkgs and other non-`path` inputs.** Their trees were fetched, not
  copied from a local directory. Use `source-map`.
- **`builtins.toFile` texts**, which are held in memory and come from no
  local tree.

`IETSP_PROVENANCE=0` turns every rewrite and every provenance line off, for
a byte comparison against the real `nix`.

## One position, one verdict

Two small design points are worth naming because both were bugs first.

**The position is added in exactly one place.** A caller hands over the bare
attribute and the dispatcher composes `<attr> (<pos>)` for the state-change
lines and `, defined at <pos>` for the failure line. A caller that passed a
composed name instead would get the position twice.

**A remote build produces two activities for one derivation.** A local
daemon with a remote builder runs the build behind a build hook, so there is
the hook's own `building X on 'ssh-ng://…'` and the build itself. The
renderer therefore drops a start for a derivation it is already showing.
And while a build operation owns a derivation, the end of an activity prints
no verdict at all — it cannot: the protocol message says a build ended, not
whether it worked, and the hook's activity ends first. The operation's own
result decides, and the log buffer stays open until then, so the daemon's
error text lands inside that build's log block instead of being printed
raw. A derivation the operation does not own — a dependency — keeps the
older behaviour and takes its verdict from the daemon's failure messages.

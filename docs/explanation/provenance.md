# Provenance: from a store path back to the file that made it

A store path does not say which tree it came from. A `.drv` path does not
say which attribute made it. iets remembers both while it evaluates, and
shows them to you.

So when a build fails, you see this:

```
error: build of packages.x86_64-linux.boom failed (shcrns9…-prov-flake-boom.drv),
       defined at /home/u/repo/flake.nix:4:7: status 9: …
```

instead of this:

```
error: build of '/nix/store/shcrns9…-prov-flake-boom.drv' failed: status 9: …
```

And a line in the build log says `tree is /home/u/repo` where it would have
said `tree is /nix/store/3nma1…-source`.

## Why this is worth the machinery

A store path is a hash of everything that went into it. That is exactly
why it is no help when you want to find something: it hides where it came
from on purpose. Most of the time that is fine, because the expression is
in front of you. It stops being fine at the two moments you most need to
know:

- **A build failed.** The failure names a `.drv`. To find the package that
  made it, you have to search. In a NixOS configuration with several
  thousand derivations, that search is not easy.
- **A build log mentions a path.** The path is a copy of a tree you are
  editing. The line tells you which file the builder did not like, but
  under a name your editor does not know.

Both are cheap to fix while the information still exists, and expensive to
rebuild later. So iets records it during evaluation.

## The source map

A source map is a list of prefixes. Each one maps a store path prefix to a
local directory, and the longest match comes first. One function resolves
a single path. Another rewrites every store path in a line of text. When a
mapping points at another mapped tree, iets resolves it when it builds the
map. So `./.` inside a flake that is already in the store still lands on
your working copy.

Three things fill the map:

1. **Every tree the evaluation copied.** `./path`, `filterSource`,
   `builtins.path`, the root flake's tree, and every locked flake input of
   type `path`. This is the case that matters: the repository you are
   working on.
2. **The `source-map` setting**, for trees this evaluation did not copy.
   It is a list of `"STOREPATH=LOCALDIR"` strings in your user
   `iets.toml`. Use it to map a nixpkgs checkout onto the store path of the
   nixpkgs the lock names. It started as a `nix.conf` setting, but Nix
   warns about keys it does not know, so it moved to iets's own file.
   Nothing checks that the directory holds the same tree as the store path.
3. **A revision**, for display only, when the root flake is a git working
   tree: the `HEAD` commit, with `-dirty` added when the tree is dirty.

## Which position is "where it was defined"

A derivation could point at several positions. Only one of them helps you.

iets records **the position of the `name` attribute's value**, not of the
`derivation` call. The reason is mechanical. `derivation` is a wrapper,
written in Nix, around the `derivationStrict` primop. So for a normal call,
the primop's own position is inside an internal file and tells you
nothing. (A direct `builtins.derivationStrict` call does keep its own
position.) Nix names the same position when it says "whose name attribute
is located at".

`stdenv.mkDerivation` computes `name` in `make-derivation.nix`, but it
passes the package's `pname` through. So the position of `pname` comes
first, which is also what nixpkgs' own `meta.position` does. A package that
sets `pname` points at its own file.

A package that sets only `name` through a nixpkgs builder, like
`runCommand` or `writeShellApplication`, loses that binding to
`make-derivation.nix`. Then iets falls back to the first attribute the set
still holds from a lint root. That is usually `src`, an attribute of
`runCommand`'s environment. The value lints use the same fallback. A bare
`runCommand "x" { } "…"` holds none of them and points into nixpkgs. There
is nothing better to point at.

The flake root and `./.` cases build `derivation` directly or set `name` in
the package file, so they point where you want.

**The attribute path comes from the command line walk, not from the
evaluator.** Classic mode records the path it walked. `iets build` uses
`-A` or the flake fragment. None of this touches the evaluator core.

## What is printed, and what is not

| place | with provenance | without |
|---|---|---|
| build started | `building <attr> (<file>:<line>:<col>)` | `building <drv path>` |
| build failed | `error: build of <attr> failed (<drv basename>), defined at <file>:<line>:<col>: <reason>` | `error: build of <drv path> failed: <reason>` |
| build log | every mapped store path replaced | the store path |
| evaluation errors | positions and messages mapped | the store path |
| `eval-jobs --provenance` | a `pos` key per job | no `pos` key |

Classic `nix-build` and `nix-instantiate` keep the same stdout. Only stderr
carries provenance. So the byte comparison against the real `nix`, which
compares stdout and the exit code, still holds. `nix-eval-jobs` 2.35 has no
position field. That is why `pos` needs a flag: the default output must
stay the same as Nix's.

Some things are not mapped, on purpose:

- **A derivation read from `/nix/store`**: an `import` of a `.drv`, a build
  of a `.drv` path, or a run that hit the evaluation cache. There is no
  position and no attribute, because the cache keeps neither. A cache hit
  still rewrites store paths in text. It just names no derivation.
- **Outputs.** An output path is not a source. Whatever tree an output came
  from, the output is not that tree, so rewriting it would be a lie.
- **nixpkgs and other inputs that are not `path`.** Their trees were
  fetched, not copied from a local directory. Use `source-map`.
- **`builtins.toFile` texts.** They live in memory and come from no local
  tree.

Set `IETS_PROVENANCE=0` to turn off every rewrite and every provenance
line, for a byte comparison against the real `nix`.

## One position, one verdict

Two small design points are worth naming, because both were bugs first.

**The position is added in exactly one place.** A caller passes the bare
attribute. The dispatcher writes `<attr> (<pos>)` for the state-change
lines and `, defined at <pos>` for the failure line. A caller that passed a
composed name would get the position twice.

**A remote build makes two activities for one derivation.** A local daemon
with a remote builder runs the build behind a build hook. So there is the
hook's own `building X on 'ssh-ng://…'`, and there is the build itself. The
renderer drops a start for a derivation it already shows. While a build
operation owns a derivation, the end of an activity prints no verdict at
all. It cannot: the protocol message says a build ended, not whether it
worked, and the hook's activity ends first. The operation's own result
decides. Until then, the log buffer stays open, so the daemon's error text
lands inside that build's log block instead of being printed raw. A
derivation the operation does not own, such as a dependency, keeps the
older behaviour: its verdict comes from the daemon's failure messages.

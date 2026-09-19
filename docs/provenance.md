# Provenance

A store path tells you nothing about the tree it came from, and a `.drv` path
tells you nothing about the attribute that made it. ietsp keeps both while it
evaluates, and prints them:

```
error: build of packages.x86_64-linux.boom failed (shcrns9…-prov-flake-boom.drv),
       defined at /home/u/repo/flake.nix:4:7: status 9: …
```

instead of

```
error: build of '/nix/store/shcrns9…-prov-flake-boom.drv' failed: status 9: …
```

and, in the build log the daemon relays,

```
tree is /home/u/repo            (was: tree is /nix/store/3nma1…-source)
```

## The source map

`crate::provenance::SourceMap` maps a store path prefix to a local directory.
It is a vector of prefixes ordered so the longest match comes first;
`locate` resolves one path and `rewrite` every store path in a line of text.
A mapping that points at another mapped tree is resolved when the map is
built, so `./.` inside a flake that is already in the store still lands on the
working copy.

Three sources fill it:

1. **Every tree the evaluation copied.** The evaluator records a
   `SourceOrigin::Tree { local, .. }` for `./path`, `filterSource`,
   `builtins.path`, the root flake's tree (`src/flake/root.rs`) and every
   locked flake input of type `path`. This is the case that matters: the repo
   you are working on.
2. **The `source-map` setting** in `ietsp.conf` or `--ietsp-option`
   (`docs/config.md`), for trees this evaluation did not copy:
   whitespace-separated `STOREPATH=LOCALDIR` entries. `extra-source-map`
   appends. This is how a nixpkgs checkout is mapped:

   ```
   source-map = /nix/store/ah3pxz4…-source=/home/u/nixpkgs
   ```

   It was a `nix.conf` setting before. Nix warns about a key it does not know,
   so the setting moved to ietsp's own file.
3. **A revision**, for display only, when the root flake is a git working
   tree: the `HEAD` commit, with `-dirty` when the tree is dirty.

## Where a derivation was defined

`DrvInfo` carries `pos` and `attr`.

`pos` is the position of the `name` attribute's value, not of the
`derivation` call. `derivation` is a Nix-level wrapper around the
`derivationStrict` primop (`src/corepkgs/derivation.nix`), so the primop's
own call position is inside `«derivation-internal»` for every normal call and
is useless; a direct `builtins.derivationStrict` call does keep its own
position. This is the same position Nix names in "whose name attribute is
located at". For `stdenv.mkDerivation` it therefore points into
`mk-derivation.nix`, not at the package; the flake root and `./.` cases, which
build `derivation` directly or set `name` in the package file, point where you
want.

`attr` comes from the cli walk, not from the evaluator: `getDerivations` in
classic mode records the path it walked
(`DrvState::set_attr`), and `ietsp build` uses `-A` or the flake fragment.
The evaluator core is untouched.

## What is printed

| place | with provenance | without |
|---|---|---|
| `Started` | `building <attr> (<file>:<line>:<col>)` | `building <drv path>` |
| `Failed` | `error: build of <attr> failed (<drv basename>), defined at <file>:<line>:<col>: <reason>` | `error: build of <drv path> failed: <reason>` |
| `Log` | every mapped store path replaced | the store path |
| evaluation errors (`report`) | positions and messages mapped | the store path |
| `eval-jobs --provenance` | a `pos` key per job | no `pos` key |

`nix-eval-jobs` 2.35 has no position field, so `pos` needs `--provenance` and
the default output stays flag-identical (`docs/eval-jobs.md`). Classic
`nix-build` and `nix-instantiate` stdout is unchanged: only stderr carries
provenance, so `scripts/classic-e2e.nu`, which compares stdout and the exit
code, is unaffected.

## What is not mapped

- **Derivations read from `/nix/store`** (`import …drv`, `ietsp build DRV`,
  a run that hit the evaluation cache): no position and no attribute. The
  cache keeps neither, so a cache hit still rewrites store paths but names no
  derivation.
- **Outputs**, and any path built elsewhere: an output path is not a source,
  so it is left alone.
- **nixpkgs and other inputs that are not `path` inputs**: their trees were
  fetched, not copied from a local directory. Map them with `source-map`.
- **`builtins.toFile` texts**: held in memory, not from a local tree.
- **Anything after the map is built**, for `report` and `eval-jobs`: those two
  run while the evaluation is live, so they use a map built at first use. The
  flake root is copied before the outputs are walked, so it is always in.

## Turning it off

`IETSP_PROVENANCE=0` disables every rewrite and every provenance line, for a
byte comparison against the real nix. `scripts/provenance-e2e.nu` checks both
directions against a live daemon (11 checks).

## One position, one verdict

The position is added in exactly one place, `build::Names` in the dispatcher:
a caller hands over the bare attribute (`Provenance::attr_of`, falling back to
the derivation's own name) and the dispatcher composes `<attr> (<pos>)` for the
state-change lines and `, defined at <pos>` for the failure line. A caller that
passed `name_of` instead would get the position twice.

A local daemon with a remote builder runs the build behind a build hook, which
means **two build activities for one derivation**: the hook's own
`building X on 'ssh-ng://…'` and the build itself. So the renderer drops a
`Started` for a derivation it is already showing, and while a build op owns a
derivation (`Renderer::owns`) the end of an activity prints no verdict at all —
it cannot: `STDERR_STOP_ACTIVITY` says a build ended, not whether it worked,
and the hook's activity ends first. The op's own result decides, and the buffer
stays open until then, so the daemon's error text lands inside that build's log
block instead of being printed raw. A derivation the op does not own (a
dependency) keeps the old behaviour: its verdict comes from the failure
messages the daemon printed.

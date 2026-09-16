# Flakes

`ietsp eval --raw DIR#attr.path` evaluates a locked flake and prints what
`nix eval --raw --option eval-cache false DIR#attr.path` prints. Code:
`src/flake/`. Judge: `scripts/flaketest.sh`.

## Scope

- `DIR` is `.`, `./x`, `/abs`, `~/x` or `path:/abs`. Registry refs
  (`nixpkgs#hello`), URLs and `?query` parameters are errors.
- `#attr.path` selects from the flake's outputs; elements may be quoted
  like Nix (`"x86_64-linux"`). No `#` or a bare `#` gives the outputs
  attrset. No `packages.<system>.` defaulting.
- `flake.lock` must exist and be complete: every node reachable from the
  root has a `narHash` (or is a relative `path:` input), and every input
  `flake.nix` declares has a lock entry. Otherwise:
  `ietsp: flake.lock missing or incomplete, run nix flake lock`.
  A `flake.nix` input whose URL changed without re-locking is not detected;
  the lock wins.
- `--strict`, `--json` and `--raw` work as with a file. `--raw` mirrors
  `nix eval --raw`: the value is coerced to a string without copying paths
  to the store, and printed without a newline.

## How it works (flake.cc:callFlake)

1. The root tree's `sourceInfo` is built like Nix's fetcher for the ref
   the CLI would produce (flakeref.cc): a directory inside a git repository
   is `git+file://<root>?dir=<subdir>`, anything else is `path:`.
   - git (git.cc:getAccessorFromWorkdir, git-utils.cc:getWorkdirInfo): the
     tree is the set of index entries that still exist in the working
     tree, with the working-tree contents and modes; untracked files,
     `.git` and submodules are excluded. `git ls-files --stage` and
     `git status --porcelain -uno` give the set and the dirty flag,
     `rev-parse HEAD`, `log -1 --format=%ct` and `rev-list --count` the
     metadata. Clean: `rev`, `shortRev`, `revCount` (not when
     `.git/shallow` exists). Dirty: `dirtyRev`, `dirtyShortRev`.
     `submodules = false` always, `lastModified` is the HEAD commit time.
   - path (path.cc): the whole directory; `lastModified` is the newest
     `lstat` mtime in it.
   The store path is `make_fixed_output_path(recursive, narHash, "source")`.
2. `call-flake.nix` from Nix 2.34.8 is embedded verbatim and called with
   the lock file text, `{ root = { sourceInfo; dir; }; }` and
   `builtins.fetchTree`.
3. `builtins.fetchTree` on a locked node (`flake::fetch`) derives the store
   path from `narHash` and returns `emitTreeAttrs`: `outPath`, `narHash`,
   `rev`/`shortRev` when `rev` is present, `revCount`, `dirtyRev`/
   `dirtyShortRev`, `lastModified`/`lastModifiedDate` (`%Y%m%d%H%M%S`
   UTC), `submodules` for `git`. A `path` input is NAR-hashed and checked
   against `narHash`. Unlocked inputs (no `narHash`) and URL arguments are
   errors.

The flake's `flake.nix` is then imported from the real store path, so
positions, `./x` path literals and `readDir` see exactly what Nix sees.

## The one place ietsp runs nix

A tree that is not in `/nix/store` must get there first:
`flake::fetch::ensure_in_store` runs
`nix eval --raw --impure --expr '(builtins.fetchTree <locked attrs>).outPath'`
once, then checks that the path Nix reports is the one the lock (or the
root tree hash) implies, so a difference in the file set is an error, not
a silent divergence. This is also how a dirty working tree gets copied.
Nothing else in ietsp runs nix; ietsp never writes to the store itself.

## Tracing cache

Everything above is recorded (`docs/tracing-cache.md`): the git queries as
`Cmd` observations, the working-tree file set as a filtered `Tree` (no
`seen_hash`: unlisted entries cannot change the result), the lock file as a
`File`, each store tree's existence as a `Stat`. The key holds the
reference text, the resolved directory, cwd, and the hashes of `flake.nix`
and `flake.lock`. A commit, an edit of a tracked file, `git add`, or a
garbage-collected input all miss; an untracked file does not.

## Measurements

`~/home#devShells.x86_64-linux.default.drvPath`, 31 locked inputs,
2026-09-16, same machine as `docs/tracing-cache.md`:

| run | wall |
|---|---|
| `nix eval --raw --option eval-cache false` | 4.28 s |
| `ietsp eval --raw --no-cache` | 0.49 s |
| `ietsp eval --raw`, cache miss (records and writes 2,535 entries) | 0.56 s |
| `ietsp eval --raw`, cache hit | 0.02 s |
| `IETSP_WORKERS=8 ietsp eval --raw --no-cache` | 0.62 s |

`~/home#nixosConfigurations.agents.config.system.build.toplevel.drvPath`:
nix 7.21 s, ietsp 1.27 s uncached.

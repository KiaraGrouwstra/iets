# Flakes

`ietsp eval --raw DIR#attr.path` evaluates a locked flake and prints what
`nix eval --raw --option eval-cache false DIR#attr.path` prints. Code:
`src/flake/`. Judge: `scripts/flaketest.nu`.

## Scope

- `DIR` goes through the parser `builtins.parseFlakeRef` uses
  (`src/flake/flakeref.rs`, see "Flake references" below), with cwd as
  the base directory. Only a local path is taken: `.`, `./x`, `/abs`,
  `~/x`, or a `path:` URL (`path:/abs`, `path:./x`, percent-escapes
  decoded). The fragment is percent-decoded, as in Nix. These are
  `ErrorKind::Unsupported`, so classic mode falls back on them:
  - registry refs (`nixpkgs#hello`, and `x#y` even where `./x` exists, as
    in flakeref.cc:parseFlakeIdRef);
  - remote refs (`github:`, `gitlab:`, `git+https:`, `git+file:`,
    `https://….tar.gz`): ietsp cannot fetch a root flake, see "A remote
    root flake" below;
  - `?query` parameters on a path (`?dir=`, `?rev=`, `?narHash=`);
  - what the parser itself leaves out (`hg+…`, `sourcehut:`,
    docs/divergences.md).

  Some invalid references are accepted (docs/divergences.md); an unknown
  scheme (`foo:bar#x`) is unsupported.
- `#attr.path` selects from the flake's outputs; elements may be quoted
  like Nix (`"x86_64-linux"`). No `#` or a bare `#` gives the outputs
  attrset. No `packages.<system>.` defaulting.
- `flake.lock` must exist and be complete: every node reachable from the
  root has a `narHash` (or is a relative `path:` input), and every input
  `flake.nix` declares has a lock entry. Otherwise:
  `ietsp: flake.lock missing or incomplete, run nix flake lock`.
  A `flake.nix` input whose URL changed without re-locking is not detected;
  the lock wins.
- `builtins.getFlake REF` takes an absolute `/abs` or `path:/abs` (parsed
  like `parseFlakeRef`) and
  returns what `callFlake` returns. It is impure only: every REF ietsp
  resolves is unlocked, which pure evaluation refuses (`docs/pure-eval.md`).
  REF is always a `path:` input, also inside a git
  repository, because flakeref.cc looks for `.git` only when it has a base
  directory and getFlake passes none. A relative REF and a `#fragment` are
  errors, as in Nix. A virtual copy (`"${./.}"` without its context) stands
  for its local tree. The lock rule above holds: Nix locks a missing input in
  memory, ietsp falls back. Remote refs (`github:`, `git+https:`) and a
  fetched input's tree are `ErrorKind::Unsupported`.
- `--strict`, `--json` and `--raw` work as with a file. `--raw` mirrors
  `nix eval --raw`: the value is coerced to a string without copying paths
  to the store, and printed without a newline.

## Flake references

`src/flake/flakeref.rs` is the one parser: `builtins.parseFlakeRef`,
`builtins.flakeRefToString`, `builtins.getFlake`, the command line and the
unlocked-input message of `fetchTree` all use it. It follows
flakeref.cc:parseFlakeRefWithFragment (a flake id first, then a URL, then
a path) and the inputFromURL, inputFromAttrs and toURL of the github,
gitlab, git, path, tarball, file and indirect schemes (libfetchers). As in
Nix, `parseFlakeRef` has no base directory, so a relative path is an
error. The corners it leaves out are in docs/divergences.md.

## A remote root flake

The command line parses `github:NixOS/nixpkgs/<rev>#hello` but refuses it
as unsupported. The download itself exists (the archive fetch below), but
four parts do not:

1. A root has no lock entry, so no `narHash` names and checks the tree
   before the download. `fetch_tarball` would have to stage the archive,
   hash it, and only then name the input-store entry by the hash it got.
2. `sourceInfo.lastModified` of an archive comes from the archive
   (tarball.cc), and the spool index keeps no mtimes.
3. `FlakeRef` is a local directory: `eval_flake_value` reads `flake.lock`
   from disk, `root::source_info` copies a local tree, and the tracing
   cache key and the `ietsp.toml` walk (`src/api.rs`) read `fr.dir`. A
   root in the input store needs a third `Source` that reads through
   `store_hooks::locate`.
4. An unlocked ref (`github:o/r`, `github:o/r/branch`, a registry id)
   must first resolve to a rev: the forge API per scheme
   (github.cc:getRevFromRef), `tarball-ttl`, the flake registries. `git`
   refs need a git fetcher, which ietsp does not have.

1 to 3 make a locked `github:`, `gitlab:` or `tarball` root work; 4 is
what most command lines need.

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
   The copy is virtual: nothing is written or fetched. The store path is
   registered as a `SourceOrigin` whose `SrcPath` is the local directory
   with the tracked-file set, and every read under it, `import`, `readFile`,
   `readDir`, `pathExists`, `hashFile`, `"${./x}"`, `builtins.path`, goes
   to the working tree through `builtins::store_hooks::locate`, with the
   tracked-file set as filter: reading an untracked file fails with Nix's
   "Path '...' in the repository ... is not tracked by Git" error, while
   `pathExists` says `false` and `readDir` does not list it. The store is
   never consulted for the root tree.
2. `call-flake.nix` from Nix 2.34.8 is embedded verbatim and called with
   the lock file text, `{ root = { sourceInfo; dir; }; }` and
   `builtins.fetchTree`.
3. `builtins.fetchTree` on a locked node (`flake::fetch`) derives the store
   path from `narHash` and returns `emitTreeAttrs`: `outPath`, `narHash`,
   `rev`/`shortRev` when `rev` is present, `revCount`, `dirtyRev`/
   `dirtyShortRev`, `lastModified`/`lastModifiedDate` (`%Y%m%d%H%M%S`
   UTC), `submodules` for `git`. A `path` input is NAR-hashed, checked
   against `narHash` when the lock has one, and is a virtual copy like the
   root. A relative `path:` input (`path:./sub`) never reaches `fetchTree`:
   `nix flake lock` writes it as `{ path = "./sub"; type = "path"; }` with no
   `narHash`, `lock::check` accepts that, and `call-flake.nix` resolves it
   under the parent's `outPath`. Unlocked remote inputs (no `narHash`) and
   URL arguments are errors.

The flake's `flake.nix` is imported under its store path, so positions,
`./x` path literals and `readDir` see exactly what Nix sees; the bytes come
from the store when the tree is there, else from the local tree.

## Getting a missing input readable

A remote input (github, git, tarball, ...) whose locked tree is not in
`/nix/store` must become readable first. `flake::fetch::ensure_readable` takes
four steps in order; from the second on it prints
`ietsp: input '<name>' is not in the store (<path>), fetching it`. The root tree
and `path:` inputs never reach it.

0. **The input store, ietsp's own.** `$XDG_CACHE_HOME/ietsp/inputs/` holds the
   spooled archive of every input ietsp has fetched, keyed by the store path
   the lock implies (`src/treestore`, "The input store" below). An entry that
   is there costs two `open` calls and no network, no daemon and no store.
1. **Substitution, as one daemon op.** `QueryValidPaths` with the substitute
   flag (`wopQueryValidPaths`, protocol >= 1.27) makes the daemon run
   `Store::substitutePaths` and answer with the paths it now has
   (`daemon.cc:performOp`). Nothing is evaluated and no derivation is built.
   Nix does the same before any fetcher runs, for every input a lock file
   holds: `lockfile.cc` marks such an input `__final`, and
   `fetchers.cc:getAccessorUnchecked` substitutes a final input's store path
   first. An expression cannot reach that path, because `builtins.fetchTree`
   rejects `__final` (`fetchTree.cc:216`) and `fetchFinalTree` is an internal
   primop, absent from `builtins` (`eval.cc:546`). So ietsp substitutes the
   tree itself. A local copy beats a download, so this stays the first step
   whenever a daemon answers; when none does, the step is skipped without a
   word.
2. **The archive types, fetched by ietsp.** `github`, `gitlab`, `sourcehut`
   and `tarball` are one download and one index, so ietsp does them itself,
   into the input store.
3. **`git` and `hg`, the last shell-out.**
   `nix eval --raw --impure --expr '(builtins.fetchTree <locked attrs>).outPath'`,
   then a check that the path nix reports is the one the lock implies, so a
   difference in the file set is an error, not a silent divergence. These two
   types need a repository, which ietsp cannot clone yet. When no nix other
   than ietsp itself is on `PATH`, the error is
   `ietsp: input '<name>' is not in the store and nix is not on PATH; fetch it with nix (...) first`.

Step 1 is what makes ietsp agree with `nix eval` on a locked `git` input
that a substituter holds but the fetcher cannot reach: `git.cc:884`
(`Cannot find Git revision '<rev>' in ref '<ref>'`) is raised whenever the
fetch of the rev fails and a stale local ref lets `git.cc:866` swallow the
failure; the flake path never gets there, because the tree comes from the
substituter. The connection it uses goes to the eval store (nix.conf's
`store`, never the `builder` setting: a remote builder has no say in what
`import` reads), is opened on the first input that is missing and is reused
for the rest of the evaluation, the registration in step 2 included. A
`grpc://` store is not a worker-protocol daemon, so there is nothing to
substitute with and step 2 decides. The op costs 0.002 s against 0.13 s for
the warm `nix build --no-link` it replaced (0.49 s cold), plus 0.05 s once
for the connection.

### The archive fetch (step 2)

`src/fetch/http.rs` is the GET: hyper over tokio-rustls, the stack tonic
already brings, no libcurl. It follows redirects as `filetransfer.cc` does
(`CURLOPT_FOLLOWLOCATION`, `CURLOPT_MAXREDIRS` 10), reads a `file://` URL
from disk, and undoes the four compressions libarchive autodetects for us:
gzip, xz, zstd, bzip2 (`tarfile.cc:TarArchive`). The URL and headers are the
ones `github.cc:getDownloadUrl` and `tarball.cc` build, per type:

| type | URL |
|---|---|
| `github`, no token | `https://<host>/<owner>/<repo>/archive/<rev>.tar.gz` |
| `github`, with a token | `https://api.<host>/repos/<owner>/<repo>/tarball/<rev>` |
| `github`, `host` != github.com | `https://<host>/api/v3/repos/<owner>/<repo>/tarball/<rev>` |
| `gitlab` | `https://<host>/api/v4/projects/<owner>%2F<repo>/repository/archive.tar.gz?sha=<rev>` |
| `sourcehut` | `https://<host>/<owner>/<repo>/archive/<rev>.tar.gz` |
| `tarball` | the lock's `url`, verbatim |

The token comes from nix.conf's `access-tokens`, matched as
`github.cc:getAccessToken` matches it: the longest key that is a prefix of
`<host>/<owner>/<repo>` and ends on a path segment, else the entry for the
host alone. Its header is the one each forge wants
(`github.cc:accessHeaderFromToken`): `Authorization: token T` for github,
`Authorization: Bearer T` for sourcehut, and for gitlab `OAuth2:T` ->
`Authorization: Bearer T`, `PAT:T` -> `Private-token: T`, anything else
split on the first colon as a header of its own.

The download is streamed. hyper runs on a thread of its own and hands its
body frames to a queue of 16, the fetching thread reads that queue through the
decompressor, and the decompressed tar goes into `spool` inside a staged
directory of the input store. `src/fetch/spool.rs` then reads that file once
to record where every member's bytes are, and writes the NAR straight out of
the index, reading each file's bytes back by offset: no tree is ever unpacked.
It writes the NAR once, into the hasher, and the staged directory only becomes
an entry if that hash is the lock's, so a wrong tree is never named, never read
and never uploaded. Peak memory is therefore the queue plus the index, a few
hundred bytes per member and nothing per byte: a full nixpkgs revision costs
19 MB of index over the evaluator's own 75 MB, where unpacking a tree cost
307 MB (13.5 s and 382 MB peak RSS before, 7.7 s and 94 MB after).

`src/fetch/spool.rs` applies the rules of `src/fetch/unpack.rs`, which walks
the tar as `tarfile.cc:unpackTarfileToSink`
does and keeps exactly what the git tree behind a locked input keeps
(`git-utils.cc:GitFileSystemObjectSinkImpl`), because that tree is what the
lock's `narHash` hashes (`tarball.cc` calls `dereferenceSingletonDirectory`
and then `treeHashToNarHash`, which is `hashPath` of the tree):

- directories, empty ones included: the sink writes an empty tree object for
  them, so they survive into the NAR.
- regular files with one bit of mode, `archive_entry_mode() & S_IXUSR`
  (`tarfile.cc:226`); everything else about the mode is dropped.
- symlinks, as the target string.
- hard links, resolved to a second entry with the target's blob and mode
  (`git-utils.cc:1348-1364`).
- the last copy of a member listed twice (`git-utils.cc:1213`, "only import
  the latest version of a path").
- one leading directory stripped, but only when it is the only entry at the
  top (`git-utils.cc:dereferenceSingletonDirectory`).
- the pax global header GitHub's tarballs start with is archive metadata,
  not a member.
- any other member type is an error (`tarfile.cc:253`), and so is a `..` in
  a member name or a member under a symlink
  (`ARCHIVE_EXTRACT_SECURE_NODOTDOT`, `ARCHIVE_EXTRACT_SECURE_SYMLINKS`).

The NAR hash of the result is then compared with the lock's `narHash`. A
mismatch names both hashes and keeps nothing: the staged directory goes, so a
wrong tree is never named, never read and never uploaded. A match is moved into
the input store with one `rename`, and the evaluation reads the input out of
that spool. Nothing is registered in any store.
`spool_matches_the_tree` holds both readers to the same NAR bytes, member for
member.

`file` is not in this set. Its store path is the flat hash of the bytes under
the input's name (`tarball.cc:FileInputScheme`), which the lock's `narHash`
does not give, so ietsp cannot say where the file belongs before it has it;
the type stays unsupported.

`IETSP_OFFLINE=1` makes any download an error that names the URL. A flake whose
inputs the input store already holds still evaluates, which is what the
second-run assertions in `scripts/flaketest.nu` use.

## The input store

`$XDG_CACHE_HOME/ietsp/inputs/` (`--cache-dir DIR` or `IETSP_CACHE_DIR` moves it
to `DIR/inputs`) holds one directory per fetched input, named by the store path
base name the lock implies. Code: `src/treestore`, a local content-addressed
tree store on `crate::cachefs`. Its `put` takes the hash the tree must have
and has no other form, so an entry's name is always what its content hashes
to: that is why no reader re-hashes, why two processes may share an entry,
and why a user-space store needs no signature and no daemon.

| file | what |
|---|---|
| `spool` | the decompressed tar, exactly as it was downloaded |
| `index` | the `spool::Tree` index: a version byte, the entry's NAR hash and size, the spool's length, then the tree as offsets, sizes, modes and symlink targets |
| `sparse` | only when the archive had GNU sparse members, whose holes are not in the tar |

The name is the content address of the tree, so an entry is safe to share
between processes and safe to trust: the NAR hash was checked against the lock
before the entry was named, and a reader never hashes it again. What a reader
does check on open is the index's version byte and the spool's size against the
length the index recorded; anything else wipes the entry, which costs one fetch.
The store-level rules are the ones the two caches follow (`docs/caches.md`): a
`version` file, one writer at a time through the store's `flock`, an atomic
`rename` of a staged directory into place, and a TTL sweep.

**Reads.** `src/builtins/store_hooks.rs:locate` is the one place that maps a
store path to its bytes. It answers a `SrcPath` (`src/store/source.rs`, Nix's
`SourcePath`): a path in a `Src`, which is `Fs` (the real file system, or the
local tree a virtual copy stands for, with its filter), `Spool` (an entry of
the input store, read by `pread` at the offset the index holds, with the
filter of a `builtins.path` or `filterSource` copy of a member) or `Mem` (a
`toFile` text or a `.drv` this evaluation holds in memory). Its methods
(`stat`, `read`, `read_dir`, `dump`) are the only readers, and each records
its own read-set observation. `import`, `readFile`,
`readDir`, `pathExists`, `readFileType`, `hashFile`, `findFile`, `storePath`,
`fetchTree { type = "path"; }`, `builtins.path`, `filterSource`, `"${path}"`
and the `sourceInfo` attrs all go through it, so a
fetched input needs no `/nix/store` entry and no daemon. `/nix/store` is checked
first and wins when it has the tree; an input that appears there later, because
a build registered it, changes nothing, since the two hold the same bytes by
construction. Symlinks inside an entry are followed the way the kernel follows
them; a target that leaves the tree is not resolved, because the spool holds no
bytes for it.

**Uploads.** Nothing is registered during evaluation. A source whose origin is an
input-store entry keeps its `Src::Spool` name, which the tracing cache stores
as is, with the NAR hash and size the index already holds; `remote::plan`
opens the entry only when it dumps the NAR, and writes it out of the spool
straight into whichever builder or store the build targets
(`docs/remote-build.md`). `ietsp build --no-build` on a flake with a fetched
input therefore uploads that input from the spool and never writes the local
daemon.

**What it costs.** A flake whose only input is
`github:NixOS/nixpkgs/e554fab`, evaluated with `--no-cache` and a warm page
cache, five runs, the `eval:` line of `--timings` (so process start is out),
2026-09-19, load average 2:

| `#attr` | from the input store | from `/nix/store` |
|---|---|---|
| `nixpkgs.lib.version` | 0.271 s (0.263-0.279) | 0.238 s (0.200-0.242) |
| `(import nixpkgs {}).hello.drvPath` | 0.573 s (0.497-0.661) | 0.619 s (0.495-0.660) |

Reading a member is not the cost: it is one `pread` into the buffer the caller
keeps, against an `open`, an `fstat` and a growing `Vec` for a file under
`/nix/store`, and the heavier evaluation is 7 % faster out of the spool. What
the input store does charge is opening the entry: 1.7 MB of index for a full
nixpkgs, read and decoded in 11 ms, which `--timings` prints as
`inputs: 1 entries opened, 0.011s`. That fixed cost is 4 % of the trivial
evaluation and 2 % of the heavy one; the rest of the 0.033 s gap on
`lib.version` is inside the run-to-run spread of either column.

**What still needs a daemon.** `git` and `hg` inputs go through
`nix eval (builtins.fetchTree ...)`, which writes `/nix/store`; ietsp has no git
fetcher. `builtins.storePath` on a path that is not on disk hands the path to
the daemon to substitute; there is no entry for it, because its content address
is not a flake `narHash`. Substitution of a locked input is an optimisation and
is skipped when no daemon answers.

## Tracing cache

Everything above is recorded (`docs/tracing-cache.md`): the git queries as
`Cmd` observations, the working-tree file set as a filtered `Tree` (no
`seen_hash`: unlisted entries cannot change the result), every file read
under the root as a `File` on the local path, the lock file as a `File`, a
remote input that `/nix/store` holds as a `Stat`, and one that the input store
serves as a single `Input`, whatever was read out of it. The key holds the
reference text, the resolved directory, cwd, and the hashes of `flake.nix`
and `flake.lock`. A commit, an edit of a tracked file, `git add`, a
garbage-collected input, or a swept input-store entry all miss; an untracked
file does not.

## Measurements

A NixOS flake with 31 locked inputs and a devShell, `devShells.x86_64-linux.default.drvPath`,
2026-09-16, same machine as `docs/tracing-cache.md`:

| run | wall |
|---|---|
| `nix eval --raw --option eval-cache false` | 4.28 s |
| `ietsp eval --raw --no-cache` | 0.49 s |
| `ietsp eval --raw`, cache miss (records and writes 2,535 entries) | 0.56 s |
| `ietsp eval --raw`, cache hit | 0.02 s |
| `IETSP_WORKERS=8 ietsp eval --raw --no-cache` | 0.62 s |

The same flake, a NixOS system configuration (`nixosConfigurations.<host>.config.system.build.toplevel.drvPath`):
nix 7.21 s, ietsp 1.27 s uncached.

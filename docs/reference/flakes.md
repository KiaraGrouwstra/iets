# Flakes

What a flake reference may be, what `builtins.getFlake` accepts, how a
locked input becomes readable, the URL each forge is asked for, and how
`access-tokens` is matched.

Code: `src/flake/` (flake evaluation) over `src/source/` (a reference
becomes a tree), reached through `crate::api::flake`. Judge:
`scripts/flaketest.nu`. Why it is built this way:
`docs/explanation/flakes-and-sources.md`.

## Scope

`ietsp eval --raw DIR#attr.path` evaluates a locked flake and prints what
`nix eval --raw --option eval-cache false DIR#attr.path` prints.

- `DIR` goes through the parser `builtins.parseFlakeRef` uses, with cwd as
  the base directory. **Only a local path is taken**: `.`, `./x`, `/abs`,
  `~/x`, or a `path:` URL (`path:/abs`, `path:./x`, percent-escapes
  decoded). The fragment is percent-decoded, as in Nix.
- These are `ErrorKind::Unsupported`, so classic mode falls back on them:
  - registry references (`nixpkgs#hello`, and `x#y` even where `./x`
    exists, as in flakeref.cc:parseFlakeIdRef);
  - remote references (`github:`, `gitlab:`, `git+https:`, `git+file:`,
    `https://….tar.gz`): ietsp cannot fetch a root flake;
  - `?query` parameters on a path (`?dir=`, `?rev=`, `?narHash=`);
  - what the parser itself leaves out (`hg+…`, `sourcehut:` — see
    `docs/reference/divergences.md`).

  Some invalid references are accepted, and an unknown scheme
  (`foo:bar#x`) is unsupported. Both lists are in
  `docs/reference/divergences.md`.
- `#attr.path` selects from the flake's outputs; elements may be quoted
  like Nix (`"x86_64-linux"`). No `#`, or a bare `#`, gives the outputs
  attribute set. There is no `packages.<system>.` defaulting.
- `flake.lock` must exist and be complete: every node reachable from the
  root has a `narHash`, or is a relative `path:` input, and every input
  `flake.nix` declares has a lock entry. Otherwise:
  `ietsp: flake.lock missing or incomplete, run nix flake lock`. A
  `flake.nix` input whose URL changed without re-locking is not detected;
  the lock wins.
- `--strict`, `--json` and `--raw` work as with a file. `--raw` mirrors
  `nix eval --raw`: the value is coerced to a string without copying paths
  to the store, and printed without a newline.

## Flake references

`src/flake/flakeref.rs` is the one parser.
`builtins.parseFlakeRef`, `builtins.flakeRefToString`,
`builtins.getFlake`, the command line and the unlocked-input message of
`fetchTree` all use it. It follows
flakeref.cc:parseFlakeRefWithFragment — a flake id first, then a URL,
then a path — and the `inputFromURL`, `inputFromAttrs` and `toURL` of
these libfetchers schemes:

| Scheme | Parsed and rendered |
|:---|:---|
| indirect id (`nixpkgs`, `nixpkgs/nixos-24.05`) | yes |
| `flake:` | yes |
| `github:` | yes |
| `gitlab:` | yes |
| `path:`, an absolute path | yes |
| `git:`, `git+…` | yes |
| `tarball+…` | yes |
| `file+…`, plain `http(s)` and `file` URLs | yes |
| `sourcehut:` | no: `ErrorKind::Unsupported` |
| `hg+…`, any other scheme or type | no: `ErrorKind::Unsupported` |

Each scheme keeps the query parameters Nix keeps for it. As in Nix,
`parseFlakeRef` has no base directory, so a relative path is an error.
For valid input the output is Nix's; the corners where ietsp is looser or
stricter are in `docs/reference/divergences.md`.

## `builtins.getFlake`

`builtins.getFlake REF` takes an absolute `/abs` or `path:/abs`, parsed
like `parseFlakeRef`, and returns what `callFlake` returns.

- It is **impure only**: every reference ietsp resolves is unlocked, which
  pure evaluation refuses (`docs/reference/settings.md`, "pure-eval").
- `REF` is always a `path:` input, also inside a git repository, because
  flakeref.cc looks for `.git` only when it has a base directory and
  `getFlake` passes none.
- A relative `REF` and a `#fragment` are errors, as in Nix.
- A virtual copy (`"${./.}"` without its context) stands for its local
  tree.
- The lock rule above holds. Nix locks a missing input in memory; ietsp
  falls back.
- Remote references (`github:`, `git+https:`) and a fetched input's tree
  are `ErrorKind::Unsupported`.

## Getting a locked input readable

A remote input whose locked tree is not in `/nix/store` must become
readable first. `source::tree` takes four steps, in order; from the second
on it prints `ietsp: input '<name>' is not in the store (<path>),
fetching it`. The root tree and `path:` inputs never reach it.

0. **The input store**, ietsp's own (below). An entry that is there costs
   two `open` calls and no network, no daemon and no store.
1. **Substitution, as one daemon op.** `QueryValidPaths` with the
   substitute flag (`wopQueryValidPaths`, protocol >= 1.27) makes the
   daemon run `Store::substitutePaths` and answer with the paths it now
   has. Nothing is evaluated and no derivation is built. The connection
   goes to the eval store — nix.conf's `store`, never the `builder`
   setting — is opened on the first missing input, and is reused for the
   rest of the evaluation. A `grpc://` store is not a worker-protocol
   daemon, so there is nothing to substitute with and step 2 decides.
   When no daemon answers, the step is skipped without a word.
2. **The archive types, fetched by ietsp**: `github`, `gitlab`,
   `sourcehut` and `tarball` are one download and one index, so ietsp does
   them itself, into the input store.
3. **`git` and `hg`, the last shell-out**:
   `nix eval --raw --impure --expr '(builtins.fetchTree <locked
   attrs>).outPath'`, then a check that the path nix reports is the one
   the lock implies, so a difference in the file set is an error and not a
   silent divergence. When no nix other than ietsp itself is on `PATH`,
   the error is `ietsp: input '<name>' is not in the store and nix is not
   on PATH; fetch it with nix (...) first`.

`file` is not in this set. Its store path is the flat hash of the bytes
under the input's name (`tarball.cc:FileInputScheme`), which the lock's
`narHash` does not give, so the type stays unsupported.

`IETSP_OFFLINE=1` makes any download an error that names the URL. A flake
whose inputs the input store already holds still evaluates.

### The archive URL, per type

`src/fetch/http.rs` is the GET: hyper over tokio-rustls, no libcurl. It
follows redirects as `filetransfer.cc` does (`CURLOPT_FOLLOWLOCATION`,
`CURLOPT_MAXREDIRS` 10), retries a transient status
(`docs/reference/settings.md`), sends a `user:password` in the URL as
Basic credentials, reads `file:/p`, `file:///p` and `file://localhost/p`
from disk, and undoes the
four compressions libarchive autodetects: gzip, xz, zstd, bzip2
(`tarfile.cc:TarArchive`). The URL is the one `github.cc:getDownloadUrl`
and `tarball.cc` build:

| type | URL |
|---|---|
| `github`, no token | `https://<host>/<owner>/<repo>/archive/<rev>.tar.gz` |
| `github`, with a token | `https://api.<host>/repos/<owner>/<repo>/tarball/<rev>` |
| `github`, `host` != github.com | `https://<host>/api/v3/repos/<owner>/<repo>/tarball/<rev>` |
| `gitlab` | `https://<host>/api/v4/projects/<owner>%2F<repo>/repository/archive.tar.gz?sha=<rev>` |
| `sourcehut` | `https://<host>/<owner>/<repo>/archive/<rev>.tar.gz` |
| `tarball` | the lock's `url`, verbatim |

### `access-tokens`

The token comes from nix.conf's `access-tokens`, matched as
`github.cc:getAccessToken` matches it: **the longest key that is a prefix
of `<host>/<owner>/<repo>` and ends on a path segment**, else the entry
for the host alone.

Its header is the one each forge wants
(`github.cc:accessHeaderFromToken`):

| Forge | Token value | Header |
|:---|:---|:---|
| github | `T` | `Authorization: token T` |
| sourcehut | `T` | `Authorization: Bearer T` |
| gitlab | `OAuth2:T` | `Authorization: Bearer T` |
| gitlab | `PAT:T` | `Private-token: T` |
| gitlab | anything else | split on the first colon, as a header of its own |

### What the tree keeps

`src/fetch/spool.rs` applies the rules of `src/fetch/unpack.rs`, which
walks the tar as `tarfile.cc:unpackTarfileToSink` does and keeps exactly
what the git tree behind a locked input keeps
(`git-utils.cc:GitFileSystemObjectSinkImpl`), because that tree is what
the lock's `narHash` hashes:

- directories, empty ones included: the sink writes an empty tree object
  for them, so they survive into the NAR;
- regular files with one bit of mode, `archive_entry_mode() & S_IXUSR`
  (`tarfile.cc:226`); everything else about the mode is dropped;
- symlinks, as the target string;
- hard links, resolved to a second entry with the target's blob and mode
  (`git-utils.cc:1348-1364`);
- the last copy of a member listed twice (`git-utils.cc:1213`);
- one leading directory stripped, but only when it is the only entry at
  the top (`git-utils.cc:dereferenceSingletonDirectory`);
- the pax global header GitHub's tarballs start with is archive metadata,
  not a member.

A member name is a `CanonPath`: any root is stripped and a `..` removes
the component before it, with nothing to remove at the top, so no member
names anything outside the tree. A member below a symlink replaces that
symlink with a directory; nothing is ever written through it. A device, a
socket and a fifo are an error (`tarfile.cc:253`); every other type flag
is a regular file, as `archive_entry_filetype` gives it. These three
match the git sink behind a locked input. `nix store prefetch-file
--unpack` is Nix's other archive path, with libarchive's extract flags,
and refuses all three.

Two limits are ietsp's own. `fetch-max-size` covers the spool and
everything a sparse member expands to together
(`docs/reference/settings.md`). A stream with over eight layers of
compression is refused, where libarchive stacks filters without a bound.

The NAR hash of the result is compared with the lock's `narHash`. A
mismatch names both hashes and keeps nothing: the staged directory goes,
so a wrong tree is never named, never read and never uploaded. A match is
moved into the input store with one `rename`. Nothing is registered in any
store.

This is also the tree `ietsp prefetch --unpack` hashes
(`docs/reference/commands.md`).

## The input store

`$XDG_CACHE_HOME/ietsp/inputs/` — `--cache-dir DIR` or `IETSP_CACHE_DIR`
moves it to `DIR/inputs` — holds one directory per fetched input, named by
the store-path base name the lock implies. Code: `src/treestore`, a local
content-addressed tree store on `crate::cachefs`. Its `put` takes the hash
the tree must have and has no other form, so an entry's name is always
what its content hashes to.

| file | what |
|---|---|
| `spool` | the decompressed tar, exactly as it was downloaded |
| `index` | the `spool::Tree` index: a version byte, the entry's NAR hash and size, the spool's length, then the tree as offsets, sizes, modes and symlink targets |
| `sparse` | only when the archive had sparse members (GNU or pax), whose holes are not in the tar |

A reader checks the index's version byte and the spool's size against the
length the index recorded; anything else wipes the entry, which costs one
fetch. The store-level rules are the ones the two caches follow
(`docs/explanation/caching.md`): a `version` file, one writer at a time
through the store's `flock`, an atomic `rename` of a staged directory into
place, and a TTL sweep.

**Reads.** `src/builtins/store_hooks.rs:locate` is the one place that maps
a store path to its bytes. It answers a `SrcPath` (`src/store/source.rs`,
Nix's `SourcePath`): a path in a `Src`, which is one of

| `Src` | What it reads |
|:---|:---|
| `Fs` | the real file system, or the local tree a virtual copy stands for, with its filter |
| `Spool` | an entry of the input store, read by `pread` at the offset the index holds, with the filter of a `builtins.path` or `filterSource` copy of a member |
| `Mem` | a `toFile` text, or a `.drv` this evaluation holds in memory |

Its methods (`stat`, `read`, `read_dir`, `dump`) are the only readers, and
each records its own read-set observation. `import`, `readFile`,
`readDir`, `pathExists`, `readFileType`, `hashFile`, `findFile`,
`storePath`, `fetchTree { type = "path"; }`, `builtins.path`,
`filterSource`, `"${path}"` and the `sourceInfo` attributes all go through
it, so a fetched input needs no `/nix/store` entry and no daemon.
`/nix/store` is checked first and wins when it has the tree. Symlinks
inside an entry are followed the way the kernel follows them; a target
that leaves the tree is not resolved, because the spool holds no bytes for
it.

**Uploads.** Nothing is registered during evaluation. A source whose
origin is an input-store entry keeps its `Src::Spool` name, which the
tracing cache stores as is, with the NAR hash and size the index already
holds; `remote::plan` opens the entry only when it dumps the NAR, and
writes it out of the spool straight into whichever builder the build
targets (`docs/reference/builders.md`).

**What still needs a daemon.** `git` and `hg` inputs go through
`nix eval (builtins.fetchTree ...)`, which writes `/nix/store`.
`builtins.storePath` on a path that is not on disk hands the path to the
daemon to substitute; there is no entry for it, because its content
address is not a flake `narHash`. Substitution of a locked input is an
optimisation and is skipped when no daemon answers.

## What `fetchTree` returns

`builtins.fetchTree` on a locked node derives the store path from
`narHash` and returns `emitTreeAttrs`: `outPath`, `narHash`, `rev` and
`shortRev` when `rev` is present, `revCount`, `dirtyRev` and
`dirtyShortRev`, `lastModified` and `lastModifiedDate` (`%Y%m%d%H%M%S`,
UTC), and `submodules` for `git`.

`revCount` is deprecated (`docs/reference/lints.md`, `rev-count`). Its
value is that of Nix.

A `path` input is NAR-hashed, checked against `narHash` when the lock has
one, and is a virtual copy like the root. A relative `path:` input
(`path:./sub`) never reaches `fetchTree`: `nix flake lock` writes it as
`{ path = "./sub"; type = "path"; }` with no `narHash`, `lock::check`
accepts that, and `call-flake.nix` resolves it under the parent's
`outPath`. Unlocked remote inputs (no `narHash`) and URL arguments are
errors.

## The root tree

The root's `sourceInfo` is built like Nix's fetcher for the reference the
CLI would produce (flakeref.cc): a directory inside a git working tree is
`git+file://<root>?dir=<subdir>`, anything else is `path:`. The interface
`source::scm::Scm` supplies the working tree, and git is the one source
control system in its list (`docs/explanation/flakes-and-sources.md`).

| Source | What the tree is, and its metadata |
|:---|:---|
| git (git.cc:getAccessorFromWorkdir, git-utils.cc:getWorkdirInfo) | the set of index entries that still exist in the working tree, with the working-tree contents and modes; untracked files, `.git` and submodules excluded. `git ls-files --stage` and `git status --porcelain -uno` give the set and the dirty flag, `rev-parse HEAD` and `log -1 --format=%ct` the revision and its date. `rev-list --count` runs when the evaluation reads `revCount`, and the tracing store keeps the count by revision. Clean: `rev`, `shortRev`, `revCount` (not when `.git/shallow` exists; deprecated, lint `rev-count`). Dirty: `dirtyRev`, `dirtyShortRev`. `submodules = false` always; `lastModified` is the HEAD commit time |
| path (path.cc) | the whole directory; `lastModified` is the newest `lstat` mtime in it |

The store path is `make_fixed_output_path(recursive, narHash, "source")`.
The copy is virtual: nothing is written or fetched. Every read under it
goes to the working tree through `builtins::store_hooks::locate`, with the
tracked-file set as filter: reading an untracked file fails with Nix's
`Path '...' in the repository ... is not tracked by Git` error, while
`pathExists` says `false` and `readDir` does not list it.

`call-flake.nix` from Nix 2.34.8 is embedded verbatim and called with the
lock file text, `{ root = { sourceInfo; dir; }; }` and
`builtins.fetchTree`.

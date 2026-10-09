# Flakes

Which flake references iets takes, what `builtins.getFlake` accepts, how a
locked input becomes readable, which URL each forge is asked for, and how
`access-tokens` is matched.

For the reasons behind all this, see
`docs/explanation/flakes-and-sources.md`.

## Scope

You can evaluate a locked flake that lives on your disk:

```sh
iets eval --raw .#packages.x86_64-linux.hello.name
```

This prints what
`nix eval --raw --option eval-cache false .#packages.x86_64-linux.hello.name`
prints.

- **The reference must be a local path**: `.`, `./x`, `/abs`, `~/x`, or a
  `path:` URL (`path:/abs`, `path:./x`, percent-escapes decoded). It is
  parsed as `builtins.parseFlakeRef` parses it, with the current directory
  as the base. The fragment is percent-decoded, as in Nix.
- iets does not take these, so classic mode falls back to Nix on them:
  - registry references (`nixpkgs#hello`, and `x#y` even where `./x`
    exists, as in Nix);
  - remote references (`github:`, `gitlab:`, `git+https:`, `git+file:`,
    `https://….tar.gz`): iets cannot fetch a root flake;
  - `?query` parameters on a path (`?dir=`, `?rev=`, `?narHash=`);
  - the schemes the parser does not take (`hg+…`, `sourcehut:`).

  Some invalid references are accepted, and an unknown scheme
  (`foo:bar#x`) is unsupported. `docs/reference/divergences.md` lists
  both.
- `#attr.path` selects from the flake's outputs. You can quote an element
  as in Nix (`"x86_64-linux"`). No `#`, or a bare `#`, gives you the whole
  outputs attribute set. iets does not fill in `packages.<system>.` for
  you.
- `flake.lock` must exist and be complete: every node reachable from the
  root has a `narHash` or is a relative `path:` input, and every input
  `flake.nix` declares has a lock entry. If not, you get
  `iets: flake.lock missing or incomplete, run nix flake lock`. If you
  change an input's URL in `flake.nix` without locking again, iets does
  not notice: the lock wins. `--reference-lock-file PATH` reads the root
  flake's lock from `PATH` instead, under the same rule.
- `--strict`, `--json` and `--raw` work as they do with a file. `--raw`
  behaves like `nix eval --raw`: the value becomes a string without copying
  paths to the store, and is printed without a newline.

## Flake references

The command line, `builtins.parseFlakeRef`, `builtins.flakeRefToString`,
`builtins.getFlake` and the unlocked-input message of `fetchTree` all use
the same parser. Like Nix's, it tries a flake id first, then a URL, then a
path.

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
| `sourcehut:` | no: unsupported |
| `hg+…`, any other scheme or type | no: unsupported |

Each scheme keeps the query parameters Nix keeps for it. As in Nix,
`parseFlakeRef` has no base directory, so a relative path is an error. For
valid input you get Nix's output. The few places where iets is looser or
stricter are in `docs/reference/divergences.md`.

## `builtins.getFlake`

`builtins.getFlake REF` takes an absolute path, `/abs` or `path:/abs`, and
returns what `callFlake` returns.

```nix
(builtins.getFlake "/home/you/project").outputs
```

- It works **only in impure evaluation**. Every reference iets resolves is
  unlocked, and pure evaluation refuses that
  (`docs/reference/settings.md`, "pure-eval").
- `REF` is always a `path:` input, even inside a git repository. Nix looks
  for `.git` only when it has a base directory, and `getFlake` gives it
  none.
- A relative `REF` and a `#fragment` are errors, as in Nix.
- A virtual copy (`"${./.}"` without its context) stands for its local
  tree.
- The lock rule above holds. Nix locks a missing input in memory; iets
  falls back to Nix.
- Remote references (`github:`, `git+https:`) and the tree of a fetched
  input are unsupported.

## Getting a locked input readable

A remote input whose locked tree is not in `/nix/store` has to become
readable first. iets tries four things, in order. From the second on, it
tells you: `iets: input '<name>' is not in the store (<path>), fetching it`.
The root tree and `path:` inputs never need this.

0. **The input store**, iets's own cache (below). An input that is already
   there costs two `open` calls: no network, no daemon, no store.
1. **Substitution.** iets asks the daemon to substitute the paths, in one
   request (`QueryValidPaths` with the substitute flag, protocol >= 1.27).
   Nothing is evaluated and nothing is built. iets asks the eval store
   (nix.conf's `store`, never the `builder` setting). It connects on the
   first missing input and keeps the connection for the rest of the
   evaluation. A `grpc://` store is not a worker-protocol daemon, so it
   cannot substitute, and step 2 decides. When no daemon answers, iets
   skips this step without a word.
2. **iets downloads it.** `github`, `gitlab`, `sourcehut` and `tarball`
   inputs are one download each, so iets fetches them itself, into the
   input store.
3. **iets asks Nix.** For `git` and `hg`, iets runs
   `nix eval --raw --impure --expr '(builtins.fetchTree <locked
   attrs>).outPath'`. Then it checks that the path Nix reports is the one
   the lock implies, so a different file set is an error, never a silent
   difference. If no `nix` other than iets itself is on `PATH`, you get
   `iets: input '<name>' is not in the store and nix is not on PATH; fetch
   it with nix (...) first`.

`file` inputs are not supported. Their store path is the flat hash of the
bytes, which the lock's `narHash` does not give.

With `IETS_OFFLINE=1`, any download is an error that names the URL. A flake
whose inputs are all in the input store still evaluates.

### The archive URL, per type

iets downloads with its own HTTP client. Like Nix, it follows up to 10
redirects. It retries a transient status (`docs/reference/settings.md`),
sends a `user:password` in the URL as Basic credentials, reads `file:/p`,
`file:///p` and `file://localhost/p` from disk, and undoes the four
compressions Nix detects: gzip, xz, zstd and bzip2. The URL is the one Nix
builds:

| type | URL |
|---|---|
| `github`, no token | `https://<host>/<owner>/<repo>/archive/<rev>.tar.gz` |
| `github`, with a token | `https://api.<host>/repos/<owner>/<repo>/tarball/<rev>` |
| `github`, `host` != github.com | `https://<host>/api/v3/repos/<owner>/<repo>/tarball/<rev>` |
| `gitlab` | `https://<host>/api/v4/projects/<owner>%2F<repo>/repository/archive.tar.gz?sha=<rev>` |
| `sourcehut` | `https://<host>/<owner>/<repo>/archive/<rev>.tar.gz` |
| `tarball` | the lock's `url`, verbatim |

### `access-tokens`

The token comes from `access-tokens` in nix.conf, matched as Nix matches
it: **the longest key that is a prefix of `<host>/<owner>/<repo>` and ends
on a path segment**, else the entry for the host alone. For example, with
`access-tokens = github.com=A github.com/acme=B`, a `github:acme/app`
input gets `B`, and `github:other/app` gets `A`.

Each forge gets the header it wants:

| Forge | Token value | Header |
|:---|:---|:---|
| github | `T` | `Authorization: token T` |
| sourcehut | `T` | `Authorization: Bearer T` |
| gitlab | `OAuth2:T` | `Authorization: Bearer T` |
| gitlab | `PAT:T` | `Private-token: T` |
| gitlab | anything else | split on the first colon, as a header of its own |

### What the tree keeps

iets unpacks an archive the way Nix does. It keeps exactly what the git
tree behind a locked input keeps, because that tree is what the lock's
`narHash` hashes:

- directories, empty ones included;
- regular files with one bit of mode, executable or not; the rest of the
  mode is dropped;
- symlinks, as the target string;
- hard links, as a second entry with the target's contents and mode;
- the last copy of a member listed twice;
- one leading directory stripped, but only when it is the only entry at
  the top;
- the pax global header that GitHub's tarballs start with is metadata, not
  a member.

Member names are cleaned up, so no member can name anything outside the
tree. Any root is stripped, and a `..` removes the component before it,
with nothing to remove at the top. A member below a symlink replaces that
symlink with a directory: iets never writes through a symlink. A device, a
socket and a fifo are errors; every other type flag is a regular file.
These three rules match the git tree behind a locked input. Nix's other
archive path, `nix store prefetch-file --unpack`, refuses all three.

Two limits are iets's own. `fetch-max-size` covers the download and
everything a sparse member expands to, together
(`docs/reference/settings.md`). A stream with more than eight layers of
compression is refused; Nix stacks filters without a bound.

Then iets compares the NAR hash of the result with the lock's `narHash`. A
mismatch names both hashes and keeps nothing, so a wrong tree is never
read and never uploaded. A match goes into the input store. Nothing is
registered in any store.

`iets prefetch --unpack` hashes the same tree
(`docs/reference/commands.md`).

## The input store

The input store is where iets keeps the inputs it fetched. It lives in
`$XDG_CACHE_HOME/iets/inputs/`. `--cache-dir DIR` or `IETS_CACHE_DIR`
moves it to `DIR/inputs`.

| path | what |
|---|---|
| `<base name>` | one input: its NAR hash and size, and its file tree; the name is the store-path base name the lock implies |
| `packs/<hex>.pack` | the contents of the files, each stored once, compressed when that helps; a pack is written once and never changed |

A file that two inputs share is stored once, so a second revision of
nixpkgs costs only the files that changed. An entry is written only under
the hash of its content. A damaged entry or pack is wiped, and that costs
you one fetch. The input store follows the same rules as the other two
caches (`docs/explanation/caching.md`): a `version` file, one writer at a
time, an atomic rename into place, and a sweep of old entries, after which
the contents no entry uses are removed.

**Reads.** `import`, `readFile`, `readDir`, `pathExists`, `readFileType`,
`hashFile`, `findFile`, `storePath`, `fetchTree { type = "path"; }`,
`builtins.path`, `filterSource`, `"${path}"` and the `sourceInfo`
attributes read a fetched input straight from the input store. So a
fetched input needs no `/nix/store` entry and no daemon. iets checks
`/nix/store` first, and uses it when it has the tree. Symlinks inside an
entry are followed the way the kernel follows them. A target outside the
tree is not resolved, because the entry holds no bytes for it.

**Uploads.** Nothing is registered during evaluation. A build reads the
entry only when it sends the NAR, straight to whichever builder the build
targets (`docs/reference/builders.md`).

**What still needs a daemon.** `git` and `hg` inputs go through
`nix eval (builtins.fetchTree ...)`, which writes to `/nix/store`.
`builtins.storePath` on a path that is not on disk asks the daemon to
substitute it. It has no input store entry, because its content address is
not a flake `narHash`. Substituting a locked input only saves time, and
iets skips it when no daemon answers.

## What `fetchTree` returns

`builtins.fetchTree` on a locked node gets the store path from `narHash`.
It returns the attributes Nix returns:

- `outPath` and `narHash`;
- `rev` and `shortRev`, when `rev` is there;
- `revCount`;
- `dirtyRev` and `dirtyShortRev`;
- `lastModified` and `lastModifiedDate` (`%Y%m%d%H%M%S`, UTC);
- `submodules`, for `git`.

`revCount` has Nix's value, but it is deprecated
(`docs/reference/lints.md`, `rev-count`).

A `path` input is NAR-hashed, checked against `narHash` when the lock has
one, and is a virtual copy, like the root. A relative `path:` input
(`path:./sub`) never reaches `fetchTree`. `nix flake lock` writes it as
`{ path = "./sub"; type = "path"; }` with no `narHash`, iets accepts that,
and `call-flake.nix` finds it under the parent's `outPath`. An unlocked
remote input (no `narHash`) and a URL argument are errors.

## The root tree

The root's `sourceInfo` is what Nix's fetcher gives for the reference the
command line would make. A directory inside a git working tree is
`git+file://<root>?dir=<subdir>`. Anything else is `path:`. Git is the one
source control system iets reads (`docs/explanation/flakes-and-sources.md`).

| Source | What the tree is, and its metadata |
|:---|:---|
| git | the files in git's index that still exist in the working tree, with their working-tree contents and modes; untracked files, `.git` and submodules are left out. Clean: `rev`, `shortRev`, `revCount` (not in a shallow clone; deprecated, lint `rev-count`). Dirty: `dirtyRev`, `dirtyShortRev`. `submodules = false` always. `lastModified` is the time of the HEAD commit. iets counts `revCount` only when the evaluation reads it, and the tracing cache keeps the count per revision |
| path | the whole directory; `lastModified` is the newest `lstat` mtime in it |

The store path is the fixed-output path of the tree's NAR hash, named
`source`. The copy is virtual: nothing is written or fetched. Every read
under it goes to your working tree, and only the files git tracks are
there. Reading an untracked file fails with Nix's `Path '...' in the
repository ... is not tracked by Git` error, `pathExists` says `false`, and
`readDir` does not list it.

iets embeds `call-flake.nix` from Nix 2.34.8 as it is. It calls it with the
lock file text, `{ root = { sourceInfo; dir; }; }` and
`builtins.fetchTree`.

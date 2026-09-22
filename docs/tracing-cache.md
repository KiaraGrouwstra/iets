# Tracing evaluation cache

`ietsp eval` and `ietsp build` remember the result of a run together with
every observation of the outside world the run made. A later run with the
same command validates those observations against the file system and the
environment. If nothing changed, it replays the result and skips
evaluation. Code: `src/trace/`.

## Read set

Every primop or evaluator step that looks at the world records one
observation (`trace::Obs`) through the `Evaluator::trace` handle. The
handle is an `Arc` shared by every fiber (`parallel::shared` rule: shared
state clones by reference). Recording takes one mutex per observation; the
miss path records ~5k observations on nixos-desktop, so the lock is not
measurable next to the sha256 of the file bytes.

| observation | recorded by | validated by |
|---|---|---|
| `File { path, size, mtime_ns, ino, sha256 }` | `import`, `scopedImport`, `readFile`, `hashFile`, `import` of a `/nix/store/*.drv`, every `.drv` read for `hashDerivationModulo` | `lstat`; on a stat mismatch the file is rehashed and accepted if the hash matches (stat fields refreshed in the entry) |
| `Dir { path, sha256 of the sorted (name, type) listing }` | `readDir` | re-list and rehash |
| `Stat { path, follow, kind }` where kind is missing, regular, directory, symlink, other | `pathExists`, `readFileType`, the `default.nix` probe of `import`, every candidate of a `<name>` search-path lookup, store paths checked for existence | re-stat |
| `Tree { root, nar_hash, stat_hash, filtered: Option<{ seen_hash: Option, accepted }> }` | `./path` coerced to a string, `builtins.path`, `filterSource`, a flake's git working tree (`seen_hash` is `None` there: git names the file set, so unlisted entries cannot matter), a `path:` flake or input (unfiltered) | recompute `stat_hash` (a walk with `lstat` only, no contents); on a mismatch re-dump the NAR with the recorded `accepted` set as filter and accept if `nar_hash` (and `seen_hash`, when kept) match |
| `Canon { path, target }` | `storePath`, `builtins.path` and `filterSource` when `canonicalize(path) != path` | `canonicalize` again |
| `Env { name, value: Option<bytes> }` | `getEnv`; `HOME` when a `~/` path literal is compiled | `var_os` again |
| `Cmd { dir, argv, sha256 of exit status and stdout }` | the `git` queries of a flake's working tree (`flake::root`) | run again, rehash |

`stat_hash` covers, for every entry the walk saw: relative path, type,
size, mtime_ns, inode, and symlink target. `seen_hash` covers the (relative
path, type) pairs the filter was asked about, so a new entry the filter
would have accepted or rejected is a miss. For an unfiltered tree the
`stat_hash` already covers that.

`builtins.currentTime` and every network fetcher (`fetchurl`,
`fetchTarball` without a `sha256`, `fetchGit`, `fetchMercurial`,
`fetchClosure`, `getFlake`) mark the run uncacheable. `fetchTree` on a locked
input and `fetchTarball` with a `sha256` are cacheable: its
result follows from the attrs and a `Stat` of the store path (see
`docs/flakes.md`). `currentTime` is a lazy thunk over an internal
primop so the mark happens only when a program forces it. `--dump-drvs`
also disables the cache (it has a side effect on every miss).

## Key

The key is computed before evaluation from the command alone:

- build id: sha256 of every file under `src/`, `Cargo.lock` and
  `build.rs`, computed by `build.rs` and embedded as `IETSP_BUILD_ID`
  (`cargo:rerun-if-changed` on those paths keeps it current; the nix build
  stays reproducible);
- subcommand (`eval` or `build`);
- the expression text; the absolute file path plus the sha256 of its
  contents; or, for a flake reference, its text, the resolved flake
  directory and the sha256 of `flake.nix` and `flake.lock` (cwd is then
  always in the key);
- `--arg`/`--argstr` name and value pairs in order; `-A`; `--strict`;
  `--json`; `--raw`; the effective lint levels and knobs (docs/lints.md);
  experimental features;
- `-I` entries and `NIX_PATH` entries as absolute `(prefix, path)` pairs
  in resolution order;
- `builtins.currentSystem`;
- cwd, only when `-E` or an `--arg` expression holds a relative path
  literal (checked with the parser); a file argument is made absolute
  before it enters the key, so the key of `ietsp eval x.nix` does not
  depend on cwd.

`--workers`, `--builder`, `--out`, `--no-build`, `--timings` are not in
the key. `--impure` and `--pure-eval` are accepted and ignored by the
evaluator, so they do not enter the key either.

## Entry

`${IETSP_CACHE_DIR:-${XDG_CACHE_HOME:-~/.cache}/ietsp/trace}/<key hex>/`:

- `readset`: the observations, binary (tag byte, varint lengths, raw
  bytes), deduplicated;
- `result`: `eval`: the exact stdout bytes; `build`: the root drv paths
  and the wanted output named by `!out!drv` context;
- `drvs` (`build` only): every `DrvInfo` this evaluation produced (store
  path, outputs, references, ATerm);
- `sources` (`build` only): the `SourceOrigin` table (store path -> `Tree {
  local, accepted }` | `Text { bytes, refs }`);
- `warnings`, only when the run printed lint warnings: the lines, in
  order. A hit prints them to stderr again before the result.

Every file is written to a temporary name in the entry directory and
renamed into place. An entry is written only after a successful,
cacheable run; a new run with the same key overwrites. A hit that
refreshed stat fields rewrites `readset` only. `eval` entries omit `drvs`
and `sources`: the subcommand is part of the key, so nothing reads them
back for `eval`, and writing the 16 MB `drvs` of nixos-desktop cost 40 ms
(3 %) on every miss.

## Lookup

1. Compute the key. With `--no-cache` or `IETSP_CACHE=0` stop here.
2. Read `readset`. Missing or unreadable: miss.
3. Validate every observation on `min(4, cores)` threads (a
   `std::thread::scope` over a shared counter). Any mismatch: miss.
4. Hit for `eval`: write `result` to stdout, exit 0. Hit for `build`: load
   `drvs` and `sources`, build the `Plan` from them, run Have/Put/Build.
5. Miss: evaluate with recording on; on success and cacheable, write the
   entry.

`--timings` prints `cache: hit|miss, N entries validated, M files
rehashed, T s` on stderr.

## Semantics

- A hit prints the same stdout bytes as a miss.
- `builtins.trace` and `builtins.warn` write to stderr during evaluation.
  A hit does not evaluate, so it prints none of them. Run with
  `--no-cache` to see them again. Lint warnings are different: the entry
  records them and a hit replays them.
- Errors are never cached: an entry is written only after the result was
  printed (or the roots were resolved) without error.
- `--no-cache` and `IETSP_CACHE=0` disable lookup and write. `--cache-dir
  DIR` (or `IETSP_CACHE_DIR`) moves the store. Default: on for `eval` and
  `build`.
- The compiled-IR cache (`src/compile/cache.rs`, docs/memory.md change 5)
  is a separate store under `${IETSP_CACHE_DIR:-~/.cache/ietsp}/ir/`. It
  is keyed on file contents, not on the run, so `--no-cache` leaves it on;
  `--no-ir-cache` or `IETSP_CACHE=0` turns it off.
- Recorded paths are absolute. A relative path in `-E` puts cwd in the key.
- A file that changes while the evaluation runs may be recorded twice with
  two hashes; validation then fails on one of them, which is a miss, never
  a wrong hit.
- Helper fibers that get cancelled still leave their observations in the
  read set. That over-approximates the read set (more misses in rare
  cases) and never under-approximates it.

## Lifecycle and concurrency

Full rules and numbers: docs/caches.md. In short:

- Many processes share the store. Readers never lock. A writer takes the
  store's `flock` for exactly one file and gives up after 50 ms, which
  costs a later miss and never a stall. `readset` is written last, so a
  cut-short write leaves nothing to hit.
- A hit that refreshed nothing writes nothing: it touches the entry's
  mtime, and only when that mtime is over a day old.
- Entries unread for 30 days are swept. The sweep runs on a write path,
  at most once a day per store, never on the lookup path, and by hand
  through `ietsp cache gc [--ttl DAYS] [--dry-run]`. A swept entry misses
  next time. A cold sweep of 10,000 entries costs 0.03 s when nothing is
  past the TTL and 0.57 s when everything is.
- An entry that does not decode is deleted, never repaired. A store whose
  `version` or `gc-stamp` file cannot be read is wiped whole; `--timings`
  says so in one line.

## Virtual paths

Built, for the paths that need it: a fetched flake input is read out of the
input store, not out of `/nix/store` (docs/flakes.md, "The input store"). The
index over the downloaded tar names every member's offset, and
`store_hooks::locate` serves `import`, `readFile`, `readDir`, `pathExists`,
`readFileType`, `hashFile` and `builtins.path` from it, so evaluation waits for
no realisation and needs no daemon.

The read set records this as one `Input` observation per entry, not one per
file: the entry's name is the NAR hash of its tree, so a name that is still
there still holds the bytes the run read. Validation is therefore two `stat`
calls and a version-byte comparison for a whole nixpkgs, against the 3,500
`File` observations the same tree would cost if its files were stated one by
one. An entry the sweep removed, or one written by a newer index version, is a
miss; the next run fetches it again.

The design idea is nix-tarmac's: keep an index over content-addressed blobs and
serve an accessor from it. What is still not built is the same trick for a
substitutable-but-absent `/nix/store` path. It is not needed:
`builtins.storePath` hands such a path to the daemon, which substitutes it
before any read, and import-from-derivation and `builtins.outputOf` are
unsupported, so the evaluator never needs bytes the store does not have yet.

## Measurements

`bench/workloads/nixos-desktop.nix` with the pinned nixpkgs, 16-core EPYC
with other builds running (load average 2-4), release build,
2026-09-16. The read set holds 5,340 observations: 3,513 files (37.7 MB
hashed), 781 `readDir` listings, 643 copied trees (625 single files, 18
directories, 10 of them filtered), 399 stats, 4 environment variables.
The `readset` file is 1.1 MB; the `build` entry adds a 16 MB `drvs` and a
440 KB `sources`.

`ietsp eval --strict`, six interleaved pairs at `--workers 1` (wall ~
user, so the recording cost is visible):

| run | wall (median) | user | sys |
|---|---|---|---|
| `--no-cache` | 1.92 s | 1.43 s | 0.47 s |
| miss (recording + write) | 1.98 s | 1.47 s | 0.48 s |
| hit | 0.04 s | 0.02 s | 0.06 s |

Recording costs ~3 % at one worker: ~28 ms is the sha256 of the 37.7 MB
of source bytes (`perf`: `digest_blocks` 4.4 % of samples against 3.0 %
without the cache), the rest is the `lstat` walk of each copied tree and
the 3-5 ms encode-and-write of the read set. At 4 workers the difference
is inside the run-to-run noise (miss 1.30-1.62 s, `--no-cache` 1.39-1.54
s): the hashing runs on the fiber that read the file, so it spreads over
the workers.

A hit takes 40 ms wall: 4-5 ms to read and decode the read set, 23-40 ms
to validate it on 4 threads (the `readDir` re-listings and the tree walks
dominate; plain file stats are a few microseconds each), the rest is
process start. Deduplicating the read set by cloning every path cost
15 ms per write; deduplicating by reference brought the write to 3-5 ms.

On a `build --no-build` hit of nixos-desktop, loading the 3,616 drvs and
657 sources from the cache took 0.11 s against 1.32 s of evaluation.

`scripts/cachetest.nu` covers the semantics (18 cases).

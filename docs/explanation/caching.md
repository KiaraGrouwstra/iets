# Caching, and why the input store is safe

ietsp keeps three things on disk. Two are caches — they hold the result of
work that can be done again. One is not a cache at all: it is where a
fetched flake input *lives*. All three follow the same discipline, and this
page explains what that discipline is and why the third one is safe to keep
in a user's home directory with no daemon and no signature.

| store | default root | entry | keyed on |
|---|---|---|---|
| compiled IR | `~/.cache/ietsp/ir` | one file | build id, path, system, store directory, source |
| tracing runs | `~/.cache/ietsp/trace` | one file in a directory | the whole command |
| fetched inputs | `~/.cache/ietsp/inputs` | one directory | the store path the lock implies |

Each cache has a flag that turns it off. `--no-tracing-cache` turns off
the tracing cache. `--no-ir-cache` turns off the IR cache. `--no-cache`
turns off both, and `IETSP_CACHE=0` and the `cache` setting are the same
switch. No flag turns off the input store, because it is not a cache: a
run needs the input's bytes. `--cache-dir DIR` moves all three stores.

The ideas come from Mic92's nix-tarmac: a writer lock a caller may give up
on, a touch interval so a hit writes nothing, a TTL sweep that runs at most
once a day, and poison-and-wipe instead of repair.

## One discipline, four rules

**Stage and rename.** An entry is one file. It is written under a temporary
name with everything the entry holds in it, and renamed into place. A
rename is atomic, so one rename publishes the whole entry: a reader sees
either the whole old entry or the whole new one, never a torn file and never
two generations mixed.

**Readers never lock.** They do not have to. The rename is what makes the
lock unnecessary on the read path, and the read path is the hot one. A
reader opens the entry once and reads every part it needs through that one
descriptor, so the bytes it goes on to use are the generation it validated,
whatever a sibling publishes meanwhile. The old file stays alive for as long
as that descriptor does, even after a publish or a sweep unlinked its name.

**A writer holds the store's `flock` for exactly one entry, and gives up
after 50 ms.** A skipped write is a miss on a later run. It is never a
stall and never an error. The lock exists so that two writers of one key
cannot cross inside one temporary file; it is not a critical section around
a run. The temporary name also carries the process id and a per-write
counter, so two threads writing one key in one process cannot cross either.
A killed writer leaves a temporary file that no reader looks at and that the
sweep removes after a day; the entry it was replacing stays whole and
usable.

Measured: four `ietsp eval` processes filling one cold store in parallel
wrote 97 IR entries and skipped none.

Two things the rename does not promise, and does not have to. Nothing is
fsynced, so a power loss can leave the name pointing at a file whose bytes
were never written; those bytes are then read as any other bad bytes are,
and the entry goes. And atomicity is per entry, not across entries: a run
that published two keys can be interrupted between them. No reader joins two
entries, so nothing depends on that.

**Poison and wipe; never repair.** An entry that does not decode is
deleted and the run continues as a miss. A store whose version file is of
another format, or whose garbage-collection stamp cannot be parsed, or
whose lock is not a file, is emptied and starts again.

That holds for *any* bytes, not only for bytes ietsp wrote: a cache
directory is writable by whoever runs ietsp, so a truncated, corrupt or
crafted entry must be a miss and never a crash. The IR decoder therefore
checks every stored constant against the immediate encodings the writer can
produce — a word is never a raw bit pattern the evaluator would follow as a
pointer — checks every index against what it has already decoded (a local
slot against its frame, a `with` depth against a `with` scope, a symbol
against the entry's name table, a position against the file's length), and
bounds both its nesting and the items its counted runs may ask for by the
entry's own length.

**ietsp removes only what ietsp put there.** A wipe is scoped to the names
that store's own layout makes — the version file, the lock, the stamp, the
temporary files, the entries and, for the IR cache, its shard directories.
Anything else in the directory survives, and the directory itself is never
removed. The three stores are subdirectories (`trace/`, `ir/`, `inputs/`)
of the cache root, so `--cache-dir DIR` and `IETSP_CACHE_DIR` never make
`DIR` itself a removal target. A store subdirectory that exists, holds
something, and carries no name from that layout is not ietsp's: ietsp
refuses it with an error naming it and runs without that cache, rather than
writing into it or removing anything from it.

The sweep removes entries not read for 30 days. It runs on a write path, at
most once a day per store, and never on the read path. The stamp is claimed
under the writer lock and the walk then runs unlocked, so a sweep of a large
store never holds a sibling's write back. It only ever removes a name that
looks like one of that store's keys, so the version file, the lock, the
stamp, and any nested store under a shared cache directory all survive.

A sweep of a tracing store of 10,000 entries, warm page cache, 16-core
EPYC: 0.03 s when nothing is past the TTL, 0.13 s for a dry run that also
sizes every entry, 0.57 s when all 10,000 go. The once-a-day sweep of a
real store is the first row.

A warm hit writes nothing at all. The tracing cache rewrites its read set
only when validation refreshed a stat field, and otherwise touches the
entry's mtime only when that mtime is over a day old. The IR cache takes
the mtime from the same `fstat` that sizes its read, and touches through
that open descriptor. So a hot cache on a busy machine does no disk writes.

## The IR cache: the same file, compiled once

Parsing and compiling the 3,500 files of the nixos-desktop workload was
about 28 % of a one-worker run. The IR cache writes each imported file's
compiled IR after its first compile and loads it on later runs. All the
files load in 229 ms against 696 ms to compile;
`hackage-packages.nix` alone loads in 71-77 ms against 190-218 ms to
compile.

Three things make the IR process-specific, and the loader undoes each one.
Symbol ids are re-interned. Positions are stored relative to the file and
rebased. Constant words are stored by content for a literal and by name for
a global or a `builtins` member. Binding frames are sorted by symbol id,
which differs between processes, so the loader re-sorts them and rewrites
every local index through a permutation stack.

Only `import` uses it. `scopedImport`, `-E` and a file under a `~/` path
compile from source. Files under 1 KiB are not cached: below that the load
costs what the compile costs.

The key holds the build id, so a new binary never reads an old entry.

It is also the only cache `eval-jobs` uses, which makes its effect easy to
see. On `nixos/release-small.nix` with four workers, 16-core EPYC:

| tool | run | wall | max RSS |
|---|---|---|---|
| `ietsp eval-jobs` | cold | 0.29 s | 85 MiB |
| `ietsp eval-jobs` | warm | 0.26 s | 85 MiB |
| `nix-eval-jobs` | cold | 0.68 s | 113 MiB |
| `nix-eval-jobs` | warm | 0.66 s | 113 MiB |

nix-eval-jobs keeps no on-disk evaluation cache for a file argument, so its
two rows differ only in page cache.

How much a second nixpkgs costs is worth knowing before you worry about
disk. Evaluating the nixos-desktop workload at the pinned nixpkgs and then
at a nixpkgs two releases older: the second added one tracing entry
(0.9 MiB) and 56 IR entries (0.5 MiB). Ten of the 66 IR entries carried
over — 15 % — and the tracing entry shares nothing at all, because its key
holds the nixpkgs path.

## The tracing cache: keyed on what a run read

This is the one that is not in Nix. `ietsp eval` and `ietsp build` remember
what a run printed, together with every observation of the outside world
the run made. A later run with the same command validates those
observations. If nothing changed, it replays the result and skips
evaluation entirely.

Every primop that looks at the world records one observation.

| observation | recorded by | validated by |
|---|---|---|
| a file's size, mtime, inode and sha256 | `import`, `readFile`, `hashFile`, every `.drv` read | `lstat`; on a stat mismatch the file is rehashed and accepted if the hash matches |
| a directory listing's hash | `readDir` | re-list and rehash |
| a path's kind | `pathExists`, `readFileType`, the `default.nix` probe, every search-path candidate | re-stat |
| a copied tree's NAR hash and stat hash | `./path` coerced to a string, `builtins.path`, `filterSource`, a flake's git working tree | recompute the stat hash with `lstat` only; on a mismatch re-dump the NAR with the recorded filter and compare |
| a resolved symlink | `storePath`, `builtins.path` and `filterSource` when canonicalising changed the path | canonicalise again |
| an environment variable | `getEnv`; `HOME` when a `~/` literal is compiled | read it again |
| a command's exit status and output | the `git` queries of a flake working tree | run it again, rehash |

A run that had an entry and lost it says why, in one line:

```
$ ietsp eval --strict ./flake-attrs.nix
re-evaluating: /home/u/repo/flake.nix changed
```

The line names the one observation that no longer holds and what about it
changed — a file's contents, a directory's entries, an environment variable,
a git working tree. The file the command names is one of those files, so
editing it reads the same as editing anything it imports. Under `--log-format internal-json` it is a level-3
record with code `cache-invalidated` (docs/reference/errors.md). Validation
runs on four threads, so the read set's own order decides which observation
is named: the lowest-indexed failure wins, and one change therefore always
names the same thing. A run with no entry prints nothing: a first run
invalidated nothing.

The key is computed from the command *before* evaluation: the build id, the
subcommand, the expression, or the path of the file or the flake directory,
the `--arg` pairs, `-A`, `--strict`, `--json`, `--raw`, the lint levels, the
search path, `builtins.currentSystem`, and the working directory only when a
relative path literal can reach it. `--workers`, `--builder`, `--timings`
and `--dry-run` are not in the key: they do not change the answer.

The key names the file by path and never by content, and the same holds for
a flake's `flake.nix` and `flake.lock`. The run reads all of them like any
other file it reads, so the read set holds them, and one command therefore
keeps one entry however often the file is edited. Content in the key would
make an edit a *different question* instead of a stale answer: the run would
miss with no entry, say nothing about why, and leave the entry of every
earlier version of the file behind until the sweep. One command, one entry,
and an edit of any file the run read — the one named on the command line
included — is an invalidation that names it.

Some things make a run uncacheable rather than wrong: `builtins.currentTime`
and every network fetcher without a hash. `currentTime` is a lazy thunk over
an internal primop, so the mark happens only if the program actually forces
it.

An entry is one file, `entry`, in a directory named by the key. The file
starts with a version byte and a table of the parts it holds — where each
one is and how long it is — and which parts exist depends on the
subcommand. `readset` holds the observations. `result` holds the exact
stdout bytes for `eval`, or the root derivation paths and the wanted output
for `build`. A `build` entry adds `drvs` (every derivation the evaluation
produced) and `sources` (where each source's bytes were), and any entry
whose run printed lint warnings adds `warnings`, which a hit replays.

An `eval` entry omits `drvs` and `sources` deliberately: the subcommand is
in the key, so nothing would read them back, and writing the 16 MB `drvs` of
nixos-desktop cost 40 ms — 3 % — on every miss.

One file is what makes the entry atomic. A lookup opens it, reads the table,
and then reads only the parts it wants out of that open file: the read set
first, and the result, tables and warnings after validation. A write the
lock cut short, or a writer that was killed, leaves the previous entry
untouched, because nothing was renamed. Two writers of one key each publish
a whole entry, so the loser's bytes are replaced, never woven in. The one
thing the file costs: a hit that refreshed a stat field has to publish the
whole entry again, not just the read set. For an `eval` entry that is the
read set plus a few kilobytes. For a `build` entry of nixos-desktop it is
17 MB, on a path that runs only when a file's mtime moved and its content
did not.

The cost of recording is about 3 % at one worker on nixos-desktop, and
inside the noise at four, because the hashing happens on the fiber that read
the file and so spreads over the workers. What it buys:

| run | wall (median) | user | sys |
|---|---|---|---|
| `--no-tracing-cache` | 1.92 s | 1.43 s | 0.47 s |
| miss (recording and writing) | 1.98 s | 1.47 s | 0.48 s |
| hit | 0.04 s | 0.02 s | 0.06 s |

The read set for that run holds 5,340 observations: 3,513 files (37.7 MB
hashed), 781 directory listings, 643 copied trees, 399 stats and four
environment variables. The read-set file is 1.1 MB. A hit is 4-5 ms to read
and decode it, 23-40 ms to validate it on four threads, and the rest is
process start.

Two properties are worth naming. Errors are never cached: an entry is
written only after the result was printed without error. And the read set
over-approximates rather than under-approximates — a cancelled helper fiber
still leaves its observations in it, which costs a rare extra miss and never
a wrong hit. A file that changes mid-run may be recorded twice with two
hashes, and validation then fails on one of them, which is a miss.

One consequence surprises people: `builtins.trace` and `builtins.warn` write
to stderr *during evaluation*, and a hit does not evaluate, so a hit prints
none of them. Lint warnings are different — the entry records them and a hit
replays them.

## The input store, and why FOD-only makes it safe

A locked flake input that is not in `/nix/store` has to become readable
somehow. ietsp downloads it into `~/.cache/ietsp/inputs`, and evaluation
then reads the input's bytes out of *that*, not out of a store. A build
uploads the input's NAR straight out of it as well. Nothing is registered
with any daemon, at any point.

That sounds like it should need trust. It does not, and the reason is the
one design decision behind the whole store: **an entry may only ever be
named by the hash its content must have, and that hash is known before the
fetch begins.**

A locked flake input is a fixed-output derivation in the only sense that
matters here. The lock file states the input's `narHash`. ietsp derives the
store path from that hash before it opens a socket. It streams the archive
into a staged directory, writes the NAR out of the index straight into a
hasher, and the staged directory becomes an entry **only if that hash is
the one the lock named**. A wrong tree is never named, never read and never
uploaded.

So the content verifies itself. Follow what that removes:

- **No signature is needed.** A signature answers "who says these bytes are
  what they claim to be?". A fixed-output hash answers it without anyone
  saying anything: recompute the hash and you know. That is why the
  put operation takes the hash the tree must have and has no other form.
- **No trusted builder is needed.** Nothing here is built. There is no
  input-addressed output, whose path is a *claim* about a build that
  happened somewhere, and which therefore needs someone trustworthy to have
  made it.
- **No daemon is needed.** A daemon exists to be the one process allowed to
  write a shared, trusted store. This store is neither shared with other
  users nor trusted in that sense: it is a directory in the user's own
  cache, and a caller does not have to trust it because it re-derives what
  the name must be from the lock.
- **Entries are safe to share between processes.** Two writers of one key
  can only ever write bytes the other would have written, because the name
  determines the content. That is why the concurrency rules above are
  enough and no protocol is needed on top of them.

What a reader *does* check on open is small: the index's version byte, and
the spool's length against what the index recorded. Anything else wipes the
entry, which costs one fetch. It never re-hashes the tree, because the name
already is the hash.

An entry holds the decompressed tar exactly as downloaded (`spool`), an
index of every member's offset, size, mode and symlink target, and a
`sparse` file when the archive had GNU sparse members. No tree is ever
unpacked. That is not only a disk saving: a full nixpkgs revision costs
19 MB of index over the evaluator's own 75 MB, where unpacking a tree cost
307 MB — 13.5 s and 382 MB peak before, 7.7 s and 94 MB after.

One place in the evaluator maps a store path to its bytes, and it serves
`import`, `readFile`, `readDir`, `pathExists`, `readFileType`, `hashFile`,
`builtins.path`, `filterSource` and a path interpolation alike, from a real
file system, from an input-store entry read by `pread` at the index's
offset, or from memory. `/nix/store` is checked first and wins when it has
the tree; an input that appears there later, because a build registered it,
changes nothing, since the two hold the same bytes by construction.

This also gives the read set a cheap shape. An input-store entry is one
observation, not one per file, because the entry's name *is* the NAR hash
of its tree: a name that is still there still holds the bytes the run read.
Validating a whole nixpkgs is therefore two stat calls and a version-byte
comparison, against the 3,513 file observations the same tree would cost if
its files were checked one by one.

There is one place a process waits for a sibling. Before it downloads,
a process takes a lock on that tree's name. A second process that needs the
same tree prints one line, blocks, and then reads the entry the first one
stored. If the first fetch failed, the second downloads it itself. The lock
is per tree, so different inputs still download in parallel.

## What this does not cover

The input store holds trees whose content address is a flake `narHash`. It
does not hold a substitutable-but-absent `/nix/store` path, and it does not
need to: `builtins.storePath` hands such a path to the daemon, which
substitutes it before any read, and import-from-derivation is unsupported,
so the evaluator never needs bytes the store does not have yet.

`git` and `hg` inputs are the other gap. They need a repository, which ietsp
cannot clone, so they still go through a real `nix` that writes
`/nix/store`.

The NAR hash of a source tree has no store of its own. It does not need one:
the tracing cache's tree observation already holds it.

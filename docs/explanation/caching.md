# Caching, and why the input store is safe

ietsp keeps three things on disk. Two are caches — they hold the result of
work that can be done again. One is not a cache at all: it is where a
fetched flake input *lives*. All three follow the same discipline, and this
page explains what that discipline is and why the third one is safe to keep
in a user's home directory with no daemon and no signature.

| store | default root | entry | keyed on |
|---|---|---|---|
| compiled IR | `~/.cache/ietsp/ir` | one file | build id, path, system, store directory, source |
| tracing runs | `~/.cache/ietsp/trace` | one directory | the whole command |
| fetched inputs | `~/.cache/ietsp/inputs` | one directory | the store path the lock implies |

The ideas come from Mic92's nix-tarmac: a writer lock a caller may give up
on, a touch interval so a hit writes nothing, a TTL sweep that runs at most
once a day, and poison-and-wipe instead of repair.

## One discipline, four rules

**Stage and rename.** Every entry file is written under a temporary name
and renamed into place. A rename is atomic, so a reader sees either the
whole old entry or the whole new one, never a torn file.

**Readers never lock.** They do not have to. The rename is what makes the
lock unnecessary on the read path, and the read path is the hot one.

**A writer holds the store's `flock` for exactly one file, and gives up
after 50 ms.** A skipped write is a miss on a later run. It is never a
stall and never an error. The lock exists so that two writers of one key
cannot cross inside one temporary file; it is not a critical section around
a run. The temporary name also carries the process id and a per-write
counter, so two threads writing one key in one process cannot cross either.
A killed writer leaves a temporary file that no reader looks at and that the
sweep removes after a day.

Measured: four `ietsp eval` processes filling one cold store in parallel
wrote 97 IR entries and skipped none.

**Poison and wipe; never repair.** An entry that does not decode is
deleted and the run continues as a miss. A store whose version file is
missing, unreadable or of another format, or whose garbage-collection stamp
cannot be parsed, or whose lock is not a file, is removed whole and starts
empty.

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

The key is computed from the command *before* evaluation: the build id, the
subcommand, the expression or the file plus its hash, the `--arg` pairs,
`-A`, `--strict`, `--json`, `--raw`, the lint levels, the search path,
`builtins.currentSystem`, and the working directory only when a relative
path literal can reach it. `--workers`, `--builder`, `--timings` and
`--dry-run` are not in the key: they do not change the answer.

Some things make a run uncacheable rather than wrong: `builtins.currentTime`
and every network fetcher without a hash. `currentTime` is a lazy thunk over
an internal primop, so the mark happens only if the program actually forces
it.

The cost of recording is about 3 % at one worker on nixos-desktop, and
inside the noise at four, because the hashing happens on the fiber that read
the file and so spreads over the workers. What it buys:

| run | wall (median) | user | sys |
|---|---|---|---|
| `--no-cache` | 1.92 s | 1.43 s | 0.47 s |
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

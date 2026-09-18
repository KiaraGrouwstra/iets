# Cache lifecycle and concurrency

ietsp keeps two on-disk caches, and both live under the same rules
(`src/cachefs.rs`). The ideas come from Mic92's nix-tarmac: a writer lock
that a caller may give up on, a touch interval so a hit writes nothing, a
TTL garbage collection run at most once a day, and poison-and-wipe instead
of in-place repair.

| store | default root | entry | keyed on |
|---|---|---|---|
| compiled IR (`src/compile/cache.rs`, docs/memory.md) | `~/.cache/ietsp/ir` | `<2 hex>/<62 hex>`, one file | build id, path, system, store dir, source |
| tracing runs (`src/trace/`, docs/tracing-cache.md) | `~/.cache/ietsp/trace` | `<64 hex>/`, one directory of files | the whole command (docs/tracing-cache.md, "Key") |

`--cache-dir DIR` or `IETSP_CACHE_DIR` moves both: the tracing store is
then `DIR` itself and the IR store `DIR/ir`.

## Concurrency

Many processes share one cache directory: `eval-jobs` workers, a direnv
hook, the classic command names, a second terminal. The invariant:

- every entry file is written to a temporary name and renamed into place,
  so a reader sees either the whole old entry or the whole new one;
- **readers never lock.** A rename is atomic, so a reader cannot see a
  torn file;
- **a writer holds the store's `flock` for exactly one file**, and a
  writer that cannot take it within 50 ms skips that write. A skipped
  write is a miss on a later run, never a stall and never an error. The
  lock exists to keep two writers of one key from crossing inside one
  temporary file; it is not a critical section around a run;
- the temporary name carries the pid and a per-write counter, so two
  threads writing one key in one process cannot cross either;
- a killed writer leaves a `.name.pid.n.tmp` file, which no reader looks
  at and the sweep removes after a day.

The tracing cache writes `readset` last. A write the lock cut short leaves
an entry with no read set, which the next lookup treats as a miss.

Measured: four `ietsp eval` processes filling one cold store in parallel
wrote 97 IR entries and skipped none.

## No writes on a warm hit

- The tracing cache rewrites `readset` when validation refreshed stat
  fields, and otherwise touches nothing but the entry's mtime, and only
  when that mtime is more than a day old. A rename into the entry
  directory already updates its mtime, so a refreshed entry needs no
  touch.
- The IR cache takes the entry's mtime from the same `fstat` that sizes
  its read, and touches through that open file descriptor when the touch
  is due.

So a hot cache on a busy box does no disk writes at all.

## Garbage collection

Entries not read for 30 days go. The sweep walks entry mtimes and removes
what is past the TTL; it runs on a write path, at most once a day per
store, and never on the hot read path. The stamp (`gc-stamp`, unix seconds)
is claimed under the writer lock and the walk then runs unlocked, so a
sweep of a large store never holds a sibling's write back. A swept tracing
entry simply misses next time.

By hand: `ietsp cache gc [--ttl DAYS] [--dry-run] [--cache-dir DIR]`.

A sweep only ever removes a name that looks like a key (64 hex characters,
62 under a shard), so `version`, `lock`, `gc-stamp`, the `ir/` store under
a shared `--cache-dir`, and anything else in the directory survive.

Cost on a tracing store of 10,000 entries (4 KiB each, 39.1 MiB of
payload, 80 MB on disk), warm page cache, 16-core EPYC:

| sweep | wall | sys |
|---|---|---|
| nothing past the TTL (one `lstat` per entry) | 0.03 s | 0.02 s |
| `--ttl 0 --dry-run` (also sizes every entry) | 0.13 s | 0.12 s |
| `--ttl 0` (removes all 10,000) | 0.57 s | 0.52 s |

The once-a-day sweep of a real store is the first row.

## Poison and wipe

Nothing is ever repaired in place.

- An entry that does not decode is deleted, and the run continues as a
  miss. That covers a truncated file, a garbled file, and a `build`
  entry whose drv or source table does not decode.
- A store whose `version` file is missing, unreadable or of another
  format, or whose `gc-stamp` cannot be parsed, or whose `lock` is not a
  file, is removed whole and starts empty. `--timings` prints one line
  (`cache: wiping <dir> (<why>)`).

`cachefs::FORMAT` is the store version. Both keys already hold the build
id, so a new ietsp binary never reads an old entry; the version file is
what lets a layout change throw the old entries away instead of leaving
them for the TTL. The first run of this code wipes an older store, which
has no version file.

## Benches

`bench/eval-jobs.nu [rounds]` runs `ietsp eval-jobs` and `nix-eval-jobs`
on `nixos/release-small.nix` with 4 workers and interleaves cold and warm
cache runs. Without `--force-recurse` both tools agree that the jobset has
one job, the `tested` aggregate, so the numbers are small. The script then
runs each tool once with `--force-recurse` and compares the job sets: 123
lines on both sides (`--no-parity` skips that pair, which takes minutes).
`eval-jobs` uses the IR cache only, so cold against warm is the IR cache.
2026-09-18, 16-core EPYC, median of 2 rounds:

| tool | run | wall | user | sys | max RSS |
|---|---|---|---|---|---|
| ietsp eval-jobs | cold | 0.29s | 0.12s | 0.08s | 85 MiB |
| ietsp eval-jobs | warm | 0.26s | 0.09s | 0.08s | 85 MiB |
| nix-eval-jobs | cold | 0.68s | 0.30s | 0.22s | 113 MiB |
| nix-eval-jobs | warm | 0.66s | 0.31s | 0.21s | 113 MiB |

`max RSS` is the main process; ietsp's evaluation now happens in a worker
process (`--max-memory-size` defaults to 4096 MB, `docs/eval-jobs.md`).

nix-eval-jobs keeps no on-disk evaluation cache for a file argument, so
its two rows differ only in page cache.

`bench/cache-growth.nu` evaluates `bench/workloads/nixos-desktop.nix` at
the pinned nixpkgs, then at a second nixpkgs revision in the store, and
reports what the second one adds:

| nixpkgs | trace entries | trace size | IR entries | IR size |
|---|---|---|---|---|
| ah3pxz4xa52h (26.11) | 1 (+1) | 1.0 MiB (+1.0 MiB) | 66 (+66) | 0.5 MiB (+0.5 MiB) |
| bgp6cqzszs95 (26.05) | 2 (+1) | 2.0 MiB (+0.9 MiB) | 122 (+56) | 1.0 MiB (+0.5 MiB) |

Two nixpkgs releases apart, 10 of 66 IR entries carried over (15 %) and
the tracing entry shares nothing: its key holds the nixpkgs path. The IR
entry count is low because only `import` uses that cache and files under
1 KiB skip it (docs/memory.md).

A/B of a warm run against the commit before this work (hyperfine,
`--warmup 1 --runs 8`, separate cache directories): 1.038 s against
1.045 s, inside the noise. A tracing-cache hit measures 45 ms either way;
run-to-run drift on this box is ±5 ms.

## Tests

`tests/cache_hygiene.rs`, after nix-tarmac's test shapes: eight threads
hammering four keys (no reader ever sees a torn or mixed entry), a child
process SIGKILLed mid-write six times (the store reads clean and the next
writer takes the lock), a truncated and a garbled entry plus a garbled
stamp (the entry and then the store go, and the store works again), and
`ietsp cache gc` over both stores (past the TTL goes, inside it stays, a
dry run changes nothing). `src/cachefs.rs` unit tests cover the version
file, the lock, the touch interval and the sweep.

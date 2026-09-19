# NAR: writing, reading, hashing

`src/store/nar.rs` is ietsp's whole NAR implementation: the archive writer
(`dump_path`, `dump_string`), the hashes Nix addresses paths by (`nar_hash`,
`nar_size`, `flat_hash`), and the reader (`restore_path`, `parse_length`).

It is on the hot path of every build. `src/remote/plan.rs` hashes every
source of a build in a thread pool, `src/remote/worker/build.rs` dumps each
one again into the upload, `src/builtins/paths.rs` hashes a tree whenever
`"${./tree}"`, `builtins.path` or `filterSource` needs its store path, and
`src/remote/copy.rs` and `--fetch-to` restore NARs from a stream.

## Parity

The output is byte-identical to `nix-store --dump`, not merely
hash-identical. `tests/nar_parity.rs` proves it on a tree of unicode and
awkward names, empty files, empty directories, symlinks (dangling, and
pointing out of the tree), executables, contents on and off the 8-byte
boundary, deep nesting and a file larger than the writer's buffer:

- the whole NAR matches `nix-store --dump` byte for byte;
- every subtree on its own hashes like `nix-store --dump | sha256sum`;
- a dump restored by `restore_path` dumps back to the same bytes;
- `parse_length` agrees with `nar_size`, and rejects a truncated archive, a
  misspelled keyword and non-zero padding;
- the reader refuses a name past `NAME_MAX` rather than failing later at
  `File::create`.

Each test skips itself when `nix` is absent; `nix develop` has it.

## Why it is shaped this way

The reader follows nix-community/go-nix PR #123 (`pkg/narv2`, edef): a
synchronous state machine over one buffered window, with peek and discard,
token *groups* matched against pre-computed framed constants, and the node
shape chosen from a single byte. The writer emits the same groups. Four
things follow, and they are where the time went:

1. **Token groups, not tokens.** NAR framing is canonical — exact lengths,
   zero padding — so a run of keywords is one fixed byte string. `framed()`
   builds them at compile time (`TOK_NAR`, `TOK_REG`, `TOK_EXE`, `TOK_SYM`,
   `TOK_DIR`, `TOK_ENT`, `TOK_NOD`, `TOK_PAR`, the same cuts go-nix makes).
   The writer copies one; the reader compares one.
2. **Byte dispatch.** The groups are cut so that byte 16 of a peeked node
   header names the node: `target` (6) after `symlink`, `contents` (8) after
   `regular`, `executable` (10) after an executable `regular`, and `y` —
   `directory`'s own last letter — for a directory. Inside a directory, byte
   0 of a 16-byte peek is 1 for `)` and 5 for `entry`. No token is parsed to
   decide anything.
3. **One 1 MiB window on each side.** `NarSink` stages framing and reads
   file contents straight into the same buffer, so a whole subtree of framing
   reaches the hasher as one update and the bytes are copied once. `Reader`
   is that window backwards: a token is a slice of it, and contents leave in
   window-sized runs. Before this, the writer emitted every 8-to-24-byte
   token to a `dyn Write` and read contents through `io::copy`'s 8 KiB stack
   buffer, and the reader called `read_exact` for each token *and its
   padding* — `perf` put 45% of a restore's user cycles in
   `read_padding`/`read_exact`/`memmove` on 1-to-8-byte reads.
4. **No per-entry allocation.** The walk keeps one byte path buffer (a
   `PathBuf::pop` re-parses the whole path to find the last separator: 10% of
   a restore on its own), one name arena and one entry index, all held to
   stack discipline. A node's type comes from the parent's `readdir`, not a
   second `lstat`.

## Measured

AMD EPYC Genoa, warm page cache, the pinned nixpkgs tree
(`/nix/store/ah3…-source`: 40k entries, 212 MB of NAR) and one 500 MB file.
`bench/nar.nu` records these; `bench/nar.nu --nix` adds the reference rows.

| case | before | after | reference |
|---|---|---|---|
| NAR hash of the tree | 848 ms | 665-672 ms | `nix hash path` 1665 ms |
| dump of the tree, no hash | 705 ms | 545-567 ms | `nix-store --dump \| sha256sum` 2034 ms |
| NAR hash of a 500 MB file | 342 ms | 353-359 ms | `nix hash path` 386 ms |
| restore of the tree from a pipe | 1962-1988 ms | 1744-1804 ms | `nix-store --restore` 2019 ms |
| **the parser alone** | no such path | **30.5 ms (6.97 GB/s)** | — |
| `"${./tree}"` store path | 1156 ms | 692-696 ms | — |

What paid and what did not:

- **The parser is now free.** `parse_length` walks the framing of the whole
  212 MB, 40k-entry archive in 30.5 ms. A restore of the same archive takes
  1806 ms, so parsing is 1.7% of it; the rest is ext4 inode creation and
  writeback, which is why the restore's wall time barely moved while its user
  time fell from 109 ms to 61 ms.
- **The framing and the walk were the dump's cost, not the hashing.** The
  dump without hashing was 705 ms of the 848 ms hash, so 83% of a tree hash
  was I/O, framing and allocation. That is what the four points above fixed.
- **SHA-256 is already optimal.** `perf` puts 99% of the 500 MB hash in
  `sha2::sha256::x86::digest_blocks`, the SHA-NI path `cpufeatures` selects
  at run time. 1.46 GB/s is the hardware's single-stream rate; the `asm`
  feature would add nothing. No change made.
- **Large files are hash-bound, so nothing to win there.** 1.5 GB/s before
  and after; the 8 KiB `io::copy` reads mattered only because they
  interleaved with framing on small files.
- **Parallel prefetch was not attempted.** A NAR is one SHA-256 stream, so
  the hash is serial, and on a warm cache there is nothing to prefetch. It is
  the remaining idea for cold-cache tree hashing.

## Why there is no separate NAR-hash cache

Caching a tree's NAR hash against a cheap signature of its `stat` data — so
an unchanged source tree is never re-dumped for its store path — is already
what the tracing cache does. `Obs::Tree` records `nar_hash` beside a
`stat_hash` over every entry's relative path, type, size, mtime, inode and
executable bit, and validation recomputes only the `stat_hash`, re-dumping
the NAR only when that differs (docs/tracing-cache.md, docs/caches.md). A
second cache under `src/cachefs.rs` would duplicate it with a weaker key.

## One thing that had to be undone

The writer's window is built once per NAR, and a `dump_path` of a two-file
tree is as common in an evaluation as one of nixpkgs. Allocating and zeroing
a megabyte for each of those cost more than the flushes it saved: it put
`bench/run.nu nixos-desktop` 6% behind. The sink now starts at 128 KiB and
grows to 1 MiB only when a file's contents would not fit, which is where the
larger buffer earns its keep in read syscalls. Head to head on the same
machine afterwards: 1.311 s against 1.337 s for the same binary without these
changes, over ten runs.

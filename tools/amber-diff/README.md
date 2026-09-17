# amber-diff: keys against amber-store-core

`src/amber/` reimplements amber-store's object model (key, UltraCDC byte
chunker, item chunker, CBOR objects, tree builders) because ietsp cannot
depend on the core-rs crate: it is a private git dependency and the nix
package build has no network. This harness proves the reimplementation
produces the same keys and the same object bytes as core-rs at the rev
amberfs pins (`72f38e9e`), with amberfs's parameters (hairesis decision
0028: bytes 8 KiB / 32 KiB / 64 KiB, `item_bits = 7`, no xattr inlining).

- `amber-keys.rs`: a core-rs `examples/` program. It runs the real
  `ingest::objects` on a path and prints every object, sorted by key:
  `<key hex> <type> <length> [<bytes hex>]`, then `root <key>`.
- `ietsp amber-keys [--lstat] PATH` prints the same list from `src/amber/`.
- `run.nu`: copies core-rs to `~/tmp/core-rs-scratch`, builds the example
  offline in its dev shell, and compares both programs on a synthetic tree
  and on real store paths.

```
nu tools/amber-diff/run.nu              # synthetic tree + 3 store paths
nu tools/amber-diff/run.nu /nix/store/...-foo   # any paths
CORE_RS=/path/to/core-rs nu tools/amber-diff/run.nu
```

Metadata: core-rs stores `lstat` uid, gid, mtime and mode in every directory
entry. ietsp stores Nix's canonical values (uid 0, gid 0, mtime 1 s, mode
0444/0555, directories 0555, symlinks 0777), which is what a registered
store path carries on disk, so `amberfs publish` of a store path and an
`ietsp build` upload of the same tree agree. On a tree outside the store the
comparison uses `--lstat`, which makes ietsp read the same fields core-rs
reads. Trees with xattrs, devices, fifos or sockets are out of scope: a NAR
cannot carry them.

Last run (2026-09-17): see `docs/remote-builder-protocol.md`, "Object
model".

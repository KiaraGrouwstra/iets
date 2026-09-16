# ietsp

A from-scratch Nix evaluator in Rust. Evaluation only. See ARCHITECTURE.md.

```
nix develop -c cargo build --release
./target/release/ietsp eval --strict -E '1 + 1'
scripts/langtests.sh
scripts/difftest.sh -I nixpkgs=/path -E '(import <nixpkgs> {}).hello.drvPath'
bench/run.sh nixos-desktop

# remote builds: evaluate in memory, ship only what the builder lacks
./target/release/ietsp-builderd --listen 127.0.0.1:9455 --store-dir /var/tmp/builder &
./target/release/ietsp build --timings -I nixpkgs=/path -E '(import <nixpkgs> {}).hello'
./target/release/ietsp build --builder http://host:9455 -A hello /path/to/nixpkgs/default.nix
./target/release/ietsp build --no-build FILE.nix     # upload only, no realise
./target/release/ietsp build /nix/store/<hash>-<name>.drv   # legacy: closure from the local store
scripts/remote-e2e.sh
```

See `docs/remote-builder-protocol.md`.

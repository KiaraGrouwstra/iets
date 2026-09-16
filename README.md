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

## Results (2026-09-16)

`bench/run.sh nixos-desktop nixos-minimal`, 16 vCPU EPYC Genoa, nixpkgs
26.11pre1059574.a831408e6378, hyperfine 5 runs, all four evaluators produce
the same drvPath. ietsp runs with its default of 4 workers.

| workload | nix 2.35 | fix 0.3 (16 threads) | iets | ietsp |
|---|---|---|---|---|
| nixos-desktop | 3.58 s | 2.13 s (1.55 s best) | 2.65 s | 1.04 s |
| nixos-minimal | 2.86 s | 1.01 s | 2.09 s | 0.85 s |

Single-threaded ietsp: 1.67 s desktop, 1.32 s minimal. Details in
`docs/parallel.md` and `bench/results/`.

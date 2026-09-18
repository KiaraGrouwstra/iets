# ietsp

A from-scratch Nix evaluator in Rust. Evaluation only. See ARCHITECTURE.md.

```
nix develop -c cargo build --release
./target/release/ietsp eval --strict -E '1 + 1'
scripts/langtests.nu
scripts/difftest.nu -I nixpkgs=/path -E '(import <nixpkgs> {}).hello.drvPath'
bench/run.nu nixos-desktop

# remote builds: evaluate in memory, ship only what the builder lacks
./target/release/ietsp-builderd --listen 127.0.0.1:9455 --store-dir /var/tmp/builder &
./target/release/ietsp build --timings -I nixpkgs=/path -E '(import <nixpkgs> {}).hello'
./target/release/ietsp build --builder http://host:9455 -A hello /path/to/nixpkgs/default.nix
# fallback: any nix-daemon over the worker protocol (daemon, unix:///path, ssh-ng://host)
./target/release/ietsp build --builder daemon -A hello /path/to/nixpkgs/default.nix
./target/release/ietsp build --no-build FILE.nix     # upload only, no realise
./target/release/ietsp build /nix/store/<hash>-<name>.drv   # legacy: closure from the local store
scripts/remote-e2e.nu

# .envrc: what `nix print-dev-env` prints, byte-exact, from the in-memory shell drv
./target/release/ietsp print-dev-env --gc-root .direnv/ietsp-env .   # see docs/direnv.md
```

See `docs/remote-builder-protocol.md`. NixOS hosts: import
`nixosModules.default` and keep `use flake` in `.envrc` (`docs/direnv.md`).

Runs are cached: a second `ietsp eval` or `ietsp build` of the same
command validates the files, directories and environment variables the
first run read and replays the result (nixos-desktop: 40 ms instead of
1 s). `--no-cache` or `IETSP_CACHE=0` turns it off, `--timings` reports
hit or miss. Design and limits: `docs/tracing-cache.md`.

## Results (2026-09-16)

`bench/run.nu nixos-desktop nixos-minimal`, 16 vCPU EPYC Genoa, nixpkgs
26.11pre1059574.a831408e6378, hyperfine 5 runs, all three evaluators produce
the same drvPath. ietsp runs with its default of 4 workers.

| workload | nix 2.35 | fix 0.3 (16 threads) | ietsp |
|---|---|---|---|
| nixos-desktop | 3.58 s | 2.13 s (1.55 s best) | 1.04 s |
| nixos-minimal | 2.86 s | 1.01 s | 0.85 s |

Single-threaded ietsp: 1.67 s desktop, 1.32 s minimal. Details in
`docs/parallel.md` and `bench/results/`.

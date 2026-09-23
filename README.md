# ietsp

A from-scratch Nix evaluator in Rust, and a client that builds what it
evaluates on a Nix daemon or a nix-grpc-store service. See ARCHITECTURE.md;
the test scripts are listed there.

```
nix develop -c cargo build --release
./target/release/ietsp eval --strict -E '1 + 1'
./target/release/ietsp --help   # the commands; ietsp COMMAND --help for its flags
nix flake check   # test, clippy, fmt, examples, langtests, partest, module
scripts/difftest.nu -I nixpkgs=/path -E '(import <nixpkgs> {}).hello.drvPath'
bench/run.nu nixos-desktop

# remote builds: evaluate in memory, ship only what the daemon lacks, over the Nix worker protocol
./target/release/ietsp build --timings -I nixpkgs=/path -E '(import <nixpkgs> {}).hello'
./target/release/ietsp build --builder ssh-ng://host -A hello /path/to/nixpkgs/default.nix
./target/release/ietsp build --builder unix:///path/to/socket --no-build FILE.nix   # upload only, no realise
./target/release/ietsp build /nix/store/<hash>-<name>.drv   # legacy: closure from the local store
./target/release/ietsp build --builder 'grpc://farm.numtide.com:50051?token-file=./jwt' --fetch-to out FILE.nix   # nix-grpc-store, natively

# locked flakes: a missing input is substituted by the daemon or downloaded and
# unpacked by ietsp; only git and hg inputs still run nix (docs/flakes.md)
./target/release/ietsp eval --raw .#packages.x86_64-linux.default.drvPath

# nix-fast-build's flags, one process: eval and build overlap, results as json or junit
./target/release/ietsp fast-build --flake .#checks --skip-cached --result-file r.json

# the hash of a URL or of a fixed-output derivation, for nurl and nix-prefetch
./target/release/ietsp prefetch --unpack https://example.org/src.tar.gz   # docs/prefetch.md
./target/release/ietsp prefetch -E '(import <nixpkgs> {}).hello.src'

# package search, `nix search`'s output; the index is cached, so the second run
# over the same package set does not evaluate (docs/search.md)
./target/release/ietsp search -I nixpkgs=/path ripgrep

# .envrc: what `nix print-dev-env` prints, byte-exact, from the in-memory shell drv
./target/release/ietsp print-dev-env --gc-root .direnv/ietsp-env .   # see docs/direnv.md

# classic mode: the same binary as nix-instantiate, nix-build, nix-eval-jobs and nix,
# exec'ing the real nix for anything it does not implement (docs/classic.md)
nix build .#classic && ./result/bin/nix-instantiate -A hello /path/to/nixpkgs
./target/release/ietsp nix-build -A hello /path/to/nixpkgs   # same, without symlinks
```

See `docs/remote-build.md` and `docs/fast-build.md` (every build path shares
one dispatcher). NixOS hosts: import
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
`docs/parallel.md`.

## Licence

LGPL-2.1-or-later, like Nix (see COPYING). Some files are copied from Nix
(corepkgs, `call-flake.nix`, `get-env.sh`, the `tests/lang` suite) under the
same licence. `proto/nix_remote.proto` comes from Mic92/nix-grpc-store, which
carries no licence yet.

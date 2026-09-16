#!/usr/bin/env bash
# Usage: bench/run.sh [workload...]   (default: nixos-desktop)
# Compares nix-instantiate, fix, iets, and ietsp with hyperfine.
set -euo pipefail
cd "$(dirname "$0")/.."
NP=${NIXPKGS:-/nix/store/ah3pxz4xa52hd6vhbx17vcsci0b8bmbr-source}
FIX=${FIX:-$HOME/tmp/fix-result/bin/fix}
IETS=${IETS:-$HOME/tmp/iets-result/bin/iets}
IETSP=${IETSP:-./target/release/ietsp}
mkdir -p bench/results bench/fix-cache
for w in "${@:-nixos-desktop}"; do
  f=$PWD/bench/workloads/$w.nix
  hyperfine --shell=none --warmup 1 --runs "${RUNS:-5}" --export-markdown "bench/results/$w.md" \
    -n nix "nix-instantiate --eval --strict --option pure-eval false -I nixpkgs=$NP $f" \
    -n fix "$FIX eval --strict --no-progress --option pure-eval false -I nixpkgs=$NP --compile-cache-dir $PWD/bench/fix-cache $f" \
    -n iets "env IETS_RESERVE=cage=64G IETS_NO_CACHE=attr $IETS eval --store null:// --impure -I nixpkgs=$NP --allowed-path $NP --allowed-path $PWD/bench/workloads $f" \
    -n ietsp "$IETSP eval --strict -I nixpkgs=$NP $f"
done

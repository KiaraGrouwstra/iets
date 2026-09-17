#!/usr/bin/env nu
# Usage: bench/run.nu [workload...]   (default: nixos-desktop)
# Compares nix-instantiate, fix, iets, and ietsp with hyperfine.

def main [...workloads: string] {
  cd ($env.FILE_PWD | path join "..")
  let np = ($env.NIXPKGS? | default "/nix/store/ah3pxz4xa52hd6vhbx17vcsci0b8bmbr-source")
  let fix = ($env.FIX? | default $"($env.HOME)/tmp/fix-result/bin/fix")
  let iets = ($env.IETS? | default $"($env.HOME)/tmp/iets-result/bin/iets")
  let ietsp = ($env.IETSP? | default "./target/release/ietsp")
  mkdir bench/results bench/fix-cache
  let ws = if ($workloads | is-empty) { [nixos-desktop] } else { $workloads }
  for w in $ws {
    let f = $"($env.PWD)/bench/workloads/($w).nix"
    ^hyperfine --shell=none --warmup 1 --runs ($env.RUNS? | default "5") --export-markdown $"bench/results/($w).md" -n nix $"nix-instantiate --eval --strict --option pure-eval false -I nixpkgs=($np) ($f)" -n fix $"($fix) eval --strict --no-progress --option pure-eval false -I nixpkgs=($np) --compile-cache-dir ($env.PWD)/bench/fix-cache ($f)" -n iets $"env IETS_RESERVE=cage=64G IETS_NO_CACHE=attr ($iets) eval --store null:// --impure -I nixpkgs=($np) --allowed-path ($np) --allowed-path ($env.PWD)/bench/workloads ($f)" -n ietsp $"($ietsp) eval --strict --no-cache -I nixpkgs=($np) ($f)"
  }
}

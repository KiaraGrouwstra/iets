#!/usr/bin/env nu
# Upload cost of a workload against a fresh ietsp-builderd: one cold run
# (every object missing) and one warm run (nothing missing), `--no-build`.
#
# usage: nu scripts/remote-bench.nu [WORKLOAD]   (default: nixos-desktop)
#        BIN=target/debug NIXPKGS=/nix/store/...-source
use lib.nu [chomp show]

def main [workload: string = "nixos-desktop"] {
  cd ($env.FILE_PWD | path join "..")
  $env.NIX_REMOTE = ($env.NIX_REMOTE? | default "daemon")
  $env.TMPDIR = ($env.TMPDIR? | default "/tmp")
  let bin = ($env.BIN? | default "target/release")
  let nixpkgs = ($env.NIXPKGS? | default "/nix/store/ah3pxz4xa52hd6vhbx17vcsci0b8bmbr-source")
  let work = (mktemp -d --tmpdir ietsp-bench.XXXXXX)
  let port = (20000 + ((random int 0..32767) mod 20000))
  let url = $"http://127.0.0.1:($port)"
  let log = $"($work)/server.log"
  let srv = (job spawn { ^$"($bin)/ietsp-builderd" --listen $"127.0.0.1:($port)" --store-dir $"($work)/builder" e>> $log })
  for _ in 1..100 {
    if ($log | path exists) and (open --raw $log | str contains "listening") { break }
    sleep 50ms
  }
  let file = $"bench/workloads/($workload).nix"
  for run in ["cold" "warm"] {
    print $"== ($run)"
    let t0 = (date now)
    let r = (^$"($bin)/ietsp" build --builder $url --timings --no-build --no-cache -I $"nixpkgs=($nixpkgs)" $file | complete)
    let total = ((date now) - $t0)
    $r.stderr | lines | where $it =~ '^(eval|plan|objects|connect|have|put|build):' | show
    print $"total: ($total)"
    if $r.exit_code != 0 { print -e ($r.stderr | chomp); job kill $srv; rm -rf $work; exit 1 }
  }
  let objects = (ls $"($work)/builder/objects" | length)
  let bytes = (ls $"($work)/builder/objects" | get size | math sum)
  print $"builder store: ($objects) objects, ($bytes)"
  job kill $srv
  rm -rf $work
}

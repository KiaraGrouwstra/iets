#!/usr/bin/env nu
# Usage: bench/nixbuild.nu
# Compares `nix build --store ssh-ng://` and `iets build --builder ssh-ng://`
# against a remote builder, eu.nixbuild.net by default (STORE). It bills per
# CPU-second: the builds are a few seconds of bash each.
#
# Cases, one hyperfine JSON each in the shape bench/chart.nu reads:
#   nixbuild-dispatch        small.nix, a new marker every run: a real build
#   nixbuild-dispatch-fetch  the same, the output copied back
#   nixbuild-cachehit        small.nix, one fixed marker built before: the
#                            builder's store already has the output
#   nixbuild-closure         closure.nix, a new marker every run: python3's
#                            drv closure, a 16 MiB random output
#   nixbuild-closure-fetch   the same, the output copied back
# Copied back means `nix copy --from` into the local store for nix and
# `--fetch-to DIR` for iets, which has no local store.
# nixbuild-phases.json holds PHASE_RUNS (default 3) instrumented runs per
# client and case: nix's from internal-json timestamps, iets's from --timings.
# The boundaries differ: nix's eval ends at its first copy, so it includes
# the ssh connect and the first queries. Compare totals across clients.
# RUNS (default 10) sets the hyperfine runs. Each run writes the account
# usage before and after to bench/results/nixbuild-env.txt.
use ../scripts/lib.nu [require-real-nix]

def usage [host: string]: nothing -> string {
  "usage\nexit\n" | ^ssh -T $host o+e>| lines | where $it =~ 'Free build time left|Build count|CPU time' | uniq | str join "; "
}

def ver [cmd: string]: nothing -> string {
  try { ^$cmd --version | lines | first } catch { "not found" }
}

# every stderr and stdout line with its arrival time, then EXIT
def stamped [cmd: string]: nothing -> table {
  let sh = "t0=$EPOCHREALTIME; { " + $cmd + "; } 2>&1 | while IFS= read -r l; do printf '%s\\t%s\\n' \"$EPOCHREALTIME\" \"$l\"; done; printf '%s\\tEXIT\\n' \"$EPOCHREALTIME\"; printf '%s\\tSTART\\n' \"$t0\""
  ^bash -c $sh | lines | parse "{t}\t{l}" | update t { into float }
}

def nix-phases [rows: table, fetch_s: float]: nothing -> record {
  let t0 = ($rows | where l == START | first | get t)
  let end = ($rows | where l == EXIT | first | get t)
  let recs = ($rows | where l starts-with "@nix " | each {|r| ($r.l | str substring 5.. | from json) | insert t $r.t })
  let copy = ($recs | where action == start and type? == 103 | get -o 0.t)
  let builds = ($recs | where action == start and type? == 105)
  let b0 = ($builds | get -o 0.t | default $end)
  let bids = ($builds | get id)
  let stops = ($recs | where action == stop and id in $bids | get t)
  let b1 = if ($stops | is-empty) { $b0 } else { $stops | math max }
  let e = ($copy | default $b0)
  { eval: ($e - $t0), upload: ($b0 - $e), build: ($b1 - $b0), fetch: (($end - $b1) + $fetch_s), total: ($end - $t0 + $fetch_s) }
}

def iets-phases [rows: table]: nothing -> record {
  let t0 = ($rows | where l == START | first | get t)
  let end = ($rows | where l == EXIT | first | get t)
  let ph = ($rows | get l | parse -r '^(?<k>eval|plan|connect|query|resolve|substitute|add|build|outputs|fetch): .*?(?<s>[0-9.]+)s$' | update s { into float } | reduce -f {} {|it, acc| $acc | upsert $it.k (($acc | get -o $it.k | default 0.0) + $it.s) })
  let g = {|k| $ph | get -o $k | default 0.0 }
  { eval: (do $g eval), upload: ((do $g plan) + (do $g connect) + (do $g query) + (do $g resolve) + (do $g substitute) + (do $g add)), build: (do $g build), fetch: ((do $g outputs) + (do $g fetch)), total: ($end - $t0) }
}

def main [] {
  require-real-nix nix
  cd ($env.FILE_PWD | path join "..")
  let np = ($env.NIXPKGS? | default "/nix/store/ah3pxz4xa52hd6vhbx17vcsci0b8bmbr-source")
  let iets = ($env.IETS? | default "./target/release/iets" | path expand)
  let store = ($env.STORE? | default "ssh-ng://eu.nixbuild.net")
  let host = ($store | str replace -r '^ssh-ng://' '' | str replace -r '\?.*$' '')
  let runs = ($env.RUNS? | default "10")
  let wd = $"($env.PWD)/bench/nixbuild"
  mkdir bench/results
  let before = (usage $host)

  let nixb = $"nix build --eval-store auto --store ($store) --no-link --print-out-paths --impure -I nixpkgs=($np)"
  let ietsb = $"($iets) build --builder ($store) --no-cache --workers 4 --impure -I nixpkgs=($np)"
  let m = "--argstr marker \"$(date +%s%N)-$$\""
  let cmds = {|f, marker, fetch|
    let nix = if $fetch {
      $"p=$\(($nixb) -f ($wd)/($f) ($marker)) && nix copy --no-check-sigs --from ($store) $p"
    } else { $"($nixb) -f ($wd)/($f) ($marker)" }
    let ie = if $fetch {
      $"d=$\(mktemp -d) && ($ietsb) --fetch-to $d/out ($wd)/($f) ($marker); s=$?; rm -rf $d; exit $s"
    } else { $"($ietsb) ($wd)/($f) ($marker)" }
    [-n nix $nix -n "iets w4" $ie]
  }
  let shared = "--argstr marker shared"
  ^bash -c $"($nixb) -f ($wd)/small.nix ($shared)"
  let cases = [
    [name file marker fetch];
    [nixbuild-dispatch small.nix $m false]
    [nixbuild-dispatch-fetch small.nix $m true]
    [nixbuild-cachehit small.nix $shared false]
    [nixbuild-closure closure.nix $m false]
    [nixbuild-closure-fetch closure.nix $m true]
  ]
  for c in $cases {
    let out = $"bench/results/($c.name)"
    (^hyperfine --shell=bash --warmup 1 --runs $runs
      --export-markdown $"($out).md" --export-json $"($out).json"
      ...(do $cmds $c.file $c.marker $c.fetch))
  }

  let pr = ($env.PHASE_RUNS? | default "3" | into int)
  let phases = ($cases | each {|c|
    1..$pr | each {|i|
      let marker = $c.marker
      let n = (stamped $"($nixb) --log-format internal-json -f ($wd)/($c.file) ($marker)")
      let fetch_s = if $c.fetch {
        let p = ($n | where l starts-with "/nix/store/" | last | get l)
        let t = (date now)
        ^nix copy --no-check-sigs --from $store $p
        ((date now) - $t | into int) / 1e9
      } else { 0.0 }
      let d = (mktemp -d)
      let ie = (stamped $"($ietsb) --timings (if $c.fetch { $'--fetch-to ($d)/out' } else { '' }) ($wd)/($c.file) ($marker)")
      rm -rf $d
      [({ case: $c.name, client: nix, run: $i } | merge (nix-phases $n $fetch_s)), ({ case: $c.name, client: "iets w4", run: $i } | merge (iets-phases $ie))]
    } | flatten
  } | flatten)
  $phases | save -f bench/results/nixbuild-phases.json

  let after = (usage $host)
  [
    $"date: (date now | date to-timezone UTC | format date '%Y-%m-%dT%H:%M:%SZ')"
    $"host: (sys host | get hostname), (sys cpu | length) cores, (sys cpu | first | get brand), kernel (sys host | get kernel_version)"
    $"builder: ($store)"
    $"nix: (ver nix)"
    $"iets: ($env.IETS_COMMIT? | default 'commit unknown'), sha256 (open --raw $iets | hash sha256 | str substring 0..15)"
    $"nixpkgs: (open --raw ([$np .git-revision] | path join) | str trim)"
    $"hyperfine: (ver hyperfine), ($runs) runs, ($pr) phase runs"
    $"load: (open --raw /proc/loadavg | str trim)"
    $"account before: ($before)"
    $"account after: ($after)"
  ] | str join "\n" | save -f bench/results/nixbuild-env.txt
  print (open bench/results/nixbuild-env.txt)
}

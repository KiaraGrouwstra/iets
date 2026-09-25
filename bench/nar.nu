#!/usr/bin/env nu
# Usage: bench/nar.nu [--cold] [--nix] [--tag baseline]
#
# Times the NAR paths of src/store/nar.rs against `nix hash path` and
# `nix-store --dump`. Writes bench/results/nar-<tag>.nuon.
#
# --cold drops the page cache before each tree run; it needs root, so it is
# off by default. Fixtures live under $env.TMPDIR and are built once.

use ../scripts/lib.nu [require-real-nix]

const NIXPKGS = "/nix/store/ah3pxz4xa52hd6vhbx17vcsci0b8bmbr-source"

def fixtures [] {
  # Not $env.TMPDIR: `nix develop` points that at a directory it deletes, and
  # the fixtures cost minutes to rebuild.
  let base = ($env.NAR_BENCH_DIR? | default $"($env.HOME)/tmp/nar-bench")
  mkdir $base
  let tree = ($base | path join "tree")
  if not ($tree | path exists) {
    print $"building tree fixture in ($tree)"
    # The store tree is read-only; a mode-preserving copy cannot descend into
    # the directories it has just created.
    ^cp -r --no-preserve=mode $NIXPKGS $tree
  }
  let big = ($base | path join "big.bin")
  if not ($big | path exists) {
    print $"building 500 MB file fixture in ($big)"
    ^sh -c $"head -c 524288000 /dev/urandom > ($big)"
  }
  if (ls $big | get 0.size | into int) != 524288000 {
    error make { msg: $"($big) is the wrong size; delete it and retry" }
  }
  let nar = ($base | path join "tree.nar")
  if not ($nar | path exists) {
    print $"dumping ($tree) to ($nar)"
    ^nix-store --dump $tree | save -f $nar
  }
  { base: $base, tree: $tree, big: $big, nar: $nar }
}

def drop-caches [cold: bool] {
  if $cold {
    ^sync
    ^sudo sh -c "echo 3 > /proc/sys/vm/drop_caches"
  }
}

# One nar_bench run; returns the parsed metrics row.
def run-bench [bin: string, op: string, path: string, runs: int, cold: bool] {
  drop-caches $cold
  let out = (^$bin $op $path ($runs | into string) | complete)
  if $out.exit_code != 0 { error make { msg: $"nar_bench ($op) failed: ($out.stderr)" } }
  let line = ($out.stdout | lines | where {|l| not ($l | str starts-with "#") } | last)
  let f = ($line | split row " ")
  { op: $op, path: $path, best_ms: ($f.3 | into float), mean_ms: ($f.4 | into float), mbs: ($f.5 | into int) }
}

def main [--cold, --nix, --tag: string = "run"] {
  if $nix { require-real-nix nix }
  cd ($env.FILE_PWD | path join "..")
  let bin = ($env.NAR_BENCH? | default $"($env.CARGO_TARGET_DIR? | default "./target")/release/examples/nar_bench")
  if not ($bin | path exists) {
    error make { msg: $"build it first: cargo build --release --example nar_bench; missing ($bin)" }
  }
  let fx = (fixtures)
  mkdir bench/results

  mut rows = []
  # (a) NAR hash of the ~40k-file, ~300 MB nixpkgs tree.
  $rows = ($rows | append (run-bench $bin hash $fx.tree 3 $cold | insert case "tree-hash"))
  # (b) NAR hash of one 500 MB file.
  $rows = ($rows | append (run-bench $bin hash $fx.big 3 $cold | insert case "bigfile-hash"))
  $rows = ($rows | append (run-bench $bin flat $fx.big 3 $cold | insert case "bigfile-flat"))
  # dump with no hashing separates I/O and framing from SHA-256.
  $rows = ($rows | append (run-bench $bin dump $fx.tree 3 $cold | insert case "tree-dump"))

  # (c) restore of the tree NAR from a pipe.
  let restored = ($fx.base | path join "restore-dest")
  rm -rf $restored
  mkdir $restored
  drop-caches $cold
  let t = (date now)
  ^sh -c $"cat ($fx.nar) | ($bin) restore ($restored) 1"
  let ms = ((date now) - $t | into int) / 1000000
  $rows = ($rows | append { op: "restore", path: $fx.nar, best_ms: $ms, mean_ms: $ms, mbs: 0, case: "tree-restore" })
  rm -rf $restored

  # The parser on its own: the framing of the same NAR, contents thrown away.
  # This is the number that separates parse overhead from file system work.
  $rows = ($rows | append (run-bench $bin parse $fx.nar 3 $cold | insert case "tree-parse"))


  # (d) the store-path computation every `nix-build ./.` pays: "${./tree}".
  let ietsp = ($env.IETSP? | default $"($env.CARGO_TARGET_DIR? | default "./target")/release/ietsp")
  let expr = ($fx.base | path join "coerce.nix")
  $"\"${($fx.tree)}\"\n" | save -f $expr
  drop-caches $cold
  let t2 = (date now)
  ^$ietsp eval --raw --no-tracing-cache $expr | ignore
  let ms2 = ((date now) - $t2 | into int) / 1000000
  $rows = ($rows | append { op: "coerce", path: $expr, best_ms: $ms2, mean_ms: $ms2, mbs: 0, case: "tree-coerce" })

  # (e) the reference implementations on the same inputs.
  if $nix {
    for c in [[case, cmd]; ["ref-tree-nix-hash", ["nix" "hash" "path" $fx.tree]] ["ref-bigfile-nix-hash", ["nix" "hash" "path" $fx.big]]] {
      drop-caches $cold
      let t3 = (date now)
      ^($c.cmd | first) ...($c.cmd | skip 1) | ignore
      let ms3 = ((date now) - $t3 | into int) / 1000000
      $rows = ($rows | append { op: "reference", path: "", best_ms: $ms3, mean_ms: $ms3, mbs: 0, case: $c.case })
    }
    drop-caches $cold
    let t4 = (date now)
    ^sh -c $"nix-store --dump ($fx.tree) | sha256sum > /dev/null"
    let ms4 = ((date now) - $t4 | into int) / 1000000
    $rows = ($rows | append { op: "reference", path: "", best_ms: $ms4, mean_ms: $ms4, mbs: 0, case: "ref-tree-nix-store-dump" })
  }

  $rows | save -f $"bench/results/nar-($tag).nuon"
  $rows | select case best_ms mean_ms mbs
}

#!/usr/bin/env nu
# Usage: bench/hash.nu [op...]   (default: every op but `file`)
# The hash harnesses, pinned to one core so the numbers repeat.
# Set CPU to pick the core and FILE to give `file` something to hash.

def main [...ops: string] {
  cd ($env.FILE_PWD | path join "..")
  let cpu = ($env.CPU? | default "")
  let runs = ($env.RUNS? | default "7")
  let pin = {|argv| if ($cpu | is-empty) { ^$argv.0 ...($argv | skip 1) } else { ^taskset -c $cpu ...$argv }}
  for b in [hash_bench sha256_mb] {
    ^cargo build --release --example $b
  }
  let want = if ($ops | is-empty) { [algos encode path mix pair] } else { $ops }
  for op in $want {
    print $"## ($op)"
    if $op == "file" {
      let f = ($env.FILE? | default "")
      if ($f | is-empty) { print "  set FILE=<path>"; continue }
      do $pin [./target/release/examples/hash_bench file $runs $f]
    } else {
      do $pin [./target/release/examples/hash_bench $op $runs]
    }
  }
  print "## multi-buffer sha256"
  do $pin [./target/release/examples/sha256_mb $runs 1048576]
}

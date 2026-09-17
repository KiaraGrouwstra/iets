#!/usr/bin/env nu
# Usage: scripts/difftest.nu [-I nixpkgs=PATH] (-E EXPR | FILE)
# Runs nix-instantiate and ietsp on the same input and diffs stdout.
use lib.nu [chomp show diff-head env-cmd]

def --wrapped main [...args: string] {
  cd ($env.FILE_PWD | path join "..")
  let ietsp = ($env.IETSP? | default "./target/release/ietsp")
  let nix = (env-cmd NIX nix-instantiate)
  let n = (^($nix | first) ...($nix | skip 1) --eval --strict --option pure-eval false ...$args | complete)
  $n.stderr | save -f /tmp/difftest.nix.err
  let i = (^$ietsp eval --strict ...$args | complete)
  $i.stderr | save -f /tmp/difftest.ietsp.err
  if $n.exit_code != 0 and $i.exit_code != 0 { print "both failed"; exit 0 }
  if $n.exit_code != $i.exit_code {
    print $"exit code differs: nix=($n.exit_code) ietsp=($i.exit_code)"
    print "--- nix stderr"
    $n.stderr | lines | last 5 | show
    print "--- ietsp stderr"
    $i.stderr | lines | last 5 | show
    exit 1
  }
  let exp = ($n.stdout | chomp)
  let got = ($i.stdout | chomp)
  if $exp != $got {
    print "output differs"
    let tmp = (mktemp -d --tmpdir difftest.XXXXXX)
    $exp + "\n" | save -f $"($tmp)/exp"
    $got + "\n" | save -f $"($tmp)/got"
    diff-head $"($tmp)/exp" $"($tmp)/got" 40 | show
    rm -rf $tmp
    exit 1
  }
  print "ok"
}

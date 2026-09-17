#!/usr/bin/env nu
# Differential test of the parallel runtime: every case is evaluated with
# 1 and N workers (default 8); stdout, exit code and the trace lines on
# stderr must match. Usage: scripts/partest.nu [N]
use lib.nu [chomp show diff-head]

# One case with 1 and N workers; prints the verdict, returns it.
def run-case [ietsp: string, n: int, name: string, expr: string]: nothing -> bool {
  let a = (^$ietsp eval --strict --workers 1 -E $expr | complete)
  $a.stderr | save -f /tmp/partest.1.err
  let b = (^$ietsp eval --strict --workers $n -E $expr | complete)
  $b.stderr | save -f /tmp/partest.2.err
  let o1 = ($a.stdout | chomp)
  let o2 = ($b.stdout | chomp)
  let e1 = ($a.stderr | lines | where $it =~ '^(trace|evaluation warning)' | sort | str join "\n")
  let e2 = ($b.stderr | lines | where $it =~ '^(trace|evaluation warning)' | sort | str join "\n")
  if $a.exit_code != $b.exit_code or $o1 != $o2 or $e1 != $e2 {
    print $"FAIL ($name): rc ($a.exit_code)/($b.exit_code)"
    let tmp = (mktemp -d --tmpdir partest.XXXXXX)
    $"($o1)\n($e1)\n" | save -f $"($tmp)/1"
    $"($o2)\n($e2)\n" | save -f $"($tmp)/2"
    diff-head $"($tmp)/1" $"($tmp)/2" 10 | show
    rm -rf $tmp
    false
  } else {
    print $"ok   ($name) \(rc=($a.exit_code)\)"
    true
  }
}

def main [n: int = 8] {
  cd ($env.FILE_PWD | path join "..")
  let ietsp = ($env.IETSP? | default "./target/release/ietsp")
  # the second run of each case must evaluate again to print its traces
  $env.IETSP_CACHE = "0"
  let cases = [
    # Shared thunk graph forced from many fibers: fib-style DAG with sharing.
    [shared-graph r#'let
  n = 400;
  m = 1000003;
  g = builtins.genList (i: if i < 2 then i else (builtins.elemAt g (i - 1) + builtins.elemAt g (i - 2) + (if i > 100 then builtins.elemAt g (i - 100) else 0)) - (builtins.elemAt g (i - 1) + builtins.elemAt g (i - 2) + (if i > 100 then builtins.elemAt g (i - 100) else 0)) / m * m) n;
  rows = builtins.genList (r: map (i: builtins.elemAt g ((i * 7 + r) - (i * 7 + r) / n * n)) (builtins.genList (x: x) 64)) 64;
in builtins.foldl' (a: b: a + builtins.foldl' (x: y: x + y) 0 b) 0 rows'#]
    # Deep nested structure, deep fan-out from every level.
    [deep-attrs r#'let mk = d: if d == 0 then { v = 1; } else builtins.listToAttrs (map (i: { name = "k${toString i}"; value = mk (d - 1); }) [ 0 1 2 3 4 ]); in mk 5'#]
    # A cycle: fan-out may claim both halves on two fibers; must still report
    # infinite recursion instead of hanging.
    [cycle r#'let x = y; y = x; in [ x y x y x y ]'#]
    # x and y sit in different fan-out ranges so two helpers can claim the
    # two halves of the cycle and park on each other: deadlock resolution.
    [cycle-two-ranges r#'let x = y + 1; y = x + 1; in [ x 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 y 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 x ]'#]
    [cycle-deep r#'let a = { p = b.q; }; b = { q = a.p; }; in [ a.p b.q 1 2 ]'#]
    # Errors: same exit code and same first error.
    [error-elem r#'map (i: if i == 3 then throw "boom" else i) [ 1 2 3 4 5 6 ]'#]
    [try-eval r#'map (i: (builtins.tryEval (if i == 3 then throw "boom" else i)).value) [ 1 2 3 4 5 6 ]'#]
    # Effects: every trace exactly once.
    [trace-once r#'map (i: builtins.trace "t${toString i}" (i * 2)) [ 1 2 3 4 5 6 7 8 9 10 ]'#]
    [warn-once r#'builtins.concatStringsSep "," (map (i: builtins.warn "w${toString i}" (toString i)) [ 1 2 3 4 5 6 ])'#]
    # Imports from many fibers (same file, concurrently).
    [import-shared r#'let f = import ./tests/lang/eval-okay-arithmetic.nix; in map (i: f) [ 1 2 3 4 5 6 7 8 ]'#]
    # Derivations: same drvPath from helpers and main.
    [drv r#'let d = derivation { name = "x"; builder = "/bin/sh"; system = "x86_64-linux"; args = map (i: toString i) [ 1 2 3 4 5 ]; }; in map (i: d.drvPath) [ 1 2 3 4 5 6 ]'#]
    [json r#'builtins.toJSON (builtins.genList (i: { a = i; b = builtins.genList (j: i * j) 8; }) 40)'#]
  ]
  let ok = ($cases | each {|c| run-case $ietsp $n $c.0 $c.1 })
  if ($ok | any {|r| not $r }) { exit 1 }
}

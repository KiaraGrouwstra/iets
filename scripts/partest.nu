#!/usr/bin/env nu
# Differential test of the parallel runtime: every case is evaluated with
# 1, 2, 4 and N workers (default 8); stdout, exit code and the trace lines
# on stderr must match. A case marked `nix` is also compared with the real
# nix-instantiate. Usage: scripts/partest.nu [N]
use lib.nu [chomp diff-head ietsp-bin show require-real-nix]

# stdout, exit code and the sorted effect lines of one run.
def outcome [res: record]: nothing -> record {
  {
    out: ($res.stdout | chomp),
    rc: $res.exit_code,
    eff: ($res.stderr | lines | where $it =~ '^(trace|evaluation warning)' | sort | str join "\n"),
  }
}

def report [name: string, a: record, b: record, what: string]: nothing -> nothing {
  print $"FAIL ($name) [($what)]: rc ($a.rc)/($b.rc)"
  let tmp = (mktemp -d --tmpdir partest.XXXXXX)
  $"($a.out)\n($a.eff)\n" | save -f $"($tmp)/1"
  $"($b.out)\n($b.eff)\n" | save -f $"($tmp)/2"
  diff-head $"($tmp)/1" $"($tmp)/2" 10 | show
  rm -rf $tmp
}

# One case at every worker count, and against nix when the case asks.
def run-case [ietsp: string, counts: list<int>, c: record]: nothing -> bool {
  let flags = (if $c.strict { [--strict] } else { [] })
  let runs = ($counts | each {|w|
    { w: $w, o: (outcome (^$ietsp eval ...$flags --workers $w -E $c.expr | complete)) }
  })
  let base = ($runs | first)
  mut ok = true
  for r in ($runs | skip 1) {
    if $r.o != $base.o {
      report $"($c.name) w($r.w)" $base.o $r.o $"1 vs ($r.w) workers"
      $ok = false
    }
  }
  if $c.nix {
    let n = (outcome (^nix-instantiate --eval ...$flags -E $c.expr | complete))
    if $n.out != $base.o.out or $n.rc != $base.o.rc {
      report $c.name $base.o $n "ietsp vs nix"
      $ok = false
    }
  }
  if $ok { print $"ok   ($c.name) \(rc=($base.o.rc)\)" }
  $ok
}

def main [n: int = 8] {
  cd ($env.FILE_PWD | path join "..")
  let ietsp = (ietsp-bin)
  # the second run of each case must evaluate again to print its traces
  $env.IETSP_CACHE = "0"
  # a slow pure computation: it keeps the demand path busy while helpers run
  let slow = "slow = n: builtins.foldl' (a: b: a + b) 0 (builtins.genList (x: x) n);"
  let cases = [
    # Shared thunk graph forced from many fibers: fib-style DAG with sharing.
    [name strict nix expr];
    [shared-graph true false r#'let
  n = 400;
  m = 1000003;
  g = builtins.genList (i: if i < 2 then i else (builtins.elemAt g (i - 1) + builtins.elemAt g (i - 2) + (if i > 100 then builtins.elemAt g (i - 100) else 0)) - (builtins.elemAt g (i - 1) + builtins.elemAt g (i - 2) + (if i > 100 then builtins.elemAt g (i - 100) else 0)) / m * m) n;
  rows = builtins.genList (r: map (i: builtins.elemAt g ((i * 7 + r) - (i * 7 + r) / n * n)) (builtins.genList (x: x) 64)) 64;
in builtins.foldl' (a: b: a + builtins.foldl' (x: y: x + y) 0 b) 0 rows'#]
    # Deep nested structure, deep fan-out from every level.
    [deep-attrs true false r#'let mk = d: if d == 0 then { v = 1; } else builtins.listToAttrs (map (i: { name = "k${toString i}"; value = mk (d - 1); }) [ 0 1 2 3 4 ]); in mk 5'#]
    # A cycle: fan-out may claim both halves on two fibers; must still report
    # infinite recursion instead of hanging.
    [cycle true false r#'let x = y; y = x; in [ x y x y x y ]'#]
    # x and y sit in different fan-out ranges so two helpers can claim the
    # two halves of the cycle and park on each other: deadlock resolution.
    [cycle-two-ranges true false r#'let x = y + 1; y = x + 1; in [ x 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 y 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 x ]'#]
    [cycle-deep true false r#'let a = { p = b.q; }; b = { q = a.p; }; in [ a.p b.q 1 2 ]'#]
    # Errors: same exit code and same first error.
    [error-elem true false r#'map (i: if i == 3 then throw "boom" else i) [ 1 2 3 4 5 6 ]'#]
    [try-eval true false r#'map (i: (builtins.tryEval (if i == 3 then throw "boom" else i)).value) [ 1 2 3 4 5 6 ]'#]
    # Effects: every trace exactly once.
    [trace-once true false r#'map (i: builtins.trace "t${toString i}" (i * 2)) [ 1 2 3 4 5 6 7 8 9 10 ]'#]
    [warn-once true false r#'builtins.concatStringsSep "," (map (i: builtins.warn "w${toString i}" (toString i)) [ 1 2 3 4 5 6 ])'#]
    # Imports from many fibers (same file, concurrently).
    [import-shared true false r#'let f = import ./tests/lang/eval-okay-arithmetic.nix; in map (i: f) [ 1 2 3 4 5 6 7 8 ]'#]
    # Derivations: same drvPath from helpers and main.
    [drv true false r#'let d = derivation { name = "x"; builder = "/bin/sh"; system = "x86_64-linux"; args = map (i: toString i) [ 1 2 3 4 5 ]; }; in map (i: d.drvPath) [ 1 2 3 4 5 6 ]'#]
    [json true false r#'builtins.toJSON (builtins.genList (i: { a = i; b = builtins.genList (j: i * j) 8; }) 40)'#]
    # A helper that unboxes a cell would make two lists of one function
    # unequal: the identity of a function is the word before forcing.
    [fn-identity true true $"let ($slow) t = \(x: y: x\) 1; A = [ t t t t t t t t ]; B = [ t t t t t t t t ]; in builtins.seq \(builtins.any \(x: false\) A\) \(builtins.seq \(slow 3000000\) \(A == B\)\)"]
    # A thunk only a helper forced still prints as <CODE>.
    [print-code false true $"let ($slow) l = map \(x: x + slow 20000\) [ 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 ]; in builtins.seq \(builtins.any \(x: x > 0\) l\) l"]
    # A NaN is not equal to itself, and keeps its sign when printed.
    ["nan" true true r#'let inf = 1.0e308 * 10.0; nan = inf - inf; in [ (nan == nan) nan (0.0 - nan) inf (-inf) ]'#]
  ]
  if ($cases | any {|c| $c.nix }) { require-real-nix nix-instantiate }
  let counts = ([1 2 4 $n] | uniq)
  let ok = ($cases | each {|c| run-case $ietsp $counts $c })
  if ($ok | any {|r| not $r }) { exit 1 }
}

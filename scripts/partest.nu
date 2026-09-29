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
    eff: ($res.stderr | lines | where $it =~ '^(trace|evaluation warning|warning: \[)' | sort | str join "\n"),
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

# One case at every worker count, and against nix when the case asks for
# it and a real nix is there. The sandbox has none, so the comparison is
# what a run by hand adds.
def run-case [ietsp: string, counts: list<int>, c: record, with_nix: bool]: nothing -> bool {
  let base_flags = (if $c.strict { [--strict] } else { [] } | append ($c.flags? | default []))
  let runs = ($counts | each {|w|
    # a `.drv` a helper wrote is an effect the program did not ask for, so
    # the file count is part of the outcome
    let dir = (if ($c.dump? | default false) { mktemp -d --tmpdir partest.drv.XXXXXX } else { null })
    let flags = (if $dir == null { $base_flags } else { $base_flags | append [--dump-drvs $dir] })
    let o = (outcome (^$ietsp eval ...$flags --workers $w -E $c.expr | complete))
    let o = (if $dir == null { $o } else { $o | insert drvs (ls $dir | length) })
    if $dir != null { rm -rf $dir }
    { w: $w, o: $o }
  })
  let base = ($runs | first)
  mut ok = true
  for r in ($runs | skip 1) {
    if $r.o != $base.o {
      report $"($c.name) w($r.w)" $base.o $r.o $"1 vs ($r.w) workers"
      $ok = false
    }
  }
  if $c.nix and $with_nix {
    let n = (outcome (^nix-instantiate --eval ...$base_flags -E $c.expr | complete))
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
  # `elem` returns at the first element, so nothing below it is demanded
  let stops = $"let ($slow) d = i: \(derivation { name = \"d${toString i}\"; builder = \"/bin/sh\"; system = \"x86_64-linux\"; }\).drvPath;"
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
    # A chain deeper than a helper stack, and deeper than nix takes: the
    # demand path has the stack of the sequential run, and a helper that
    # runs out of stack drops its task.
    [deep-chain true false r#'let xs = builtins.genList (i: i) 200000; r = builtins.foldl' (acc: x: { v = acc; }) null xs; in builtins.deepSeq r 1'#]
    # A thunk many fan-out cells share and that fails: one failure stops the
    # speculation instead of starting it again from every task.
    [failing-shared true true $"let ($slow) bad = builtins.seq \(slow 100000\) \(throw \"bad\"\); l = builtins.genList \(i: bad + i\) 20000; in builtins.foldl' \(a: b: a + b\) 0 l"]
  ]
  # Effects of work the program never asks for: a lint finding and a
  # dumped .drv must follow demand, not the scheduler.
  let effect_cases = [
    [name strict nix flags dump expr];
    [lint-speculated true false [--lint eq-type-mismatch=warn] false
      $"let ($slow) in builtins.any \(x: x\) [ \(slow 3000000 > 0\) \(1 == \"a\"\) \(2 == \"b\"\) \(3 == \"c\"\) \(4 == \"d\"\) ]"]
    [lint-demanded true false [--lint eq-type-mismatch=warn] false
      $"let ($slow) in builtins.all \(x: x == false\) [ \(slow 2000000 > 0 && false\) \(1 == \"a\"\) \(2 == \"b\"\) \(3 == \"c\"\) \(4 == \"d\"\) ]"]
    [drv-speculated true false [] true $"($stops) in builtins.elem 1 [ \(slow 3000000 * 0 + 1\) \(builtins.stringLength \(d 1\)\) \(builtins.stringLength \(d 2\)\) \(builtins.stringLength \(d 3\)\) ]"]
    [drv-demanded true false [] true $"($stops) in map \(i: builtins.stringLength \(d i\)\) [ 1 2 3 4 5 6 7 8 ]"]
  ]
  let cases = ($cases | each {|c| $c | insert flags [] | insert dump false } | append $effect_cases)
  # The sandbox this runs in for `nix flake check` has no nix at all, so
  # the comparison against the reference is what a run by hand adds.
  let with_nix = ((which nix-instantiate | length) > 0)
  if $with_nix {
    require-real-nix nix-instantiate
  } else {
    print "note: no nix-instantiate on PATH; comparing worker counts only"
  }
  let counts = ([1 2 4 $n] | uniq)
  let ok = ($cases | each {|c| run-case $ietsp $counts $c $with_nix })
  if ($ok | any {|r| not $r }) { exit 1 }
}

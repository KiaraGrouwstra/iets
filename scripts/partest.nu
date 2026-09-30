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
    # The same, where every thunk sits in exactly one cell: `genList` puts
    # its thunk in the result cell and nothing copies it, because `any`
    # stops at the first element and never passes the others to the
    # predicate. Knowing that a thunk reaches one cell therefore does not
    # make unboxing it invisible: the cell it does reach is the one the
    # printer reads (docs/explanation/evaluation.md, "One cell is not enough").
    [print-code-single false true $"let ($slow) l = builtins.genList \(i: slow 20000 * 0 + i + 1\) 20; in builtins.seq \(builtins.any \(x: x > 0\) l\) l"]
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
  # Every route by which one thunk's word reaches a second cell. `pre`
  # binds the shape, `a` and `b` build two cells from the one thunk `t`
  # through that route. `any` fans the first list out so the helpers force
  # `t`, `slow` gives them the time, and `==` then reads both cells: the
  # word before forcing decides the identity of a function, so a cell a
  # helper unboxed is not equal to one it left alone. Every route needs its
  # own case, because a route nothing marks is a silent return of the bug
  # and the routes that are marked would not show it.
  let routes = [
    [name pre a b];
    [local "" "[ t t t t t t t t ]" "[ t t t t t t t t ]"]
    [select "s = { inherit t; };"
      "[ s.t s.t s.t s.t s.t s.t s.t s.t ]" "[ s.t s.t s.t s.t s.t s.t s.t s.t ]"]
    [apply "f = x: [ x x x x x x x x ];" "f t" "f t"]
    [concat "L = [ t t t t ]; M = [ t t t t ];" "L ++ M" "L ++ M"]
    [tail "L = [ 0 t t t t t t t t ];" "builtins.tail L" "builtins.tail L"]
    [filter "L = [ t 1 t 2 t 3 t 4 t 5 t 6 t 7 t 8 ];"
      "builtins.filter (x: !(builtins.isInt x)) L" "builtins.filter (x: !(builtins.isInt x)) L"]
    [attr-values "s = { a = t; b = t; c = t; d = t; e = t; f = t; g = t; h = t; };"
      "builtins.attrValues s" "builtins.attrValues s"]
    [cat-attrs "l = builtins.genList (i: { k = t; }) 8;"
      "builtins.catAttrs \"k\" l" "builtins.catAttrs \"k\" l"]
    [list-to-attrs "l = builtins.genList (i: { name = \"k${toString i}\"; value = t; }) 8;"
      "builtins.attrValues (builtins.listToAttrs l)"
      "builtins.attrValues (builtins.listToAttrs l)"]
    [update "s = { a = t; b = t; c = t; d = t; e = t; f = t; g = t; h = t; };"
      "builtins.attrValues (s // { z = 1; })" "builtins.attrValues (s // { z = 1; })"]
    [inherit-from "s = { a = t; b = t; c = t; d = t; e = t; f = t; g = t; h = t; };"
      "builtins.attrValues { inherit (s) a b c d e f g h; }"
      "builtins.attrValues { inherit (s) a b c d e f g h; }"]
    [rec-attrs "" "builtins.attrValues (rec { a = t; b = t; c = t; d = t; e = t; f = t; g = t; h = t; })"
      "builtins.attrValues (rec { a = t; b = t; c = t; d = t; e = t; f = t; g = t; h = t; })"]
    [map-arg "L = [ t t t t t t t t ];"
      "builtins.concatLists (builtins.map (x: [ x ]) L)"
      "builtins.concatLists (builtins.map (x: [ x ]) L)"]
    # one `mapAttrs` result, read twice: a second call would make fresh map
    # words and the two sides would differ whatever the workers do
    [map-attrs "s = { a = t; b = t; c = t; d = t; e = t; f = t; g = t; h = t; }; m = builtins.attrValues (builtins.mapAttrs (n: v: v) s);"
      "builtins.tail ([ 0 ] ++ m)" "builtins.tail ([ 0 ] ++ m)"]
    [zip-attrs "s = { a = t; };"
      "(builtins.zipAttrsWith (n: vs: vs) [ s s s s s s s s ]).a"
      "(builtins.zipAttrsWith (n: vs: vs) [ s s s s s s s s ]).a"]
    [with-scope "s = { a = t; b = t; c = t; d = t; };"
      "with s; [ a b c d a b c d ]" "with s; [ a b c d a b c d ]"]
    [remove-attrs "s = { a = t; b = t; c = t; d = t; e = t; f = t; g = t; h = t; z = 1; };"
      "builtins.attrValues (builtins.removeAttrs s [ \"z\" ])"
      "builtins.attrValues (builtins.removeAttrs s [ \"z\" ])"]
    [intersect-attrs "s = { a = t; b = t; c = t; d = t; e = t; f = t; g = t; h = t; };"
      "builtins.attrValues (builtins.intersectAttrs s s)"
      "builtins.attrValues (builtins.intersectAttrs s s)"]
    [foldl "L = [ t t t t t t t t ];"
      "builtins.foldl' (acc: x: acc ++ [ x ]) [] L" "builtins.foldl' (acc: x: acc ++ [ x ]) [] L"]
  ]
  # One thunk shared through many routes at once: the stress case the guard
  # runs under.
  let stress = "L = [ t t t t ]; s = { a = t; b = t; c = t; d = t; }; W = (builtins.tail ([ 0 ] ++ L)) ++ (builtins.attrValues s) ++ (builtins.attrValues { inherit (s) a b c d; }) ++ (builtins.concatLists (builtins.map (x: [ x ]) L)) ++ (builtins.attrValues (builtins.mapAttrs (n: v: v) s)) ++ (builtins.filter (x: !(builtins.isInt x)) [ t 1 t 2 ]) ++ (with s; [ a b c d ]) ++ (builtins.attrValues (s // { z = 1; })) ++ (builtins.attrValues (rec { e = t; f = t; })) ++ [ s.a s.b ];"
  # two copies of one list, so every cell of both sides holds a word that
  # came through one of the routes above
  let stress_a = "builtins.tail ([ 0 ] ++ W)"
  let route_cases = ($routes | each {|r|
    {
      name: $"route-($r.name)",
      strict: true,
      nix: false,
      flags: [],
      dump: false,
      expr: $"let ($slow) t = \(x: y: x\) 1; ($r.pre) A = ($r.a); B = ($r.b); in builtins.seq \(builtins.any \(x: false\) A\) \(builtins.seq \(slow 3000000\) \(A == B\)\)",
    }
  } | append {
    name: "route-stress",
    strict: true,
    nix: false,
    flags: [],
    dump: false,
    expr: $"let ($slow) t = \(x: y: x\) 1; ($stress) A = ($stress_a); B = ($stress_a); in builtins.seq \(builtins.any \(x: false\) A\) \(builtins.seq \(slow 3000000\) \(A == B\)\)",
  })
  let cases = ($cases | each {|c| $c | insert flags [] | insert dump false } | append $effect_cases | append $route_cases)
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

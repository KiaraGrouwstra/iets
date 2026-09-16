#!/usr/bin/env bash
# Differential test of the parallel runtime: every case is evaluated with
# 1 and N workers (default 8); stdout, exit code and the trace lines on
# stderr must match. Usage: scripts/partest.sh [N]
set -uo pipefail
cd "$(dirname "$0")/.."
IETSP=${IETSP:-./target/release/ietsp}
# the second run of each case must evaluate again to print its traces
export IETSP_CACHE=0
N=${1:-8}
fail=0
run() { # name expr
  local name=$1 expr=$2 o1 o2 e1 e2 r1 r2
  o1=$("$IETSP" eval --strict --workers 1 -E "$expr" 2>/tmp/partest.1.err); r1=$?
  o2=$("$IETSP" eval --strict --workers "$N" -E "$expr" 2>/tmp/partest.2.err); r2=$?
  e1=$(grep -E '^(trace|evaluation warning)' /tmp/partest.1.err | sort)
  e2=$(grep -E '^(trace|evaluation warning)' /tmp/partest.2.err | sort)
  if [ "$r1" != "$r2" ] || [ "$o1" != "$o2" ] || [ "$e1" != "$e2" ]; then
    echo "FAIL $name: rc $r1/$r2"
    diff <(echo "$o1"; echo "$e1") <(echo "$o2"; echo "$e2") | head -10
    fail=1
  else
    echo "ok   $name (rc=$r1)"
  fi
}
# Shared thunk graph forced from many fibers: fib-style DAG with sharing.
run shared-graph 'let
  n = 400;
  m = 1000003;
  g = builtins.genList (i: if i < 2 then i else (builtins.elemAt g (i - 1) + builtins.elemAt g (i - 2) + (if i > 100 then builtins.elemAt g (i - 100) else 0)) - (builtins.elemAt g (i - 1) + builtins.elemAt g (i - 2) + (if i > 100 then builtins.elemAt g (i - 100) else 0)) / m * m) n;
  rows = builtins.genList (r: map (i: builtins.elemAt g ((i * 7 + r) - (i * 7 + r) / n * n)) (builtins.genList (x: x) 64)) 64;
in builtins.foldl'"'"' (a: b: a + builtins.foldl'"'"' (x: y: x + y) 0 b) 0 rows'
# Deep nested structure, deep fan-out from every level.
run deep-attrs 'let mk = d: if d == 0 then { v = 1; } else builtins.listToAttrs (map (i: { name = "k${toString i}"; value = mk (d - 1); }) [ 0 1 2 3 4 ]); in mk 5'
# A cycle: fan-out may claim both halves on two fibers; must still report
# infinite recursion instead of hanging.
run cycle 'let x = y; y = x; in [ x y x y x y ]'
# x and y sit in different fan-out ranges so two helpers can claim the
# two halves of the cycle and park on each other: deadlock resolution.
run cycle-two-ranges 'let x = y + 1; y = x + 1; in [ x 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 y 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 x ]'
run cycle-deep 'let a = { p = b.q; }; b = { q = a.p; }; in [ a.p b.q 1 2 ]'
# Errors: same exit code and same first error.
run error-elem 'map (i: if i == 3 then throw "boom" else i) [ 1 2 3 4 5 6 ]'
run try-eval 'map (i: (builtins.tryEval (if i == 3 then throw "boom" else i)).value) [ 1 2 3 4 5 6 ]'
# Effects: every trace exactly once.
run trace-once 'map (i: builtins.trace "t${toString i}" (i * 2)) [ 1 2 3 4 5 6 7 8 9 10 ]'
run warn-once 'builtins.concatStringsSep "," (map (i: builtins.warn "w${toString i}" (toString i)) [ 1 2 3 4 5 6 ])'
# Imports from many fibers (same file, concurrently).
run import-shared 'let f = import ./tests/lang/eval-okay-arithmetic.nix; in map (i: f) [ 1 2 3 4 5 6 7 8 ]'
# Derivations: same drvPath from helpers and main.
run drv 'let d = derivation { name = "x"; builder = "/bin/sh"; system = "x86_64-linux"; args = map (i: toString i) [ 1 2 3 4 5 ]; }; in map (i: d.drvPath) [ 1 2 3 4 5 6 ]'
run json 'builtins.toJSON (builtins.genList (i: { a = i; b = builtins.genList (j: i * j) 8; }) 40)'
exit $fail

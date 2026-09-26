#!/usr/bin/env nu
# End-to-end test of `ietsp build` over an attribute tree, against the local
# nix-daemon.
#
# The flake has `checks.<system>` for two systems with four tiny /bin/sh
# derivations: `ok`, `dep` (which `chain` reads) and `bad` (which exits 1);
# plus a `packages` set. Nothing needs nixpkgs.
#
# 1. DIR#checks: the current system's checks are built, the failure does not
#    stop the others, the exit code is Nix's 100, stdout has the paths built.
# 2. --json: the `plumbing build` shape, one outcome per check.
# 3. the same build again: nothing is built twice.
# 4. one root: the failing check alone, and a set of one package.
# 5. a plain Nix file with -A and --argstr.
# 6. --dry-run over a tree.
# 7. an attribute that does not evaluate, and a tree with nothing in it.
#
# usage: nu scripts/build-e2e.nu
#        BIN=target/debug nu scripts/build-e2e.nu
use lib.nu [current-system fastbuild-flake ietsp-bin isolate verdict]

def --wrapped go [...cmd: string]: nothing -> record {
  ^($cmd | first) ...($cmd | skip 1) | complete
}

def main [] {
  let ietsp = (ietsp-bin)
  let tmp = (isolate build-e2e)
  let system = (current-system)
  let flake = $"($tmp)/flake"
  fastbuild-flake $flake
  let checks = $"path:($flake)#checks"
  let names = ([bad chain dep ok] | each {|n| $"checks.($system).($n)" })
  mut ok = []

  print "== 1. build path:DIR#checks"
  let r1 = (go $ietsp build --builder daemon $checks)
  let paths = ($r1.stdout | lines)
  $ok = ($ok | append (verdict ($r1.exit_code == 100) "a failing check exits 100" $"exit ($r1.exit_code)"))
  $ok = ($ok | append (verdict ($r1.stderr =~ $"checks.($system).bad failed") "the failing attribute is named" $r1.stderr))
  $ok = ($ok | append (verdict ($r1.stderr =~ 'about to fail') "the failure log tail is printed" $r1.stderr))
  $ok = ($ok | append (verdict ($r1.stderr =~ 'building fb-chain') "the checks after the failure built" $r1.stderr))
  $ok = ($ok | append (verdict ($r1.stderr =~ $"3 built, 1 failed: checks.($system).bad") "the closing line counts them" $r1.stderr))
  $ok = ($ok | append (verdict (($paths | each {|p| $p | str replace -r '^/nix/store/[^-]+-' '' }) == [fb-chain fb-dep fb-ok]) "stdout has one path per output built" $r1.stdout))
  $ok = ($ok | append (verdict ($paths | all {|p| $p | path exists }) "every path is there" $r1.stdout))
  $ok = ($ok | append (verdict (not ($r1.stderr =~ 'aarch64-linux')) "the other system is not evaluated into the build" $r1.stderr))

  print "== 2. --json"
  let r2 = (go $ietsp build --builder daemon --json $checks)
  let j = ($r2.stdout | from json)
  let bad = ($j.outcomes | where attr == $"checks.($system).bad" | first)
  let goods = ($j.outcomes | where attr != $"checks.($system).bad")
  $ok = ($ok | append (verdict ($r2.exit_code == 100) "--json keeps the exit code" $"exit ($r2.exit_code)"))
  $ok = ($ok | append (verdict (($j.outcomes | get attr) == $names) "one outcome per check, by attribute" $"($j.outcomes | get attr)"))
  $ok = ($ok | append (verdict (($bad | columns | sort) == [attr drv logTail outputs reason success]) "the outcome has the plumbing keys" $"($bad | columns)"))
  $ok = ($ok | append (verdict ((not $bad.success) and ($bad.outputs == {}) and ($bad.reason =~ 'fb-bad.drv')) "the failure has a reason and no output" $"($bad)"))
  $ok = ($ok | append (verdict ($bad.logTail | any {|l| $l =~ 'about to fail' }) "the failure has its log tail" $"($bad.logTail)"))
  $ok = ($ok | append (verdict ($goods | all {|o| $o.success and ($o.outputs.out in $paths) }) "the others have the paths of case 1" $"($goods)"))

  print "== 3. what is built is not built again"
  $ok = ($ok | append (verdict (not ($r2.stderr =~ 'building fb-(ok|dep|chain)')) "no second build of a valid output" $r2.stderr))
  $ok = ($ok | append (verdict ($r2.stderr =~ 'about to fail') "the failure is tried again" $r2.stderr))

  print "== 4. one root"
  let r4 = (go $ietsp build --builder daemon $"($checks).($system).bad")
  $ok = ($ok | append (verdict (($r4.exit_code == 100) and ($r4.stdout == "")) "the failing check alone: exit 100, no path" $"exit ($r4.exit_code) ($r4.stdout)"))
  $ok = ($ok | append (verdict (not ($r4.stderr =~ ' built, ')) "one root has no closing line" $r4.stderr))
  let r4p = (go $ietsp build --builder daemon $"path:($flake)#packages")
  $ok = ($ok | append (verdict (($r4p.exit_code == 0) and ($r4p.stdout =~ '-fb-lone\n$')) "#packages builds its one member" $"exit ($r4p.exit_code) ($r4p.stdout)"))
  let r4o = (go $ietsp build --builder daemon $"path:($flake)#checks.aarch64-linux" --dry-run --json)
  $ok = ($ok | append (verdict (($r4o.stdout | from json | get willBuild | length) == 4) "a system named in full is walked as it is" $r4o.stdout))

  print "== 5. a file, -A and --argstr"
  let rel = $"($tmp)/release.nix"
  $"{ system ? builtins.currentSystem }:
let f = import ($flake)/flake.nix; o = f.outputs { self = null; };
in { ci = o.checks.${system}; }
" | save -f $rel
  let r5 = (go $ietsp build --builder daemon $rel -A ci --argstr system $system -k)
  $ok = ($ok | append (verdict ($r5.exit_code == 100) "-A ci reaches the failing check" $"exit ($r5.exit_code)"))
  $ok = ($ok | append (verdict ($r5.stderr =~ 'ci.bad failed') "the failure is named by its attribute path" $r5.stderr))
  $ok = ($ok | append (verdict (($r5.stdout | lines) == $paths) "the same paths as the flake gave" $r5.stdout))

  print "== 6. --dry-run over a tree"
  fastbuild-flake $"($tmp)/fresh"
  let r6 = (go $ietsp build --builder daemon --dry-run --json $"path:($tmp)/fresh#checks")
  let m = ($r6.stdout | from json)
  $ok = ($ok | append (verdict (($r6.exit_code == 0) and (($m.willBuild | length) == 4)) "the plan has the four checks" $r6.stdout))

  print "== 7. no verdict without an evaluation"
  let r7 = (go $ietsp build --builder daemon --no-tracing-cache -E $"let f = import ($tmp)/fresh/flake.nix; in \(f.outputs { self = null; }\).checks.($system) // { broken = throw \"boom\"; }")
  $ok = ($ok | append (verdict (($r7.exit_code == 1) and ($r7.stdout == "") and ($r7.stderr =~ 'boom')) "an attribute that throws stops the run" $"exit ($r7.exit_code) ($r7.stderr)"))
  $ok = ($ok | append (verdict (not ($r7.stderr =~ 'building fb-')) "nothing was built" $r7.stderr))
  let r7e = (go $ietsp build --builder daemon -E '{ a = { }; }')
  $ok = ($ok | append (verdict (($r7e.exit_code == 1) and ($r7e.stderr =~ 'nothing to build')) "a tree with no derivation is an error" $"exit ($r7e.exit_code) ($r7e.stderr)"))

  ^rm -rf $tmp
  let failed = ($ok | where $it == false | length)
  print $"($ok | length) cases, ($failed) failed"
  if $failed > 0 { exit 1 }
  print "PASS"
}

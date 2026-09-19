#!/usr/bin/env nu
# End-to-end test of `ietsp fast-build` against the local nix-daemon, and a
# comparison with the real nix-fast-build on the same flake.
#
# The flake has `checks.<system>` with three tiny /bin/sh derivations: `ok`,
# `dep` (which `chain` reads) and `bad` (which exits 1); plus a `packages` set
# so `--flake DIR#packages` has something to walk. Nothing needs nixpkgs.
#
# 1. --flake path:DIR#checks against the local daemon: exit 1, the failing
#    attribute named in the summary, the successes built.
# 2. --result-file json: well-formed, the right attribute set and outcomes.
# 3. --result-file junit: well-formed, the right counts, the log tail on the
#    failure.
# 4. --out-link: one symlink per successful attribute, none for the failure.
# 5. --fail-fast: the attributes after the failure are reported skipped.
# 6. --stream-json-lines: every line on stdout parses as JSON.
# 7. --systems with a system nothing is built for: no build.
# 8. the flags nix-fast-build has and ietsp refuses, and an unknown flag.
# 9. the same flake through the real nix-fast-build, comparing the JSON
#    result file's attributes and per-attribute outcomes (not timings).
#    Skipped with a note when nix-fast-build is not available.
#
# usage: nu scripts/fastbuild-e2e.nu
#        BIN=target/debug nu scripts/fastbuild-e2e.nu
use lib.nu [chomp current-system fastbuild-flake ietsp-bin isolate show verdict]

def --wrapped go [...cmd: string]: nothing -> record {
  ^($cmd | first) ...($cmd | skip 1) | complete
}

def main [] {
  let ietsp = (ietsp-bin)
  let tmp = (isolate fastbuild-e2e)
  let system = (current-system)
  let flake = $"($tmp)/flake"
  fastbuild-flake $flake
  let target = $"path:($flake)#checks"
  mut ok = []

  # --- 1. the plain run: exit 1, the failure named, the successes built
  print "== 1. fast-build --flake path:DIR#checks"
  let r1 = (go $ietsp fast-build --flake $target --systems $system)
  $ok = ($ok | append (verdict ($r1.exit_code == 1) "exit 1 on a failing check" $"exit ($r1.exit_code)"))
  $ok = ($ok | append (verdict ($r1.stderr =~ $"($system).bad") "the failing attribute is named" $r1.stderr))
  $ok = ($ok | append (verdict ($r1.stderr =~ 'about to fail') "the failure log tail is printed" $r1.stderr))
  $ok = ($ok | append (verdict ($r1.stderr =~ 'building fb-chain') "the dependent check built" $r1.stderr))

  # --- 2. JSON result file
  print "== 2. --result-file json"
  let jf = $"($tmp)/result.json"
  let r2 = (go $ietsp fast-build --flake $target --systems $system --result-file $jf)
  let j = (open --raw $jf | from json)
  let builds = ($j.results | where type == "BUILD")
  let evals = ($j.results | where type == "EVAL")
  let attrs = ($builds | get attr | sort)
  let want = ([$"($system).bad" $"($system).chain" $"($system).dep" $"($system).ok"] | sort)
  $ok = ($ok | append (verdict ($attrs == $want) "json has one BUILD per check" $"got ($attrs)"))
  # An EVAL record per attribute the walk saw, both systems included; a BUILD
  # record only for what was dispatched, as nix-fast-build does.
  $ok = ($ok | append (verdict (($evals | length) == 8) "json has one EVAL per attribute" $"got ($evals | length)"))
  let bad = ($builds | where attr == $"($system).bad" | first)
  let goods = ($builds | where attr != $"($system).bad")
  $ok = ($ok | append (verdict (not $bad.success) "the failing check is not a success" ""))
  $ok = ($ok | append (verdict ($goods | all {|b| $b.success }) "the other checks succeeded" ""))
  $ok = ($ok | append (verdict ($bad.drvPath | str starts-with "/nix/store/") "drvPath is set" ""))
  $ok = ($ok | append (verdict (($goods | first).outputs.out? | is-not-empty) "outputs are set" ""))

  # --- 3. JUnit result file
  print "== 3. --result-file junit"
  let xf = $"($tmp)/result.xml"
  let r3 = (go $ietsp fast-build --flake $target --systems $system --result-format junit --result-file $xf)
  let x = (open --raw $xf)
  $ok = ($ok | append (verdict ($x | str starts-with "<testsuites>") "junit is an XML document" ""))
  $ok = ($ok | append (verdict ($x =~ 'type="BuildFailure"') "junit has a failure element" ""))
  $ok = ($ok | append (verdict ($x =~ 'about to fail') "junit carries the failure log" ""))
  $ok = ($ok | append (verdict ($x =~ 'classname="Build"') "junit has Build testcases" ""))
  # every testsuite attribute is a number, and the counts add up
  let tests = ($x | parse -r 'tests="(?<n>\d+)"' | get n.0 | into int)
  let fails = ($x | parse -r 'failures="(?<n>\d+)"' | get n.0 | into int)
  $ok = ($ok | append (verdict ($tests == 12 and $fails == 1) "junit counts 8 evals + 4 builds, 1 failure" $"tests=($tests) failures=($fails)"))

  # --- 4. --out-link
  print "== 4. --out-link"
  let links = $"($tmp)/links"
  mkdir $links
  let r4 = (go $ietsp fast-build --flake $target --systems $system --out-link $"($links)/result")
  let made = (ls $links | get name | each {|p| $p | path basename } | sort)
  $ok = ($ok | append (verdict ($made == [$"result-($system).chain" $"result-($system).dep" $"result-($system).ok"]) "one out-link per successful check" $"got ($made)"))
  let dangling = ($made | any {|n| not ($"($links)/($n)" | path exists) })
  $ok = ($ok | append (verdict (not $dangling) "every out-link resolves" ""))

  # --- 5. --fail-fast
  print "== 5. --fail-fast"
  let ff = $"($tmp)/ff.json"
  let r5 = (go $ietsp fast-build --flake $target --systems $system --fail-fast --result-file $ff)
  let jff = (open --raw $ff | from json)
  let ffbuilds = ($jff.results | where type == "BUILD")
  let skipped = ($ffbuilds | where ($it.skipped? | default false))
  $ok = ($ok | append (verdict ($r5.exit_code == 1) "--fail-fast still exits 1" $"exit ($r5.exit_code)"))
  $ok = ($ok | append (verdict (($ffbuilds | where {|b| not ($b.success) } | length) >= 1) "--fail-fast reports the failure" ""))
  print $"     --fail-fast skipped ($skipped | length) attributes"

  # --- 6. --stream-json-lines
  print "== 6. --stream-json-lines"
  let r6 = (go $ietsp fast-build --flake $target --systems $system --stream-json-lines)
  let jl = ($r6.stdout | lines | where $it != "")
  mut parsed = true
  for l in $jl { if (do -i { $l | from json } | is-empty) { $parsed = false } }
  $ok = ($ok | append (verdict (($jl | length) >= 8) "one json line per result on stdout" $"got ($jl | length)"))
  $ok = ($ok | append (verdict $parsed "every stdout line parses as json" ""))
  let types = ($jl | each {|l| $l | from json | get type } | uniq | sort)
  $ok = ($ok | append (verdict ($types == ["BUILD" "EVAL"]) "the lines are EVAL and BUILD" $"got ($types)"))

  # --- 7. --systems that matches nothing
  print "== 7. --systems with no match"
  let r7 = (go $ietsp fast-build --flake $target --systems "riscv64-linux")
  $ok = ($ok | append (verdict ($r7.exit_code == 0) "nothing to build, exit 0" $"exit ($r7.exit_code)"))
  $ok = ($ok | append (verdict (not ($r7.stderr =~ 'building fb-')) "no build was dispatched" $r7.stderr))

  # --- 8. refused and unknown flags
  print "== 8. refused flags"
  for f in ["--cachix-cache" "--attic-cache" "--niks3-server" "--copy-to"] {
    let r = (go $ietsp fast-build $f x --flake $target)
    $ok = ($ok | append (verdict (($r.exit_code == 1) and ($r.stderr =~ 'not supported by ietsp fast-build')) $"($f) is refused" $r.stderr))
  }
  let rr = (go $ietsp fast-build --remote host --flake $target)
  $ok = ($ok | append (verdict (($rr.exit_code == 1) and ($rr.stderr =~ 'not supported')) "--remote is refused" $rr.stderr))
  # An unknown flag falls back to the real nix-fast-build; with
  # IETSP_NO_FALLBACK=1 the fallback is visible without running it.
  let ru = (with-env {IETSP_NO_FALLBACK: "1"} { go $ietsp fast-build --nonsense })
  $ok = ($ok | append (verdict ($ru.stderr =~ 'falling back to nix-fast-build') "an unknown flag falls back" $ru.stderr))

  # --- 9. the nixpkgs-free packages set, and --file mode with -A
  print "== 9. --flake #packages and --file -A"
  let r9a = (go $ietsp fast-build --flake $"path:($flake)#packages" --systems $system)
  $ok = ($ok | append (verdict ($r9a.exit_code == 0) "#packages builds and exits 0" $"exit ($r9a.exit_code)"))
  $ok = ($ok | append (verdict ($r9a.stderr =~ 'building fb-lone') "#packages.lone was built" $r9a.stderr))
  # The same derivations through a plain Nix file, selected with -A.
  let rel = $"($tmp)/release.nix"
  $"{ system ? builtins.currentSystem }:
let f = import ($flake)/flake.nix; o = f.outputs { self = null; };
in { ci = o.checks.${system}; }
" | save -f $rel
  let r9b = (go $ietsp fast-build --file $rel -A ci --argstr system $system)
  $ok = ($ok | append (verdict ($r9b.exit_code == 1) "--file -A ci reaches the failing check" $"exit ($r9b.exit_code)"))
  $ok = ($ok | append (verdict ($r9b.stderr =~ 'bad') "--file -A ci names the failure" $r9b.stderr))

  # --- 10. the same flake through the real nix-fast-build
  print "== 10. against the real nix-fast-build"
  let nfb = (^bash -c "command -v nix-fast-build || true" | chomp)
  if ($nfb | is-empty) {
    print "     skipped: nix-fast-build is not on PATH. Run it with"
    print "       nix run github:Mic92/nix-fast-build/a3e9a5a17fa66f84bec8538ef8f98f0a1a9c6228 -- ..."
    print "     the dev shell does not ship it."
  } else {
    let nf = $"($tmp)/nfb.json"
    let rn = (go $nfb --flake $target --systems $system --result-file $nf)
    let jn = (open --raw $nf | from json)
    let ours = ($j.results | where type == "BUILD" | select attr success | sort-by attr)
    let theirs = ($jn.results | where type == "BUILD" | select attr success | sort-by attr)
    $ok = ($ok | append (verdict ($ours == $theirs) "the same attributes and outcomes as nix-fast-build" $"ours ($ours) theirs ($theirs)"))
    $ok = ($ok | append (verdict ($r2.exit_code == $rn.exit_code) "the same exit code as nix-fast-build" $"ours ($r2.exit_code) theirs ($rn.exit_code)"))
  }

  ^rm -rf $tmp
  let failed = ($ok | where $it == false | length)
  print $"($ok | length) cases, ($failed) failed"
  if $failed > 0 { exit 1 }
  print "PASS"
}

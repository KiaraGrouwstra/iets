#!/usr/bin/env nu
# End-to-end test of `ietsp plumbing` (docs/plumbing.md). One case per op,
# checked against the JSON shape that file specifies: the shape is the
# contract.
#
# 1. resolve: an installable becomes derivation roots.
# 2. plan: derivation roots become an upload plan, references first.
# 3. missing: what a build would build and fetch, and --check-cache-status.
# 4. build: outcomes, the exit code of a failure, --gc-root and --no-build.
# 5. composition: every op reads a previous op's stdout with --roots -.
# 6. fetch: a URL becomes a hash. The derivation form is the same call, and
#    scripts/prefetch-e2e.nu builds one.
#
# Needs a live nix-daemon and `nix` on PATH, so it is not a flake check.
#
# usage: scripts/plumbing-e2e.nu
#        BIN=target/debug scripts/plumbing-e2e.nu
use lib.nu [chomp current-system exit-code ietsp-bin isolate out-of run-in verdict]

const HELLO = r#'derivation {
  name = "plumb-hello";
  system = "@SYSTEM@";
  builder = "/bin/sh";
  args = [ "-c" "echo @NONCE@ > $out" ];
}
'#

const CHAIN = r#'let
  dep = derivation {
    name = "plumb-hello";
    system = "@SYSTEM@";
    builder = "/bin/sh";
    args = [ "-c" "echo @NONCE@ > $out" ];
  };
in derivation {
  name = "plumb-top";
  system = "@SYSTEM@";
  builder = "/bin/sh";
  args = [ "-c" "cat ${dep} > $out" ];
}
'#

# one derivation at the top, one under a recursed set, one under a set that is
# not recursed, and one whose meta.outputsToInstall picks an output.
const SET = r#'let
  mk = n: outs: derivation {
    name = n;
    system = "@SYSTEM@";
    builder = "/bin/sh";
    args = [ "-c" "echo @NONCE@ > $out" ];
    outputs = outs;
  };
in {
  a = mk "plumb-wa" [ "out" ];
  sub = { recurseForDerivations = true; b = mk "plumb-wb" [ "out" ]; };
  hidden = { c = mk "plumb-wc" [ "out" ]; };
  multi = (mk "plumb-wm" [ "out" "dev" ]) // { meta.outputsToInstall = [ "dev" ]; };
}
'#

const BAD = r#'derivation {
  name = "plumb-bad";
  system = "@SYSTEM@";
  builder = "/bin/sh";
  args = [ "-c" "echo boom @NONCE@ >&2; exit 7" ];
}
'#

let work = (isolate plumbing)
run-in $work {
  let ietsp = (ietsp-bin)
  let system = (current-system)
  let nonce = (random uuid)
  $HELLO | str replace -a "@SYSTEM@" $system | str replace -a "@NONCE@" $nonce | save -f $"($work)/hello.nix"
  $BAD | str replace -a "@SYSTEM@" $system | str replace -a "@NONCE@" $nonce | save -f $"($work)/bad.nix"
  $CHAIN | str replace -a "@SYSTEM@" $system | str replace -a "@NONCE@" $nonce | save -f $"($work)/chain.nix"
  $SET | str replace -a "@SYSTEM@" $system | str replace -a "@NONCE@" $nonce | save -f $"($work)/set.nix"
  cd $work
  # the ops read the closure from /nix/store, so the roots are instantiated
  # there first; nothing here builds them.
  let hello = (out-of nix-instantiate hello.nix | chomp)
  let bad = (out-of nix-instantiate bad.nix | chomp)
  let top = (out-of nix-instantiate chain.nix | chomp)
  mut ok = true

  print "== 1. resolve"
  let r = (out-of $ietsp plumbing resolve hello.nix | from json)
  $ok = (verdict (($r.roots | length) == 1) "resolve: one root" $"roots ($r.roots)") and $ok
  $ok = (verdict ($r.roots.0.drv == $hello) "resolve: the drv nix-instantiate wrote" $"($r.roots.0.drv) != ($hello)") and $ok
  # --walk get-drvs is get-drvs.cc, which is what nix-instantiate walks
  let w = (out-of $ietsp plumbing resolve --walk get-drvs set.nix | from json)
  let walked = (out-of nix-instantiate set.nix | lines)
  $ok = (verdict (($w.roots | get drv) == $walked) "resolve --walk get-drvs: nix-instantiate's roots" $"($w.roots)") and $ok
  $ok = (verdict (($w.roots | get attr) == [a multi sub.b]) "resolve --walk get-drvs: the attribute path of each" $"($w.roots)") and $ok
  # --walk to-install is get-drvs.cc:queryOutputs, which is what nix build installs
  let t = (out-of $ietsp plumbing resolve --walk to-install set.nix -A multi | from json)
  $ok = (verdict (($t.roots | get output) == [dev]) "resolve --walk to-install: meta.outputsToInstall" $"($t.roots)") and $ok
  let d = (out-of $ietsp plumbing resolve set.nix -A multi | from json)
  $ok = (verdict ($d.roots.0.output == null) "resolve --walk context: the value named no output" $"($d.roots)") and $ok

  print "== 2. plan"
  let p = (out-of $ietsp plumbing plan $hello | from json)
  $ok = (verdict ($p.drvs == [$hello]) "plan: the root's closure" $"drvs ($p.drvs)") and $ok
  $ok = (verdict ($p.storeDrvs == 1) "plan: read from /nix/store" $"storeDrvs ($p.storeDrvs)") and $ok
  $ok = (verdict ($p.sources == []) "plan: no sources" $"sources ($p.sources)") and $ok

  print "== 3. missing"
  let m = (out-of $ietsp plumbing missing $hello | from json)
  $ok = (verdict ($m.willBuild == [$hello]) "missing: willBuild" $"willBuild ($m.willBuild)") and $ok
  let keys = ($m | columns | sort)
  let want = ([downloadSize narSize unknown willBuild willSubstitute] | sort)
  $ok = (verdict ($keys == $want) "missing: the documented keys" $"keys ($keys)") and $ok
  let c = (out-of $ietsp plumbing missing --check-cache-status $hello | from json)
  $ok = (verdict ($c.roots.0.cacheStatus == "notBuilt") "missing: --check-cache-status per root" $"roots ($c.roots)") and $ok
  $ok = (verdict ($c.roots.0.neededBuilds == [$hello]) "missing: neededBuilds" $"($c.roots.0)") and $ok
  # docs/plumbing.md: byte for byte what `ietsp build --dry-run --json` prints,
  # willBuild in the plan's order and willSubstitute by store-path name.
  let op = (^$ietsp plumbing missing $top)
  let dry = (^$ietsp build --dry-run --json chain.nix)
  $ok = (verdict ($op == $dry) "missing: byte-identical to build --dry-run --json" $"($op)\n($dry)") and $ok

  print "== 4. build"
  let b = (out-of $ietsp plumbing build $hello | from json)
  $ok = (verdict ($b.outcomes.0.success) "build: the outcome" $"($b.outcomes)") and $ok
  let out = $b.outcomes.0.outputs.out
  $ok = (verdict ($out | path exists) "build: the output is in the store" $out) and $ok
  $ok = (verdict ((exit-code $ietsp plumbing build $bad) == 100) "build: a failed build exits 100" "exit") and $ok
  mkdir gc
  let g = (out-of $ietsp plumbing build --gc-root $"($work)/gc" $hello | from json)
  let link = $"($work)/gc/($out | path basename)"
  $ok = (verdict ($g.gcRoots == [$link]) "build: --gc-root reports the links" $"($g.gcRoots?)") and $ok
  $ok = (verdict ((^readlink $link | chomp) == $out) "build: --gc-root links the output" $link) and $ok
  let n = (out-of $ietsp plumbing build --no-build $hello | from json)
  $ok = (verdict ($n.outcomes == []) "build: --no-build builds nothing" $"($n)") and $ok

  print "== 5. composition"
  let piped = (^$ietsp plumbing plan $hello | ^$ietsp plumbing build --roots - | from json)
  $ok = (verdict ($piped.outcomes.0.outputs.out == $out) "plan | build --roots -" $"($piped)") and $ok
  let via = (^$ietsp plumbing resolve hello.nix | ^$ietsp plumbing missing --roots - | from json)
  $ok = (verdict ($via.willBuild == []) "resolve | missing --roots - (already built)" $"($via)") and $ok

  print "== 6. fetch"
  "fetch me\n" | save -f $"($work)/bytes.txt"
  let url = $"file://($work)/bytes.txt"
  let f = (out-of $ietsp plumbing fetch $url | from json)
  let hashed = (out-of nix hash file --sri $"($work)/bytes.txt" | chomp)
  $ok = (verdict ($f == {hash: $hashed, url: $url}) "fetch: {hash, url}" $"($f | to json -r), want ($hashed)") and $ok
  let s = (out-of $ietsp plumbing fetch --hash-type sha512 $url | from json)
  let want = (out-of nix hash file --sri --type sha512 $"($work)/bytes.txt" | chomp)
  $ok = (verdict ($s.hash == $want) "fetch: --hash-type" $"($s.hash) != ($want)") and $ok

  if $ok { print "PASS"; 0 } else { print "FAIL"; 1 }
}

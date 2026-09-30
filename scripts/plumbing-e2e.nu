#!/usr/bin/env nu
# End-to-end test of `iets plumbing` (docs/reference/plumbing.md). One case per op,
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
# Needs a live nix-daemon and `nix` on PATH: the flake check runs it in a
# container (nix/e2e-test.nix).
#
# usage: scripts/plumbing-e2e.nu
#        BIN=target/debug scripts/plumbing-e2e.nu
use lib.nu [chomp current-system exit-code iets-bin isolate out-of require-real-nix run-in verdict]

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
  args = [ "-c" "echo ${dep} > $out" ];
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

# one derivation whose `args` interpolate 32 others, so the parallel
# evaluator coerces them on helper fibers: --dump-drvs must write all 33.
const MANY = r#'let
  dep = n: derivation {
    name = "plumb-dep-${toString n}";
    system = "@SYSTEM@";
    builder = "/bin/sh";
    args = [ "-c" "echo @NONCE@ ${toString n} > $out" ];
  };
  deps = builtins.genList dep 32;
in derivation {
  name = "plumb-root";
  system = "@SYSTEM@";
  builder = "/bin/sh";
  args = [ "-c" "echo @NONCE@ > $out" ] ++ (map (d: "${d}") deps);
}
'#

# MANY is flat: the demand path takes each dep's own thunk, so an effect
# parked on it is released. DEEP is not. A helper claims one group's thunk
# and makes nine derivations under it, and the demand path reaches those
# values through the attribute, never through the thunk the helper claimed.
# Held back, the dump lost 126 to 142 of the 145 on repeated runs.
const DEEP = r#'let
  dep = n: derivation {
    name = "plumb-deep-dep-${toString n}";
    system = "@SYSTEM@";
    builder = "/bin/sh";
    args = [ "-c" "echo @NONCE@ ${toString n} > $out" ];
  };
  group = g: {
    drv = derivation {
      name = "plumb-deep-group-${toString g}";
      system = "@SYSTEM@";
      builder = "/bin/sh";
      args = [ "-c" "echo @NONCE@ > $out" ] ++ map (n: "${dep (g * 8 + n)}") (builtins.genList (x: x) 8);
    };
  };
  groups = map (g: (group g).drv) (builtins.genList (x: x) 16);
in derivation {
  name = "plumb-deep-root";
  system = "@SYSTEM@";
  builder = "/bin/sh";
  args = [ "-c" "echo @NONCE@ > $out" ] ++ map (d: "${d}") groups;
}
'#

const BAD = r#'derivation {
  name = "plumb-bad";
  system = "@SYSTEM@";
  builder = "/bin/sh";
  args = [ "-c" "echo boom @NONCE@ >&2; exit 7" ];
}
'#

require-real-nix
let work = (isolate plumbing)
run-in $work {
  let iets = (iets-bin)
  let system = (current-system)
  let nonce = (random uuid)
  $HELLO | str replace -a "@SYSTEM@" $system | str replace -a "@NONCE@" $nonce | save -f $"($work)/hello.nix"
  $BAD | str replace -a "@SYSTEM@" $system | str replace -a "@NONCE@" $nonce | save -f $"($work)/bad.nix"
  $CHAIN | str replace -a "@SYSTEM@" $system | str replace -a "@NONCE@" $nonce | save -f $"($work)/chain.nix"
  $SET | str replace -a "@SYSTEM@" $system | str replace -a "@NONCE@" $nonce | save -f $"($work)/set.nix"
  $MANY | str replace -a "@SYSTEM@" $system | str replace -a "@NONCE@" $nonce | save -f $"($work)/many.nix"
  $DEEP | str replace -a "@SYSTEM@" $system | str replace -a "@NONCE@" $nonce | save -f $"($work)/deep.nix"
  cd $work
  # the ops read the closure from /nix/store, so the roots are instantiated
  # there first; nothing here builds them.
  let hello = (out-of nix-instantiate hello.nix | chomp)
  let bad = (out-of nix-instantiate bad.nix | chomp)
  let top = (out-of nix-instantiate chain.nix | chomp)
  mut ok = true

  print "== 1. resolve"
  let r = (out-of $iets plumbing resolve hello.nix | from json)
  $ok = (verdict (($r.roots | length) == 1) "resolve: one root" $"roots ($r.roots)") and $ok
  $ok = (verdict ($r.roots.0.drv == $hello) "resolve: the drv nix-instantiate wrote" $"($r.roots.0.drv) != ($hello)") and $ok
  # --walk get-drvs is get-drvs.cc, which is what nix-instantiate walks
  let w = (out-of $iets plumbing resolve --walk get-drvs set.nix | from json)
  let walked = (out-of nix-instantiate set.nix | lines)
  $ok = (verdict (($w.roots | get drv) == $walked) "resolve --walk get-drvs: nix-instantiate's roots" $"($w.roots)") and $ok
  $ok = (verdict (($w.roots | get attr) == [a multi sub.b]) "resolve --walk get-drvs: the attribute path of each" $"($w.roots)") and $ok
  # --walk to-install is get-drvs.cc:queryOutputs, which is what nix build installs
  let t = (out-of $iets plumbing resolve --walk to-install set.nix -A multi | from json)
  $ok = (verdict (($t.roots | get output) == [dev]) "resolve --walk to-install: meta.outputsToInstall" $"($t.roots)") and $ok
  let d = (out-of $iets plumbing resolve set.nix -A multi | from json)
  $ok = (verdict ($d.roots.0.output == null) "resolve --walk context: the value named no output" $"($d.roots)") and $ok
  # a derivation a helper fiber created is dumped too, or the closure is partial
  let many = (out-of $iets plumbing resolve --dump-drvs many many.nix | from json)
  $ok = (verdict (($many.roots | length) == 1) "resolve: the one root" $"($many.roots)") and $ok
  $ok = (verdict ((ls many | length) == 33) "resolve --dump-drvs: every derivation, whichever fiber made it" $"(ls many | length)") and $ok
  # same, where the demand path never takes the thunk the helper claimed.
  # Three runs, because a held-back dump loses a different subset each time.
  let deep = (1..3 | each {|i|
    rm -rf $"deep($i)"
    out-of $iets plumbing resolve --dump-drvs $"deep($i)" deep.nix | ignore
    ls $"deep($i)" | length
  })
  $ok = (verdict ($deep == [145 145 145]) "resolve --dump-drvs: every derivation under a helper's thunk, every run" $"($deep)") and $ok

  print "== 2. plan"
  let p = (out-of $iets plumbing plan $hello | from json)
  $ok = (verdict ($p.drvs == [$hello]) "plan: the root's closure" $"drvs ($p.drvs)") and $ok
  $ok = (verdict ($p.storeDrvs == 1) "plan: read from /nix/store" $"storeDrvs ($p.storeDrvs)") and $ok
  $ok = (verdict ($p.sources == []) "plan: no sources" $"sources ($p.sources)") and $ok

  print "== 3. missing"
  let m = (out-of $iets plumbing missing $hello | from json)
  $ok = (verdict ($m.willBuild == [$hello]) "missing: willBuild" $"willBuild ($m.willBuild)") and $ok
  let keys = ($m | columns | sort)
  let want = ([downloadSize narSize unknown willBuild willSubstitute] | sort)
  $ok = (verdict ($keys == $want) "missing: the documented keys" $"keys ($keys)") and $ok
  let c = (out-of $iets plumbing missing --check-cache-status $hello | from json)
  $ok = (verdict ($c.roots.0.cacheStatus == "notBuilt") "missing: --check-cache-status per root" $"roots ($c.roots)") and $ok
  $ok = (verdict ($c.roots.0.neededBuilds == [$hello]) "missing: neededBuilds" $"($c.roots.0)") and $ok
  # docs/reference/plumbing.md: byte for byte what `iets build --dry-run --json` prints,
  # willBuild in the plan's order and willSubstitute by store-path name.
  let op = (^$iets plumbing missing $top)
  let dry = (^$iets build --dry-run --json chain.nix)
  $ok = (verdict ($op == $dry) "missing: byte-identical to build --dry-run --json" $"($op)\n($dry)") and $ok

  print "== 4. build"
  let b = (out-of $iets plumbing build $hello | from json)
  $ok = (verdict ($b.outcomes.0.success) "build: the outcome" $"($b.outcomes)") and $ok
  let out = $b.outcomes.0.outputs.out
  $ok = (verdict ($out | path exists) "build: the output is in the store" $out) and $ok
  $ok = (verdict ((exit-code $iets plumbing build $bad) == 100) "build: a failed build exits 100" "exit") and $ok
  mkdir gc
  let g = (out-of $iets plumbing build --gc-root $"($work)/gc" $hello | from json)
  let link = $"($work)/gc/($out | path basename)"
  $ok = (verdict ($g.gcRoots == [$link]) "build: --gc-root reports the links" $"($g.gcRoots?)") and $ok
  $ok = (verdict ((^readlink $link | chomp) == $out) "build: --gc-root links the output" $link) and $ok
  let n = (out-of $iets plumbing build --no-build $hello | from json)
  $ok = (verdict ($n.outcomes == []) "build: --no-build builds nothing" $"($n)") and $ok

  print "== 5. composition"
  let piped = (^$iets plumbing plan $hello | ^$iets plumbing build --roots - | from json)
  $ok = (verdict ($piped.outcomes.0.outputs.out == $out) "plan | build --roots -" $"($piped)") and $ok
  let via = (^$iets plumbing resolve hello.nix | ^$iets plumbing missing --roots - | from json)
  $ok = (verdict ($via.willBuild == []) "resolve | missing --roots - (already built)" $"($via)") and $ok
  # a closure that is in no store: only --dump-drvs carries it across the pipe.
  $CHAIN | str replace -a "@SYSTEM@" $system | str replace -a "@NONCE@" (random uuid) | save -f $"($work)/fresh.nix"
  let bare = (^$iets plumbing resolve fresh.nix | ^$iets plumbing plan --roots - | complete)
  $ok = (verdict ($bare.exit_code != 0) "resolve | plan without a dump: the drv is in no store" $"($bare)") and $ok
  let c = (^$iets plumbing resolve --dump-drvs dump fresh.nix | ^$iets plumbing plan --roots - | from json)
  $ok = (verdict ($c.storeDrvs == 0) "resolve --dump-drvs | plan: nothing read from /nix/store" $"($c)") and $ok
  $ok = (verdict (($c.drvs | length) == 2) "resolve --dump-drvs | plan: the whole closure" $"($c)") and $ok
  let cb = (^$iets plumbing resolve --dump-drvs dump fresh.nix | ^$iets plumbing build --roots - | from json)
  $ok = (verdict ($cb.outcomes.0.success) "resolve --dump-drvs | build" $"($cb)") and $ok

  print "== 6. fetch"
  "fetch me\n" | save -f $"($work)/bytes.txt"
  let url = $"file://($work)/bytes.txt"
  let f = (out-of $iets plumbing fetch $url | from json)
  let hashed = (out-of nix hash file --sri $"($work)/bytes.txt" | chomp)
  $ok = (verdict ($f == {hash: $hashed, url: $url}) "fetch: {hash, url}" $"($f | to json -r), want ($hashed)") and $ok
  let s = (out-of $iets plumbing fetch --hash-type sha512 $url | from json)
  let want = (out-of nix hash file --sri --type sha512 $"($work)/bytes.txt" | chomp)
  $ok = (verdict ($s.hash == $want) "fetch: --hash-type" $"($s.hash) != ($want)") and $ok

  if $ok { print "PASS"; 0 } else { print "FAIL"; 1 }
}

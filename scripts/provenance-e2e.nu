#!/usr/bin/env nu
# Provenance end to end (docs/explanation/provenance.md). Needs a live nix-daemon and
# `nix` on PATH; it builds, so it is not a flake check.
#
# 1. `nix-build` on a failing derivation: stderr names the attribute, the
#    local file the derivation was defined in, and the local tree in the
#    build log, not the store path.
# 2. the same run with IETSP_PROVENANCE=0: the store paths come back.
# 3. an evaluation error inside a tree that was copied to the store (a
#    flake): the position names the working copy.
# 4. `eval-jobs --provenance` adds a mapped `pos`; without the flag there
#    is no `pos` key.
#
# usage: nu scripts/provenance-e2e.nu
#        BIN=target/debug nu scripts/provenance-e2e.nu
use lib.nu [current-system ietsp-bin isolate make-farm verdict]

# A tree whose derivation fails after echoing its own source path.
def make-case [dir: string] {
  mkdir $"($dir)/src"
  $"hello ($nu.pid)\n" | save -f $"($dir)/src/a"
  r#'derivation {
  name = "prov-e2e-fail";
  system = "@SYSTEM@";
  builder = "/bin/sh";
  args = [ "-c" "echo source is ${./src}; exit 1" ];
}
'# | str replace -a "@SYSTEM@" (current-system) | save -f $"($dir)/default.nix"
}

# A locked flake with one failing derivation and one throwing attribute.
def make-flake [dir: string, system: string] {
  mkdir $dir
  r#'{
  outputs = _: {
    packages.@SYSTEM@.boom = derivation {
      name = "prov-e2e-flake";
      system = "@SYSTEM@";
      builder = "/bin/sh";
      args = [ "-c" "echo tree is ${./.}; exit 1" ];
    };
  };
}
'# | str replace -a "@SYSTEM@" $system | save -f $"($dir)/flake.nix"
  '{"nodes":{"root":{}},"root":"root","version":7}' | save -f $"($dir)/flake.lock"
  'throw "boom from the tree"' | save -f $"($dir)/bad.nix"
}

def main [] {
  let ietsp = (ietsp-bin)
  let tmp = (isolate provenance-e2e)
  let system = (current-system)
  let farm = (make-farm $ietsp $tmp)
  let d = $"($tmp)/case"
  make-case $d
  let fd = $"($tmp)/flake"
  make-flake $fd $system
  mut ok = []

  print "== 1. nix-build on a failing derivation"
  let r = (^$"($farm)/nix-build" $"($d)/default.nix" | complete)
  let err = $r.stderr
  $ok = ($ok | append (verdict ($r.exit_code != 0) "the build fails" $"exit ($r.exit_code), expected non-zero"))
  $ok = ($ok | append (verdict ($err | str contains $"defined at ($d)/default.nix:")
    "the failure names the local file it was defined in" $"no 'defined at ($d)/default.nix:' in stderr:\n($err)"))
  $ok = ($ok | append (verdict ($err | str contains $"source is ($d)/src")
    "the build log names the local tree" $"no 'source is ($d)/src' in stderr:\n($err)"))
  $ok = ($ok | append (verdict ($err | str contains "prov-e2e-fail failed")
    "the failure names the derivation" $"no 'prov-e2e-fail failed' in stderr:\n($err)"))

  print "== 2. the same run with IETSP_PROVENANCE=0"
  let plain = (with-env {IETSP_PROVENANCE: "0"} { ^$"($farm)/nix-build" $"($d)/default.nix" | complete })
  $ok = ($ok | append (verdict (not ($plain.stderr | str contains "defined at"))
    "no provenance line with IETSP_PROVENANCE=0" $"stderr:\n($plain.stderr)"))
  $ok = ($ok | append (verdict ($plain.stderr | str contains "source is /nix/store/")
    "the store path is left alone with IETSP_PROVENANCE=0" $"stderr:\n($plain.stderr)"))

  print "== 3. an evaluation error inside a flake tree"
  let bad = (^$ietsp eval --no-cache $"($fd)/bad.nix" | complete)
  $ok = ($ok | append (verdict ($bad.stderr | str contains $"($fd)/bad.nix:1:1")
    "a local file keeps its own position" $"stderr:\n($bad.stderr)"))
  # The flake tree is copied to the store, so the position comes back mapped.
  let fe = (^$ietsp build --no-cache --builder daemon $"($fd)#packages.($system).boom" | complete)
  $ok = ($ok | append (verdict ($fe.stderr | str contains $"defined at ($fd)/flake.nix:")
    "a flake derivation names the working copy" $"no 'defined at ($fd)/flake.nix:' in stderr:\n($fe.stderr)"))
  $ok = ($ok | append (verdict ($fe.stderr | str contains $"tree is ($fd)")
    "a flake build log names the working copy" $"no 'tree is ($fd)' in stderr:\n($fe.stderr)"))

  print "== 4. eval-jobs --provenance"
  let with_pos = (^$ietsp eval-jobs --no-cache --provenance --flake $"($fd)#packages.($system)" | complete)
  let without = (^$ietsp eval-jobs --no-cache --flake $"($fd)#packages.($system)" | complete)
  $ok = ($ok | append (verdict ($with_pos.stdout | str contains $"\"pos\":\"($fd)/flake.nix:")
    "--provenance adds a mapped pos" $"stdout:\n($with_pos.stdout)"))
  $ok = ($ok | append (verdict (not ($without.stdout | str contains "\"pos\""))
    "no pos without the flag" $"stdout:\n($without.stdout)"))

  rm -rf $tmp
  let bad = ($ok | where $it == false | length)
  print $"($ok | length) checks, ($bad) failed"
  if $bad > 0 { exit 1 }
}

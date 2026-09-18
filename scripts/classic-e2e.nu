#!/usr/bin/env nu
# Classic mode against the real nix: for every case, run the real
# nix-instantiate/nix-build and ours and compare stdout byte for byte and the
# exit code. Needs a live nix-daemon and `nix` on PATH (the dev shell has
# nix 2.34.8); it writes to the store, so it is not a flake check.
#
# usage: nu scripts/classic-e2e.nu
#        BIN=target/debug nu scripts/classic-e2e.nu
use lib.nu [chomp show verdict]

const NIXPKGS = "/nix/store/ah3pxz4xa52hd6vhbx17vcsci0b8bmbr-source"

def real-bin-dir []: nothing -> string {
  (^readlink -f (^bash -c "command -v nix-instantiate" | chomp) | chomp | path dirname)
}

# The symlink farm: our binary under the classic names.
def make-farm [bin: string, dir: string]: nothing -> string {
  mkdir $"($dir)/bin"
  for n in [nix-instantiate nix-build nix-eval-jobs nix] {
    ^ln -sf ($bin | path expand) $"($dir)/bin/($n)"
  }
  $"($dir)/bin"
}

def --wrapped run-both [farm: string, name: string, --cwd: string = ".", ...args: string] {
  let ours = (^$"($farm)/($name)" ...$args | complete)
  let real = (^$name ...$args | complete)
  {ours: $ours, real: $real}
}

# Compares stdout bytes and exit code; prints ok/FAIL and returns the verdict.
def cmp-case [label: string, r: record]: nothing -> bool {
  let same_out = ($r.ours.stdout | into binary) == ($r.real.stdout | into binary)
  let same_code = $r.ours.exit_code == $r.real.exit_code
  if $same_out and $same_code {
    verdict true $label ""
  } else {
    let d = $"($label): exit ours=($r.ours.exit_code) real=($r.real.exit_code)"
    print $"FAIL ($d)"
    print $"  ours stdout: ($r.ours.stdout | str substring 0..400)"
    print $"  real stdout: ($r.real.stdout | str substring 0..400)"
    print $"  ours stderr: ($r.ours.stderr | str substring 0..400)"
    false
  }
}

def main [] {
  let bin = ($env | get -o BIN | default "target/release")
  let ietsp = $"($bin)/ietsp"
  if not ($ietsp | path exists) { print -e $"no ($ietsp); cargo build --release first"; exit 1 }
  let real = (real-bin-dir)
  print $"real nix: ($real)"
  $env.IETSP_NIX_BIN_DIR = $real
  let tmp = (mktemp -d -t classic-e2e-XXXXXX)
  let farm = (make-farm $ietsp $tmp)
  mut ok = []

  # --- nix-instantiate: drv paths
  $ok = ($ok | append (cmp-case "instantiate <nixpkgs> -A hello"
    (run-both $farm nix-instantiate -I $"nixpkgs=($NIXPKGS)" "<nixpkgs>" -A hello)))
  $ok = ($ok | append (cmp-case "instantiate <nixpkgs> -A glibc (multi-output)"
    (run-both $farm nix-instantiate -I $"nixpkgs=($NIXPKGS)" "<nixpkgs>" -A glibc)))
  $ok = ($ok | append (cmp-case "instantiate <nixpkgs> -A glibc.dev (!output suffix)"
    (run-both $farm nix-instantiate -I $"nixpkgs=($NIXPKGS)" "<nixpkgs>" -A glibc.dev)))
  $ok = ($ok | append (cmp-case "instantiate two -A in one run"
    (run-both $farm nix-instantiate -I $"nixpkgs=($NIXPKGS)" "<nixpkgs>" -A hello -A glibc.dev)))

  # --- nix-instantiate: --eval
  $ok = ($ok | append (cmp-case "instantiate --eval --strict --json"
    (run-both $farm nix-instantiate --eval --strict --json -E "{ a = 1; }")))
  $ok = ($ok | append (cmp-case "instantiate --eval"
    (run-both $farm nix-instantiate --eval -E "{ a = 1; b = [ 1 2 ]; }")))
  $ok = ($ok | append (cmp-case "instantiate --eval --raw"
    (run-both $farm nix-instantiate --eval --raw -E "\"x\" + \"y\"")))
  $ok = ($ok | append (cmp-case "instantiate --eval --argstr"
    (run-both $farm nix-instantiate --eval --strict --json --argstr who world -E "{ who }: { inherit who; }")))

  # --- a tiny custom derivation, like scripts/daemon-e2e.nu case 1
  let d = $"($tmp)/case"
  mkdir $"($d)/src"
  $"hello ($nu.pid)\n" | save $"($d)/src/a"
  r#'derivation {
  name = "classic-e2e";
  system = "x86_64-linux";
  builder = "/bin/sh";
  args = [ "-c" "cat ${@DIR@/src}/a > $out" ];
}
'# | str replace -a "@DIR@" $d | save $"($d)/default.nix"
  r#'derivation {
  name = "classic-e2e-multi";
  system = "x86_64-linux";
  outputs = [ "out" "dev" ];
  builder = "/bin/sh";
  args = [ "-c" "echo a > $out; echo b > $dev" ];
}
'# | save $"($d)/multi.nix"

  $ok = ($ok | append (cmp-case "instantiate a custom drv"
    (run-both $farm nix-instantiate $"($d)/default.nix")))
  $ok = ($ok | append (cmp-case "instantiate --readonly-mode"
    (run-both $farm nix-instantiate --readonly-mode $"($d)/default.nix")))

  # --- --add-root: the printed path is the link, and the daemon knows the root
  mut root_ok = true
  for who in [real ours] {
    let cmd = if $who == "real" { "nix-instantiate" } else { $"($farm)/nix-instantiate" }
    let link = $"($tmp)/gcroot-($who)"
    let out = (^$cmd --add-root $link $"($d)/default.nix" | complete)
    let printed = ($out.stdout | chomp)
    if $printed != $link { print $"FAIL --add-root ($who): printed '($printed)' != '($link)'"; $root_ok = false }
    let target = (^readlink $link | chomp)
    let roots = (^nix-store --query --roots $target | complete | get stdout)
    if not ($roots | str contains $link) { print $"FAIL --add-root ($who): ($link) is not a gc root"; $root_ok = false }
  }
  $ok = ($ok | append (verdict $root_ok "--add-root links and registers a gc root" "see above"))

  # --- nix-build: default `result`, -o, multi-output naming
  for case in [
    {label: "build a custom drv, default result", args: [$"($d)/default.nix"], links: [result]}
    {label: "build -o link", args: [-o link $"($d)/default.nix"], links: [link]}
    {label: "build --no-out-link", args: [--no-out-link $"($d)/default.nix"], links: []}
    {label: "build two outputs of one drv", args: [-E $"let m = import ($d)/multi.nix; in [ m.dev m.out ]"], links: [result result-dev]}
    {label: "build two drvs", args: [-E ("[ (import " + $d + "/default.nix) (import " + $d + "/multi.nix) ]")], links: [result result-2]}
  ] {
    mut got = {}
    for who in [real ours] {
      let wd = $"($tmp)/build-($who)"
      mkdir $wd
      cd $wd
      let cmd = if $who == "real" { "nix-build" } else { $"($farm)/nix-build" }
      let r = (^$cmd ...$case.args | complete)
      let links = ($case.links | each {|l| if ($l | path exists) { ^readlink $l | chomp } else { $"MISSING ($l)" } })
      $got = ($got | insert $who {out: ($r.stdout), code: $r.exit_code, links: $links, err: $r.stderr})
    }
    let same = (($got.ours.out | into binary) == ($got.real.out | into binary)) and ($got.ours.code == $got.real.code) and ($got.ours.links == $got.real.links)
    if $same {
      print $"ok   ($case.label)"
    } else {
      print $"FAIL ($case.label)"
      print $"  ours: code=($got.ours.code) links=($got.ours.links)"
      print $"  real: code=($got.real.code) links=($got.real.links)"
      print $"  ours stdout: ($got.ours.out)"
      print $"  real stdout: ($got.real.out)"
      print $"  ours stderr: ($got.ours.err | str substring 0..400)"
    }
    $ok = ($ok | append $same)
  }

  # --- fallbacks
  $ok = ($ok | append (cmp-case "--parse execs the real nix-instantiate"
    (run-both $farm nix-instantiate --parse -E 1)))
  let fb = (^$"($farm)/nix-instantiate" --parse -E 1 | complete)
  $ok = ($ok | append (verdict ($fb.stderr | str contains "ietsp: falling back to nix-instantiate:")
    "the fallback prints one line on stderr" $"stderr was '($fb.stderr)'"))

  let nofb = (with-env {IETSP_NO_FALLBACK: "1"} { ^$"($farm)/nix-instantiate" --parse -E 1 | complete })
  $ok = ($ok | append (verdict ($nofb.exit_code == 1) "IETSP_NO_FALLBACK=1 exits 1" $"exit ($nofb.exit_code)"))

  mkdir $"($tmp)/tb"
  "hi\n" | save $"($tmp)/tb/f"
  ^tar -czf $"($tmp)/x.tar.gz" -C $tmp tb
  $ok = ($ok | append (cmp-case "fetchTarball falls back and prints the real result"
    (run-both $farm nix-instantiate --eval -E $"builtins.fetchTarball \"file://($tmp)/x.tar.gz\"")))

  let repl = (^$"($farm)/nix" repl --version | complete)
  $ok = ($ok | append (verdict ($repl.stderr | str contains "ietsp: falling back to nix:")
    "the 'nix' name falls back" $"stderr was '($repl.stderr)'"))

  cd /
  rm -rf $tmp
  let failed = ($ok | where $it == false | length)
  print $"($ok | length) cases, ($failed) failed"
  if $failed > 0 { exit 1 }
}

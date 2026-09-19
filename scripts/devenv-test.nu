#!/usr/bin/env nu
# `ietsp print-dev-env` against `nix print-dev-env --option eval-cache false`:
#  a. byte-identical bash for this repo's flake and a scratch mkShell flake;
#  b. --json identical modulo key order (jq -S);
#  c. warm run (cache hit) time, printed;
#  d. --gc-root creates the symlink and nix-store --gc --print-roots lists it;
#  e. a garbage-collected env output is rebuilt on the next (cached) run;
#  f. a shell whose package is absent from /nix/store gets it substituted;
#  g. `#attr` selects a shell;
#  h. a `git worktree` of this repo at a path nix has never copied: ietsp
#     prints the same bash without fetching the root tree.
# The scratch flakes sit on the nixpkgs this repo pins (lock written by hand,
# so nothing is fetched: both evaluators find the tree in the store by narHash).
#
# usage: scripts/devenv-test.nu            # uses target/release
#        IETSP=target/debug/ietsp scripts/devenv-test.nu
use lib.nu [chomp show diff-head]

def --wrapped nixf [...args: string] { ^nix --extra-experimental-features "nix-command flakes" ...$args }

def ok [msg: string]: nothing -> bool { print $"ok   ($msg)"; true }
def bad [msg: string, ...lines: string]: nothing -> bool {
  print $"FAIL ($msg)"
  for l in $lines { print $"  ($l)" }
  false
}
def ms [t0: datetime, t1: datetime]: nothing -> int { (($t1 - $t0) / 1ms) | math floor }
def tail-of [file: string, n: int]: nothing -> string { open --raw $file | lines | last $n | str join "\n" }
def same [a: string, b: string]: nothing -> bool { (open --raw $a | into binary) == (open --raw $b | into binary) }
def sorted-json [file: string]: nothing -> string { open --raw $file | ^jq -S . }

# nix-env WORK FLAKE FILE [ARGS]: nix print-dev-env from inside FLAKE (the script names <cwd>/outputs).
def --wrapped nix-env [work: string, flake: string, file: string, ...args: string]: nothing -> int {
  cd $flake
  let r = (nixf print-dev-env --option eval-cache "false" ...$args $flake | complete)
  $r.stdout | save -f $file
  $r.stderr | save -f $"($work)/nix.err"
  $r.exit_code
}
def --wrapped ietsp-env [ctx: record, flake: string, file: string, ...args: string]: nothing -> int {
  cd $flake
  let r = (^($ctx.ietsp) print-dev-env ...$args $flake | complete)
  $r.stdout | save -f $file
  $r.stderr | save -f $"($ctx.work)/ietsp.err"
  $r.exit_code
}

def run-tests [ctx: record]: nothing -> int {
  let work = $ctx.work
  let ietsp = $ctx.ietsp
  $env.IETSP_CACHE_DIR = $"($work)/cache"
  mut res = []

  # The scratch flakes: `shell` for a-c, `scratch` for e-g (its default shell
  # names a package that must stay absent from the store until f).
  let locked = (open --raw flake.lock | from json | get nodes.nixpkgs.locked)
  let rev = $locked.rev
  let lock = {
    nodes: {
      nixpkgs: { locked: $locked, original: { owner: "NixOS", repo: "nixpkgs", rev: $rev, type: "github" } }
      root: { inputs: { nixpkgs: "nixpkgs" } }
    }
    root: "root"
    version: 7
  }
  let shell = $"($work)/shell"
  mkdir $shell
  r#'{
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/@REV@";
  outputs = { self, nixpkgs }:
    let pkgs = nixpkgs.legacyPackages.x86_64-linux; in {
      devShells.x86_64-linux.default = pkgs.mkShell {
        packages = [ pkgs.hello pkgs.jq ];
        GREETING = "hi";
        shellHook = "export SCRATCH_FLAKE=\"$PWD\"\n";
      };
    };
}
'# | str replace -a "@REV@" $rev | save $"($shell)/flake.nix"
  $lock | to json | save $"($shell)/flake.lock"

  for flake in [$env.PWD $shell] {
    let name = ($flake | path basename)
    if (nix-env $work $flake $"($work)/nix.sh") != 0 { $res ++= [(bad $"nix print-dev-env ($name)" (tail-of $"($work)/nix.err" 3))] }
    if (ietsp-env $ctx $flake $"($work)/ietsp.sh" --no-cache) != 0 { $res ++= [(bad $"ietsp print-dev-env ($name)" (tail-of $"($work)/ietsp.err" 3))] }
    if (same $"($work)/nix.sh" $"($work)/ietsp.sh") {
      $res ++= [(ok $"a. bash identical: ($name) \((open --raw $"($work)/nix.sh" | into binary | bytes length) bytes\)")]
    } else {
      $res ++= [(bad $"a. bash differs: ($name)" (diff-head $"($work)/nix.sh" $"($work)/ietsp.sh" 8 | str join "\n"))]
    }

    nix-env $work $flake $"($work)/nix.json" --json
    ietsp-env $ctx $flake $"($work)/ietsp.json" --no-cache --json
    sorted-json $"($work)/nix.json" | save -f $"($work)/nix.sorted.json"
    sorted-json $"($work)/ietsp.json" | save -f $"($work)/ietsp.sorted.json"
    if (same $"($work)/nix.sorted.json" $"($work)/ietsp.sorted.json") {
      $res ++= [(ok $"b. json identical: ($name)")]
    } else {
      $res ++= [(bad $"b. json differs: ($name)" (diff-head $"($work)/nix.sorted.json" $"($work)/ietsp.sorted.json" 8 | str join "\n"))]
    }

    ietsp-env $ctx $flake $"($work)/miss.sh"          # cache miss, writes the entry
    let t0 = (date now)
    ietsp-env $ctx $flake $"($work)/hit.sh"
    let t1 = (date now)
    if (same $"($work)/nix.sh" $"($work)/hit.sh") {
      $res ++= [(ok $"c. warm run identical: ($name), (ms $t0 $t1) ms")]
    } else {
      $res ++= [(bad $"c. warm run differs: ($name)" (diff-head $"($work)/nix.sh" $"($work)/hit.sh" 5 | str join "\n"))]
    }
  }

  # ---- scratch flake -----------------------------------------------------------
  mut absent = ""
  mut absent_out = ""
  for p in [cowsay sl figlet lolcat] {
    let r = (nixf eval --raw --impure --expr $"\(builtins.getFlake \"($env.PWD)\"\).inputs.nixpkgs.legacyPackages.x86_64-linux.($p).outPath" | complete)
    if $r.exit_code != 0 { continue }
    let out = ($r.stdout | chomp)
    if not ($out | path exists) { $absent = $p; $absent_out = $out; break }
  }
  if $absent == "" {
    $absent = "cowsay"
    $absent_out = ""
    print "note: every candidate package is present; f. cannot show a substitution"
  }
  let scratch = $"($work)/scratch"
  mkdir $scratch
  r#'{
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/@REV@";
  outputs = { self, nixpkgs }:
    let pkgs = nixpkgs.legacyPackages.x86_64-linux; in {
      devShells.x86_64-linux.default = pkgs.mkShell { packages = [ pkgs.@ABSENT@ ]; };
      devShells.x86_64-linux.other = pkgs.mkShell { FOO = "bar"; };
    };
}
'# | str replace -a "@REV@" $rev | str replace -a "@ABSENT@" $absent | save $"($scratch)/flake.nix"
  $lock | to json | save $"($scratch)/flake.lock"
  let root = $"($work)/roots/env"

  if (ietsp-env $ctx $scratch $"($work)/scratch.sh" --gc-root $root) != 0 { $res ++= [(bad "ietsp print-dev-env scratch" (tail-of $"($work)/ietsp.err" 5))] }
  let outpath = (^readlink $root | complete | get stdout | chomp)
  if $outpath =~ '^/nix/store/.*-nix-shell-env$' {
    $res ++= [(ok $"d. gc root symlink: ($root) -> ($outpath)")]
  } else {
    $res ++= [(bad $"d. gc root symlink missing or wrong: '($outpath)'")]
  }
  if (^nix-store --gc --print-roots | complete | get stdout | str contains $root) {
    $res ++= [(ok "d. nix-store --gc --print-roots lists the root")]
  } else {
    $res ++= [(bad "d. root not registered with the daemon")]
  }
  if $absent_out != "" {
    if ($absent_out | path exists) {
      $res ++= [(ok $"f. ($absent) was substituted: ($absent_out)")]
    } else {
      $res ++= [(bad $"f. ($absent_out) is still absent")]
    }
  }
  nix-env $work $scratch $"($work)/scratch-nix.sh"
  if (same $"($work)/scratch-nix.sh" $"($work)/scratch.sh") {
    $res ++= [(ok "a. bash identical: scratch")]
  } else {
    $res ++= [(bad "a. bash differs: scratch" (diff-head $"($work)/scratch-nix.sh" $"($work)/scratch.sh" 8 | str join "\n"))]
  }

  # e. simulate a GC of the env output: drop the root, delete the path, rerun on the warm cache.
  rm -f $root
  let deleted = ($outpath != "" and (^nix-store --delete $outpath | complete | get exit_code) == 0 and not ($outpath | path exists))
  if $deleted {
    let t0 = (date now)
    ietsp-env $ctx $scratch $"($work)/scratch2.sh" --gc-root $root --timings
    let t1 = (date now)
    let relinked = (^readlink $root | complete | get stdout | chomp) == $outpath
    if ($outpath | path exists) and $relinked and (same $"($work)/scratch.sh" $"($work)/scratch2.sh") {
      let notices = (open --raw $"($work)/ietsp.err" | lines | where ($it | str contains "cache: env output") | length)
      $res ++= [(ok $"e. deleted env output rebuilt on the next run, (ms $t0 $t1) ms \(($notices) fallback notice\)")]
    } else {
      $res ++= [(bad "e. env output not rebuilt" (tail-of $"($work)/ietsp.err" 5))]
    }
  } else {
    $res ++= [(bad $"e. could not delete ($outpath) \(another root holds it?\)")]
  }

  # g. #attr
  ietsp-env $ctx $scratch $"($work)/other.sh" ""
  do { cd $scratch; ^$ietsp print-dev-env $"($scratch)#other" | complete } | do {|r| $r.stdout | save -f $"($work)/other.sh"; $r.stderr | save -f $"($work)/ietsp.err" } $in
  do { cd $scratch; nixf print-dev-env --option eval-cache "false" $"($scratch)#other" | complete | get stdout } | save -f $"($work)/other-nix.sh"
  let has_foo = (open --raw $"($work)/other.sh" | lines | any {|l| $l == "FOO='bar'" })
  if $has_foo and (same $"($work)/other.sh" $"($work)/other-nix.sh") {
    $res ++= [(ok "g. #other selects devShells.x86_64-linux.other, identical to nix")]
  } else {
    $res ++= [(bad "g. #other" (diff-head $"($work)/other-nix.sh" $"($work)/other.sh" 5 | str join "\n") (tail-of $"($work)/ietsp.err" 3))]
  }
  let missing = (do { cd $scratch; ^$ietsp print-dev-env $"($scratch)#missing" | complete })
  $missing.stderr | save -f $"($work)/ietsp.err"
  if $missing.exit_code == 0 {
    $res ++= [(bad "g. #missing should fail")]
  } else if ($missing.stderr | str contains "does not provide attribute 'devShells.x86_64-linux.missing'") {
    $res ++= [(ok "g. #missing reports the attribute paths tried")]
  } else {
    $res ++= [(bad "g. #missing error text" (tail-of $"($work)/ietsp.err" 2))]
  }

  # h. ietsp first (cache off) on a worktree nix has never copied; then nix for the reference
  let fresh = $"($work)/fresh"
  ^git -C $env.PWD worktree add -q $fresh HEAD
  let r = (do { cd $fresh; with-env {IETSP_CACHE: "0"} { ^$ietsp print-dev-env --no-cache $fresh | complete } })
  $r.stdout | save -f $"($work)/fresh.sh"
  $r.stderr | save -f $"($work)/ietsp.err"
  nix-env $work $fresh $"($work)/fresh-nix.sh"
  ^git -C $env.PWD worktree remove --force $fresh
  if $r.exit_code == 0 and not ($r.stderr | str contains "fetching") and (same $"($work)/fresh-nix.sh" $"($work)/fresh.sh") {
    $res ++= [(ok "h. worktree nix never saw: bash identical, nothing fetched")]
  } else {
    $res ++= [(bad $"h. worktree \(exit ($r.exit_code)\)" (tail-of $"($work)/ietsp.err" 3) (diff-head $"($work)/fresh-nix.sh" $"($work)/fresh.sh" 5 | str join "\n"))]
  }

  let fail = ($res | where not $it | length)
  print $"($res | where $it | length) passed, ($fail) failed"
  if $fail == 0 { 0 } else { 1 }
}

def main [] {
  cd ($env.FILE_PWD | path join "..")
  let ctx = {
    ietsp: ($env.IETSP? | default ($env.PWD | path join "target/release/ietsp"))
    work: (mktemp -d --tmpdir ietsp-devenv.XXXXXX)
  }
  $env.NIX_REMOTE = ($env.NIX_REMOTE? | default "daemon")
  # the host's ietsp.conf must not steer these builds (a remote `builder` changes cores)
  $env.IETSP_CONF_DIR = ($ctx.work | path join "no-conf")
  $env.XDG_CONFIG_HOME = ($ctx.work | path join "no-xdg")
  hide-env -i IETSP_CONFIG
  let code = (try { run-tests $ctx } catch {|e| print -e $e.rendered; 1 })
  rm -rf $ctx.work
  exit $code
}

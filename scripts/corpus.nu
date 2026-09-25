#!/usr/bin/env nu
# Runs tests/corpus/*.nix against ietsp and compares with the .exp files.
# `--update [NAME...]` regenerates the .exp files from nix-instantiate
# ($NIX overrides it) instead. Runs from tests/ so paths print as /pwd/....
use lib.nu [ROOT chomp env-cmd flags-of glob-sorted ietsp-bin require-real-nix show]

def main [--update, ...names: string] {
  let ietsp = if $update { "" } else { ietsp-bin }
  $env.HOME = "/fake-home"
  $env.TEST_VAR = "foo"
  cd ($ROOT | path join tests)
  let pwd = $env.PWD
  let files = if ($names | is-empty) {
    glob-sorted corpus/*.nix
  } else {
    $names | each {|t| $"corpus/($t | str replace -r '\.nix$' '').nix" }
  }
  if $update { update $files $pwd }
  mut pass = 0
  mut failed = []
  for f in $files {
    let n = ($f | str replace -r '\.nix$' '')
    if not ($"($n).exp" | path exists) { continue }
    let r = (^$ietsp eval --strict ...(flags-of $n) $f | complete)
    let out = ($r.stdout | str replace -a $pwd "/pwd" | chomp)
    if $out == (open --raw $"($n).exp" | chomp) {
      $pass += 1
    } else {
      $failed = ($failed | append ($n | path basename))
    }
  }
  print $"pass=($pass) fail=($failed | length)"
  print ($failed | str join "\n")
  if ($failed | is-not-empty) { exit 1 }
}

# Writes each FILE's .exp from nix-instantiate and exits: 0, or nix's code
# for the last file that failed to evaluate (its .exp is removed).
def update [files: list<string>, pwd: string] {
  let nix = (env-cmd NIX nix-instantiate)
  require-real-nix ($nix | first)
  mut last = 0
  for f in $files {
    let n = ($f | str replace -r '\.nix$' '')
    let flags = (flags-of $n)
    let r = (^($nix | first) ...($nix | skip 1) --eval --strict --option pure-eval false ...$flags $f | complete)
    if $r.exit_code == 0 {
      $r.stdout | str replace -a $pwd "/pwd" | save -f $"($n).exp"
      print $"updated ($n).exp"
      $last = 0
    } else {
      rm -f $"($n).exp"
      print $"FAILED ($f) \(no .exp written\)"
      ($r.stdout + $r.stderr) | lines | last 3 | show
      $last = $r.exit_code
    }
  }
  exit $last
}

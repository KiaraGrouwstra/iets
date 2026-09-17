#!/usr/bin/env nu
# Runs tests/corpus/*.nix against ietsp and compares with the .exp captured
# by scripts/corpus-update.nu. Runs from tests/ so paths print as /pwd/....
use lib.nu [chomp flags-of glob-sorted]

def main [] {
  cd ($env.FILE_PWD | path join "..")
  let ietsp = ($env.IETSP? | default ($env.PWD | path join "target/release/ietsp"))
  $env.HOME = "/fake-home"
  $env.TEST_VAR = "foo"
  cd tests
  let pwd = $env.PWD
  mut pass = 0
  mut fail = 0
  mut failed = []
  for f in (glob-sorted corpus/*.nix) {
    let n = ($f | str replace -r '\.nix$' '')
    if not ($"($n).exp" | path exists) { continue }
    let r = (^$ietsp eval --strict ...(flags-of $n) $f | complete)
    let out = ($r.stdout | str replace -a $pwd "/pwd" | chomp)
    if $out == (open --raw $"($n).exp" | chomp) {
      $pass += 1
    } else {
      $fail += 1
      $failed = ($failed | append ($n | path basename))
    }
  }
  print $"pass=($pass) fail=($fail)"
  print ($failed | str join "\n")
  if $fail != 0 { exit 1 }
}

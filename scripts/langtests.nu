#!/usr/bin/env nu
# Runs tests/lang/eval-okay-*.nix and eval-fail-*.nix against ietsp, from
# the tests/ directory like Nix's own harness (paths print as /pwd/lang/...).
use lib.nu [chomp flags-of exit-code glob-sorted]

def main [] {
  cd ($env.FILE_PWD | path join "..")
  let ietsp = ($env.IETSP? | default ($env.PWD | path join "target/release/ietsp"))
  $env.HOME = "/fake-home"
  $env.TEST_VAR = "foo" # for eval-okay-getenv.nix
  cd tests
  $env.NIX_PATH = "lang/dir3:lang/dir4"
  let pwd = $env.PWD
  mut pass = 0
  mut fail = 0
  mut failed = []
  for f in (glob-sorted lang/eval-okay-*.nix) {
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
  for f in (glob-sorted lang/eval-fail-*.nix) {
    let n = ($f | str replace -r '\.nix$' '')
    let rc = (exit-code $ietsp eval --strict ...(flags-of $n) $f)
    if $rc == 0 {
      $fail += 1
      $failed = ($failed | append $"($n | path basename) \(should fail\)")
    } else if $rc >= 128 {
      $fail += 1
      $failed = ($failed | append $"($n | path basename) \(crashed rc=($rc)\)")
    } else {
      $pass += 1
    }
  }
  print $"pass=($pass) fail=($fail)"
  print ($failed | first ($env.SHOW? | default 40 | into int) | str join "\n")
}

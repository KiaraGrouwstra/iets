#!/usr/bin/env nu
# Regenerates tests/corpus/*.exp from nix-instantiate. Pass test names to
# update a subset. Runs from tests/ so paths print as /pwd/....
use lib.nu [flags-of show env-cmd glob-sorted]

def main [...tests: string] {
  cd ($env.FILE_PWD | path join "..")
  let nix = (env-cmd NIX nix-instantiate)
  $env.HOME = "/fake-home"
  $env.TEST_VAR = "foo"
  cd tests
  let pwd = $env.PWD
  let files = if ($tests | is-empty) {
    glob-sorted corpus/*.nix
  } else {
    $tests | each {|t| $"corpus/($t | str replace -r '\.nix$' '').nix" }
  }
  # exit code of the last file, like the bash loop: 0, or nix's on failure
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
      let r2 = (^($nix | first) ...($nix | skip 1) --eval --strict ...$flags $f | complete)
      ($r2.stdout + $r2.stderr) | lines | last 3 | show
      $last = $r2.exit_code
    }
  }
  exit $last
}

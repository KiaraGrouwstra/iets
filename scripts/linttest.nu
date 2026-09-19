#!/usr/bin/env nu
# One fixture per lint in tests/lint/<name>.nix (docs/lints.md). Each runs
# three times, from tests/ so paths print as /pwd/lint/...:
#   default levels: no `warning: [` line at all (parity);
#   --lint NAME=warn: stderr equals <name>.warn.exp;
#   --lint NAME=error: exit 1 and the first stderr line is `error: [NAME] ...`.
# Then the framework cases in tests/lint/cases/: IETSP_LINT, ietsp.toml,
# `all=` and `family:` selectors, one warning per site, tryEval not
# catching a lint error, and a warning replayed on a tracing-cache hit.
# UPDATE=1 rewrites the .warn.exp files.
use lib.nu [chomp flags-of glob-sorted verdict]

def norm [pwd: string]: string -> string { $in | str replace -a $pwd "/pwd" | chomp }

def main [] {
  cd ($env.FILE_PWD | path join "..")
  let ietsp = ($env.IETSP? | default ($env.PWD | path join "target/release/ietsp"))
  $env.HOME = "/fake-home"
  cd tests
  let pwd = $env.PWD
  # git refuses to track a `.git` entry, so the fixture gets one at run time
  mkdir lint/git-in-source.d/.git
  mut res = []
  for f in (glob-sorted lint/*.nix) {
    let n = ($f | str replace -r '\.nix$' '')
    let name = ($n | path basename)
    let flags = (flags-of $n)
    let d = (^$ietsp eval --strict --no-cache ...$flags $f | complete)
    let quiet = (($d.stderr | lines | where ($it | str starts-with "warning: [") | where not ($it | str starts-with $"warning: [($name)]") | length) == 0)
    let w = (^$ietsp eval --strict --no-cache ...$flags --lint $"($name)=warn" $f | complete)
    let got = ($w.stderr | norm $pwd)
    if ($env.UPDATE? | default "") == "1" { $got + "\n" | save -f $"($n).warn.exp" }
    let exp = (if ($"($n).warn.exp" | path exists) { open --raw $"($n).warn.exp" | chomp } else { "" })
    let e = (^$ietsp eval --strict --no-cache ...$flags --lint $"($name)=error" $f | complete)
    let first = ($e.stderr | lines | get -o 0 | default "")
    let fires = ($got | str contains $"warning: [($name)]")
    let ok = $quiet and $fires and ($got == $exp) and ($e.exit_code == 1) and ($first | str starts-with $"error: [($name)]")
    let why = (if not $quiet { "fires at the default level" } else if not $fires { "does not fire at warn" } else if $got != $exp { "stderr differs from .warn.exp" } else { $"error run: rc=($e.exit_code) first line '($first)'" })
    $res ++= [(verdict $ok $name $"($name): ($why)")]
  }
  rm -rf lint/git-in-source.d/.git

  let with_warn = "warning: [with] `with` brings unknown names into scope; prefer `inherit` or `let`"
  let r = (with-env {IETSP_LINT: "with=warn"} { ^$ietsp eval --no-cache lint/with.nix | complete })
  $res ++= [(verdict ($r.stderr | str contains $with_warn) "IETSP_LINT sets a level" "IETSP_LINT ignored")]
  let r = (with-env {IETSP_LINT: "with=warn"} { ^$ietsp eval --no-cache --lint with=off lint/with.nix | complete })
  $res ++= [(verdict (not ($r.stderr | str contains "warning:")) "--lint wins over IETSP_LINT" "--lint did not override IETSP_LINT")]
  let r = (^$ietsp eval --no-cache lint/cases/toml/main.nix | complete)
  $res ++= [(verdict ($r.stderr | str contains $with_warn) "ietsp.toml sets a level" "ietsp.toml ignored")]
  let r = (^$ietsp eval --no-cache --lint family:scope=warn lint/with.nix | complete)
  $res ++= [(verdict ($r.stderr | str contains $with_warn) "family:scope selects the family" "family selector ignored")]
  let r = (^$ietsp eval --no-cache --lint all=warn --lint with=off lint/with.nix | complete)
  $res ++= [(verdict (not ($r.stderr | str contains "[with]")) "later --lint wins over all=" "all= not overridden")]
  let r = (^$ietsp eval --strict --no-cache --lint eq-type-mismatch=warn lint/cases/dedup.nix | complete)
  let n = ($r.stderr | lines | where ($it | str starts-with "warning: [eq-type-mismatch]") | length)
  $res ++= [(verdict ($n == 1) "one warning per site" $"one warning per site: got ($n)")]
  let r = (^$ietsp eval --no-cache --lint with=error lint/cases/tryeval.nix | complete)
  $res ++= [(verdict ($r.exit_code == 1 and ($r.stderr | str contains "error: [with]")) "tryEval does not catch a lint error" $"tryEval caught the lint error \(rc=($r.exit_code)\)")]
  let r = (^$ietsp lint list --lint with=error | complete)
  $res ++= [(verdict (($r.stdout | lines | where ($it | str starts-with "with ") | first) =~ 'error') "lint list shows effective levels" "lint list")]
  let r = (^$ietsp eval --no-cache --lint nope=warn lint/with.nix | complete)
  $res ++= [(verdict ($r.exit_code == 2 and ($r.stderr | str contains "unknown lint 'nope'")) "unknown lint name is a usage error" "unknown lint name")]

  let cache = (mktemp -d --tmpdir ietsp-linttest.XXXXXX)
  let r1 = (with-env {IETSP_CACHE_DIR: $cache} { ^$ietsp eval --strict --timings --lint with=warn lint/with.nix | complete })
  let r2 = (with-env {IETSP_CACHE_DIR: $cache} { ^$ietsp eval --strict --timings --lint with=warn lint/with.nix | complete })
  rm -rf $cache
  let hit = ($r2.stderr | str contains "cache: hit")
  let same = (($r1.stderr | lines | where ($it | str starts-with "warning:")) == ($r2.stderr | lines | where ($it | str starts-with "warning:")))
  $res ++= [(verdict ($hit and $same and ($r1.stderr | str contains $with_warn)) "a warning is replayed on a cache hit" $"cache replay: hit=($hit) same=($same)")]

  let fail = ($res | where not $it | length)
  print $"pass=($res | where $it | length) fail=($fail)"
  if $fail != 0 { exit 1 }
}

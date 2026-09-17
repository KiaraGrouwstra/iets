#!/usr/bin/env nu
# Semantics of the tracing evaluation cache (docs/tracing-cache.md): hits
# print identical output, every kind of change misses, uncacheable runs and
# failed runs write nothing, --no-cache touches nothing.
use lib.nu [chomp verdict]

# ietsp eval --strict --timings ARGS: rc, stdout, and the `cache:` lines joined with spaces.
def --wrapped run-eval [ietsp: string, ...args: string]: nothing -> record {
  let r = (^$ietsp eval --strict --timings ...$args | complete)
  {
    rc: $r.exit_code
    out: ($r.stdout | chomp)
    status: ($r.stderr | lines | where $it =~ '^cache:' | each {|l| $l + " " } | str join)
  }
}

# name, mtime and size of every file under DIR
def fingerprint [dir: string]: nothing -> list {
  glob $"($dir)/**/*" | where ($it | path type) == "file" | sort | each {|f| ls -l $f | select name modified size | first }
}

def run-tests [ietsp: string, work: string]: nothing -> int {
  $env.IETSP_CACHE_DIR = $"($work)/cache"
  $env.CACHETEST_VAR = "one"
  mkdir $"($work)/src/dir" $"($work)/inc1" $"($work)/inc2"
  '{ a = import ./lib.nix; b = builtins.attrNames (builtins.readDir ./dir); c = builtins.getEnv "CACHETEST_VAR"; d = import <inc/x.nix>; }' + "\n" | save $"($work)/src/main.nix"
  "40 + 2\n" | save $"($work)/src/lib.nix"
  "x\n" | save $"($work)/src/dir/one"
  "\"inc1\"\n" | save $"($work)/inc1/x.nix"
  "\"inc2\"\n" | save $"($work)/inc2/x.nix"
  let main = ["-I" $"inc=($work)/inc1" $"($work)/src/main.nix"]
  mut res = []

  let r = (run-eval $ietsp ...$main)
  let first = $r.out
  $res ++= [(verdict ($r.rc == 0 and $r.status =~ 'miss \(no entry\).*wrote') "first run misses and writes" $"first run misses and writes \(($r.status)\)")]
  let r = (run-eval $ietsp ...$main)
  $res ++= [(verdict ($r.rc == 0 and ($r.status | str contains "cache: hit") and $r.out == $first) "second run hits with identical output" $"second run hits with identical output \(($r.status)\)")]

  "40 + 3\n" | save -f $"($work)/src/lib.nix"
  let r = (run-eval $ietsp ...$main)
  $res ++= [(verdict ($r.status =~ 'miss \(.*lib\.nix changed' and ($r.out | str contains "a = 43")) "editing an imported file misses" $"editing an imported file misses \(($r.status)\)")]

  ^touch -d 2000-01-01 $"($work)/src/lib.nix"
  let r = (run-eval $ietsp ...$main)
  $res ++= [(verdict ($r.status =~ 'cache: hit.*1 rehashed') "touch on an imported file hits after one rehash" $"touch hits after one rehash \(($r.status)\)")]
  let r = (run-eval $ietsp ...$main)
  $res ++= [(verdict ($r.status =~ 'cache: hit.*0 rehashed') "refreshed stat fields skip the rehash next time" $"refresh persisted \(($r.status)\)")]

  "y\n" | save $"($work)/src/dir/two"
  let r = (run-eval $ietsp ...$main)
  $res ++= [(verdict ($r.status =~ 'miss \(.*directory' and ($r.out | str contains '"two"')) "adding a file to a readDir'd directory misses" $"readDir change misses \(($r.status)\)")]

  let r = (with-env {CACHETEST_VAR: "two"} { run-eval $ietsp ...$main })
  $res ++= [(verdict (($r.status | str contains 'miss ($CACHETEST_VAR changed)') and ($r.out | str contains 'c = "two"')) "changing a getEnv variable misses" $"getEnv change misses \(($r.status)\)")]
  let r = (run-eval $ietsp ...$main)
  $res ++= [(verdict ($r.status =~ 'miss \(.*CACHETEST_VAR') "changing it back misses again (one entry per key)" $"one entry per key \(($r.status)\)")]

  let r = (run-eval $ietsp -I $"inc=($work)/inc2" $"($work)/src/main.nix")
  $res ++= [(verdict (($r.status | str contains "miss (no entry)") and ($r.out | str contains 'd = "inc2"')) "-I change misses (new key)" $"-I change misses \(($r.status)\)")]

  let r = (run-eval $ietsp -E 'builtins.currentTime > 0')
  $res ++= [(verdict ($r.status | str contains "not written") "currentTime never caches" $"currentTime never caches \(($r.status)\)")]
  let r = (run-eval $ietsp -E 'builtins.currentTime > 0')
  $res ++= [(verdict ($r.status | str contains "miss (no entry)") "currentTime: still no entry on the second run" $"currentTime second run \(($r.status)\)")]

  let r = (run-eval $ietsp -E '(builtins.tryEval (builtins.fetchTarball "https://x/y.tar.gz")).success')
  $res ++= [(verdict ($r.rc != 0 and not ($r.status | str contains "wrote")) "a fetcher call fails and never caches" $"fetcher never caches \(($r.status)\)")]

  let before = (fingerprint $env.IETSP_CACHE_DIR)
  let r = (run-eval $ietsp --no-cache ...$main)
  $res ++= [(verdict ($r.status == "") "--no-cache prints no cache line" $"--no-cache prints no cache line \(($r.status)\)")]
  run-eval $ietsp --no-cache -E 'import /nonexistent/nocache-probe.nix' | ignore
  let after = (fingerprint $env.IETSP_CACHE_DIR)
  $res ++= [(verdict ($before == $after) "--no-cache never reads or writes" "--no-cache changed the cache dir")]
  let r = (with-env {IETSP_CACHE: "0"} { run-eval $ietsp ...$main })
  $res ++= [(verdict ($r.status == "") "IETSP_CACHE=0 disables the cache" $"IETSP_CACHE=0 \(($r.status)\)")]

  'throw "boom"' + "\n" | save -f $"($work)/src/lib.nix"
  let r = (run-eval $ietsp ...$main)
  $res ++= [(verdict ($r.rc != 0 and not ($r.status | str contains "wrote")) "a failed evaluation writes nothing" $"failed evaluation wrote \(($r.status)\)")]
  "1\n" | save -f $"($work)/src/lib.nix"
  let r = (run-eval $ietsp ...$main)
  $res ++= [(verdict ($r.rc == 0 and ($r.status | str contains "miss (")) "after the failure the fixed file misses, not a stale hit" $"after failure \(($r.status)\)")]

  # identical output on a hit for a lang test with paths and positions
  let t = "tests/lang/eval-okay-getattrpos.nix"
  let o1 = (^$ietsp eval --strict $t | complete | get stdout | chomp)
  let r2 = (^$ietsp eval --strict --timings $t | complete)
  let hits = ($r2.stderr | lines | where ($it | str contains "cache: hit") | length)
  $res ++= [(verdict ($o1 == ($r2.stdout | chomp) and $hits == 1) "lang test hit prints identical output" "lang test hit")]

  let fail = if ($res | all {|r| $r }) { 0 } else { 1 }
  print $"pass=($res | where $it | length) fail=($fail)"
  $fail
}

def main [] {
  cd ($env.FILE_PWD | path join "..")
  let ietsp = ($env.IETSP? | default ($env.PWD | path join "target/release/ietsp"))
  let work = (mktemp -d --tmpdir ietsp-cachetest.XXXXXX)
  let code = (try { run-tests $ietsp $work } catch {|e| print -e $e.rendered; 1 })
  rm -rf $work
  exit $code
}

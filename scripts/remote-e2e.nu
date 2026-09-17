#!/usr/bin/env nu
# End-to-end test of the remote builder: start ietsp-builderd on localhost,
# build a small derivation with a local source tree (exercises Put of
# chunks, a manifest, and a drv), then build hello from the pinned nixpkgs
# (exercises Put of a 600-drv closure, then a warm run with nothing missing).
# Cases 1-3 go through the legacy `.drv` path; cases 4-6 evaluate in memory
# and never read the closure from the local store.
#
# usage: scripts/remote-e2e.nu            # uses target/release
#        BIN=target/debug scripts/remote-e2e.nu
use lib.nu [chomp show]

# Like bash `set -e`: an error unwinds to main, which cleans up and exits 1.
def fail [work: string, msg: string] {
  print -e $"FAIL: ($msg)"
  print -e "--- server log:"
  print -e (open --raw $"($work)/server.log" | chomp)
  error make -u {msg: "fail"}
}

# stdout of CMD; a non-zero exit is an error (set -e)
def --wrapped out-of [...cmd: string]: nothing -> string {
  let r = (^($cmd | first) ...($cmd | skip 1) | complete)
  if $r.exit_code != 0 { error make -u {msg: $"($cmd | str join ' ') failed \(exit ($r.exit_code)\)"} }
  $r.stdout
}
def --wrapped quiet-ok [...cmd: string]: nothing -> bool { (^($cmd | first) ...($cmd | skip 1) | complete | get exit_code) == 0 }
# `grep -E RE FILE` under set -e: prints the matches, fails when there are none
def show-matching [re: string, file: string] {
  let l = (open --raw $file | lines | where $it =~ $re)
  if ($l | is-empty) { error make -u {msg: $"no ($re) lines in ($file)"} }
  $l | show
}
# `open --raw` is a byte stream; == against a string is always false, so compare bytes
def bytes-of [f: string]: nothing -> binary { open --raw $f | into binary }
def same-file [a: string, b: string]: nothing -> bool { (bytes-of $a) == (bytes-of $b) }
def has-listening [log: string]: nothing -> bool { ($log | path exists) and (open --raw $log | str contains "listening") }

# ietsp build ARGS with the client log in FILE; the output path, or fail
def --wrapped build [ctx: record, log: string, ...args: string]: nothing -> string {
  let r = (^$"($ctx.bin)/ietsp" build --builder $ctx.url --timings ...$args | complete)
  $r.stderr | save -f $"($ctx.work)/($log)"
  if $r.exit_code != 0 { print ($r.stderr | chomp); fail $ctx.work "client exited non-zero" }
  $r.stdout | chomp
}

# the job id of a fresh ietsp-builderd
def start-server [ctx: record]: nothing -> int {
  let bin = $ctx.bin
  let listen = $"127.0.0.1:($ctx.port)"
  let store = $"($ctx.work)/builder"
  let log = $"($ctx.work)/server.log"
  job spawn { ^$"($bin)/ietsp-builderd" --listen $listen --store-dir $store e>> $log }
}

def run-tests [ctx: record] {
  let work = $ctx.work
  let bin = $ctx.bin
  let nixpkgs = $ctx.nixpkgs
  mut srv = (start-server $ctx)
  for _ in 1..100 {
    if (has-listening $"($work)/server.log") { break }
    sleep 50ms
  }
  if not (has-listening $"($work)/server.log") {
    print "server did not start"
    print (open --raw $"($work)/server.log" | chomp)
    error make -u {msg: "fail"}
  }

  print "== 1. custom derivation with a source tree"
  mkdir $"($work)/src/sub"
  $"hello (random int 0..32767) ($nu.pid) (date now | into int)\n" | save $"($work)/src/a"
  "#!/bin/sh\necho hi\n" | save $"($work)/src/run.sh"
  ^chmod +x $"($work)/src/run.sh"
  "zzz\n" | save $"($work)/src/sub/b"
  ^ln -s a $"($work)/src/link"
  let system = (^nix-instantiate --eval -E builtins.currentSystem | str replace -a '"' '' | chomp)
  r#'derivation {
  name = "ietsp-e2e";
  system = "@SYSTEM@";
  builder = "/bin/sh";
  # busybox sh has no cat; use builtins only
  args = [ "-c" "while read -r l; do echo \"$l\"; done < ${@WORK@/src}/a > $out; while read -r l; do echo \"$l\"; done < ${@WORK@/src}/sub/b >> $out" ];
}
'# | str replace -a "@SYSTEM@" $system | str replace -a "@WORK@" $work | save $"($work)/e2e.nix"
  let drv = (^nix-instantiate $"($work)/e2e.nix" | chomp)
  let src = (out-of nix-store -q --references $drv | chomp)
  let expected = (out-of nix-store -q --outputs $drv | chomp)
  print $"drv: ($drv)"
  print $"src: ($src)"
  cp $drv $"($work)/"
  # Forget both so the builder has to upload and register them.
  if (quiet-ok nix-store --delete $drv $src) {
    print "deleted drv and source from the local store"
    if (quiet-ok nix-store --check-validity $src) { fail $work "source still valid after delete" }
  } else {
    print $"note: could not delete ($drv)/($src) from the store; upload still runs, registration is a no-op"
  }
  let out = (build $ctx client1.log --source $"($src)=($work)/src" $"($work)/($drv | path basename)")
  print (open --raw $"($work)/client1.log" | chomp)
  if $out != $expected { fail $work $"output '($out)' != expected '($expected)'" }
  if not ($out | path exists) { fail $work "output path does not exist" }
  if ((bytes-of $"($work)/src/a") ++ (bytes-of $"($work)/src/sub/b")) != (bytes-of $out) { fail $work "output content differs" }
  if not (quiet-ok nix-store --check-validity $drv $src) { fail $work "drv or source not valid after build" }
  if not (quiet-ok test -x $"($src)/run.sh") { fail $work "restored tree lost the executable bit" }
  if (^readlink $"($src)/link" | chomp) != "a" { fail $work "restored tree lost the symlink" }
  print $"ok: ($out)"

  print "== 2. hello from nixpkgs (cold builder, warm local store)"
  let hello = (^nix-instantiate -I $"nixpkgs=($nixpkgs)" '<nixpkgs>' -A hello err> /dev/null | chomp)
  let expected = (out-of nix-store -q --outputs $hello | chomp)
  let out = (build $ctx client2.log $hello)
  show-matching '^(plan|connect|have|put|build):' $"($work)/client2.log"
  if $out != $expected { fail $work $"hello output '($out)' != expected '($expected)'" }
  print $"ok: ($out)"

  print "== 3. hello again (warm builder)"
  let out = (build $ctx client3.log $hello)
  show-matching '^(plan|connect|have|put|build):' $"($work)/client3.log"
  if not (open --raw $"($work)/client3.log" | lines | any {|l| $l =~ 'have: .* 0 missing' }) { fail $work "warm run still had missing objects" }
  if $out != $expected { fail $work $"hello output '($out)' != expected '($expected)'" }
  print $"ok: ($out)"

  print "== 4. eval path: custom derivation, drv and source absent on the builder"
  mkdir $"($work)/src2"
  $"eval (random int 0..32767) ($nu.pid) (date now | into int)\n" | save $"($work)/src2/somefile"
  r#'derivation {
  name = "ietsp-e2e-eval";
  system = "@SYSTEM@";
  builder = "/bin/sh";
  args = [ "-c" "while read -r l; do echo \"$l\"; done < ${@WORK@/src2/somefile} > $out" ];
}
'# | str replace -a "@SYSTEM@" $system | str replace -a "@WORK@" $work | save $"($work)/e2e-eval.nix"
  let drv = (^nix-instantiate $"($work)/e2e-eval.nix" | chomp)
  let src = (out-of nix-store -q --references $drv | chomp)
  let expected = (out-of nix-store -q --outputs $drv | chomp)
  if not (quiet-ok nix-store --delete $drv $src) { print $"note: could not delete ($drv)/($src) from the store" }
  let out = (build $ctx client4.log $"($work)/e2e-eval.nix")
  show-matching '^(eval|plan|connect|have|put|build):' $"($work)/client4.log"
  if (open --raw $"($work)/client4.log" | str contains "read from /nix/store") { fail $work "eval path fell back to the local store" }
  if $out != $expected { fail $work $"output '($out)' != expected '($expected)'" }
  if not (same-file $"($work)/src2/somefile" $out) { fail $work "output content differs" }
  print $"ok: ($out)"

  print "== 4b. eval path: toFile text that references a copied source no drv lists"
  mkdir $"($work)/src3"
  $"text (random int 0..32767) ($nu.pid) (date now | into int)\n" | save $"($work)/src3/inner"
  r#'derivation {
  name = "ietsp-e2e-text";
  system = "@SYSTEM@";
  builder = "/bin/sh";
  args = [ "-c" "read -r p < ${builtins.toFile "pointer" "${@WORK@/src3/inner}\n"}; while read -r l; do echo \"$l\"; done < \"$p\" > $out" ];
}
'# | str replace -a "@SYSTEM@" $system | str replace -a "@WORK@" $work | save $"($work)/e2e-text.nix"
  let drv = (^nix-instantiate $"($work)/e2e-text.nix" | chomp)
  let expected = (out-of nix-store -q --outputs $drv | chomp)
  let refs = (out-of nix-store -q --references $drv | lines)
  let inner = (^nix-store -q --references ...$refs | complete | get stdout | lines | where ($it | str ends-with "-inner"))
  if not (quiet-ok nix-store --delete $drv ...$refs ...$inner) { print $"note: could not delete ($drv) and its sources from the store" }
  let out = (build $ctx client4b.log $"($work)/e2e-text.nix")
  show-matching '^(eval|plan|connect|have|put|build):' $"($work)/client4b.log"
  if $out != $expected { fail $work $"output '($out)' != expected '($expected)'" }
  if not (same-file $"($work)/src3/inner" $out) { fail $work "output content differs" }
  print $"ok: ($out)"

  print "== 5. eval path: hello (cold builder)"
  rm -rf $"($work)/builder"
  job kill $srv
  while ($srv in (job list | get id)) { sleep 10ms }
  $srv = (start-server $ctx)
  sleep 300ms
  let expected = (out-of nix-store -q --outputs $hello | chomp)
  let out = (build $ctx client5.log -I $"nixpkgs=($nixpkgs)" -E '(import <nixpkgs> {}).hello')
  show-matching '^(eval|plan|connect|have|put|build):' $"($work)/client5.log"
  if (open --raw $"($work)/client5.log" | str contains "read from /nix/store") { fail $work "eval path fell back to the local store" }
  if $out != $expected { fail $work $"hello output '($out)' != expected '($expected)'" }
  print $"ok: ($out)"

  print "== 6. eval path: hello (warm builder)"
  let out = (build $ctx client6.log -I $"nixpkgs=($nixpkgs)" -E '(import <nixpkgs> {}).hello')
  show-matching '^(eval|plan|connect|have|put|build):' $"($work)/client6.log"
  if not (open --raw $"($work)/client6.log" | lines | any {|l| $l =~ 'have: .* 0 missing' }) { fail $work "warm run still had missing objects" }
  if $out != $expected { fail $work $"hello output '($out)' != expected '($expected)'" }
  print $"ok: ($out)"
  print "PASS"
}

def main [] {
  cd ($env.FILE_PWD | path join "..")
  $env.NIX_REMOTE = ($env.NIX_REMOTE? | default "daemon")
  $env.TMPDIR = ($env.TMPDIR? | default "/tmp")
  let port = (20000 + ((random int 0..32767) mod 20000))
  let ctx = {
    bin: ($env.BIN? | default "target/release")
    nixpkgs: ($env.NIXPKGS? | default "/nix/store/ah3pxz4xa52hd6vhbx17vcsci0b8bmbr-source")
    work: (mktemp -d --tmpdir ietsp-e2e.XXXXXX)
    port: $port
    url: $"http://127.0.0.1:($port)"
  }
  let code = (try { run-tests $ctx; 0 } catch {|e| if $e.msg != "fail" { print -e $e.rendered }; 1 })
  # cleanup, as the bash EXIT trap: the builder is the only job
  for j in (job list | get id) { job kill $j }
  rm -rf $ctx.work
  exit $code
}

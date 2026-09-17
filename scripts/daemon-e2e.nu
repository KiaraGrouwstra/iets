#!/usr/bin/env nu
# End-to-end test of the worker-protocol fallback (`--builder daemon`,
# `unix://`): the same cases as remote-e2e.nu against a Nix daemon.
#
# 1. custom derivation with a source tree through the local daemon, drv and
#    source deleted first; checks that the registered paths are byte-identical
#    to what nix-instantiate created (nix-store -q --hash, nix derivation show,
#    nix path-info ca), and --fetch-to.
# 2. hello through the local daemon: the closure is GC-rooted here, so every
#    path is valid and only QueryValidPaths and the realise run.
# 3. hello cold against a scratch chroot daemon (nix-daemon --store
#    local?root=DIR on its own socket), --no-build: 776 paths uploaded in one
#    AddMultipleToStore; hashes and ca checked in that store.
# 4. hello warm against the scratch daemon: 0 missing.
# 5. custom derivation through unix:///nix/var/nix/daemon-socket/socket.
# 6. custom derivation through ssh-ng://, with an `ssh` shim in PATH that runs
#    `nix-daemon --stdio` locally: the stdio transport without a remote host.
#
# usage: scripts/daemon-e2e.nu            # uses target/release
#        BIN=target/debug scripts/daemon-e2e.nu
use lib.nu [chomp show]

# Like bash `set -e`: an error unwinds to main, which cleans up and exits 1.
def fail [msg: string] {
  print -e $"FAIL: ($msg)"
  error make -u {msg: "fail"}
}

# stdout of CMD; a non-zero exit is an error (set -e)
def --wrapped out-of [...cmd: string]: nothing -> string {
  let r = (^($cmd | first) ...($cmd | skip 1) | complete)
  if $r.exit_code != 0 { error make -u {msg: $"($cmd | str join ' ') failed \(exit ($r.exit_code)\)"} }
  $r.stdout
}
def --wrapped quiet-ok [...cmd: string]: nothing -> bool { (^($cmd | first) ...($cmd | skip 1) | complete | get exit_code) == 0 }
def lines-matching [re: string, file: string]: nothing -> list<string> { open --raw $file | lines | where $it =~ $re }
# `grep -E RE FILE` under set -e: prints the matches, fails when there are none
def show-matching [re: string, file: string] {
  let l = (lines-matching $re $file)
  if ($l | is-empty) { error make -u {msg: $"no ($re) lines in ($file)"} }
  $l | show
}
def path-info [fields: string, p: string, ...store: string]: nothing -> string {
  let r = (^nix path-info ...$store --json $p | complete)
  if $r.exit_code != 0 { error make -u {msg: $"nix path-info ($p) failed"} }
  $r.stdout | ^jq -c $".[] | ($fields)"
}
# `open --raw` is a byte stream; == against a string is always false, so compare bytes
def bytes-of [f: string]: nothing -> binary { open --raw $f | into binary }
def same-file [a: string, b: string]: nothing -> bool { (bytes-of $a) == (bytes-of $b) }

# make-case DIR NAME: writes DIR/src and DIR/case.nix, a derivation that copies the tree.
def make-case [dir: string, name: string, system: string] {
  mkdir $"($dir)/src/sub"
  $"hello (random int 0..32767) ($nu.pid) (date now | into int)\n" | save $"($dir)/src/a"
  "#!/bin/sh\necho hi\n" | save $"($dir)/src/run.sh"
  ^chmod +x $"($dir)/src/run.sh"
  "zzz\n" | save $"($dir)/src/sub/b"
  ^ln -s a $"($dir)/src/link"
  r#'derivation {
  name = "@NAME@";
  system = "@SYSTEM@";
  builder = "/bin/sh";
  args = [ "-c" "while read -r l; do echo \"$l\"; done < ${@DIR@/src}/a > $out; while read -r l; do echo \"$l\"; done < ${@DIR@/src}/sub/b >> $out" ];
}
'# | str replace -a "@NAME@" $name | str replace -a "@SYSTEM@" $system | str replace -a "@DIR@" $dir | save $"($dir)/case.nix"
}

# run-custom DIR URL: instantiate, record what Nix created, delete, build via URL, compare.
def run-custom [bin: string, dir: string, url: string] {
  let drv = (^nix-instantiate $"($dir)/case.nix" err> /dev/null | chomp)
  let src = (out-of nix-store -q --references $drv | chomp)
  let expected = (out-of nix-store -q --outputs $drv | chomp)
  print $"drv: ($drv)"
  print $"src: ($src)"
  cp $drv $"($dir)/nix.drv"
  out-of nix-store -q --hash $drv $src | save -f $"($dir)/nix.hashes"
  ^nix derivation show $drv err> /dev/null | save -f $"($dir)/nix.show"
  path-info "{ca, narHash, narSize, references}" $src | save -f $"($dir)/nix.src-info"
  path-info "{ca, narHash, narSize, references}" $drv | save -f $"($dir)/nix.drv-info"
  if not (quiet-ok nix-store --delete $drv $src) { fail $"cannot delete ($drv) and ($src) from the store" }
  if (quiet-ok nix-store --check-validity $src) { fail "source still valid after delete" }
  let r = (^$"($bin)/ietsp" build --builder $url --timings --no-cache --fetch-to $"($dir)/fetch" $"($dir)/case.nix" | complete)
  $r.stderr | save -f $"($dir)/client.log"
  if $r.exit_code != 0 { print ($r.stderr | chomp); fail "client exited non-zero" }
  let out = ($r.stdout | chomp)
  show-matching '^(eval|plan|connect|query|add|build|outputs|fetch|worker):' $"($dir)/client.log"
  if ($r.stderr | str contains "read from /nix/store") { fail "eval path fell back to the local store" }
  if $out != $expected { fail $"output '($out)' != expected '($expected)'" }
  if ((bytes-of $"($dir)/src/a") ++ (bytes-of $"($dir)/src/sub/b")) != (bytes-of $out) { fail "output content differs" }
  if not (quiet-ok nix-store --check-validity $drv $src) { fail "drv or source not valid after build" }
  if not (same-file $drv $"($dir)/nix.drv") { fail "registered drv differs from nix-instantiate's" }
  if (out-of nix-store -q --hash $drv $src | into binary) != (bytes-of $"($dir)/nix.hashes") { fail "nix-store -q --hash differs" }
  if (^nix derivation show $drv err> /dev/null | into binary) != (bytes-of $"($dir)/nix.show") { fail "nix derivation show differs" }
  if (path-info "{ca, narHash, narSize, references}" $src | into binary) != (bytes-of $"($dir)/nix.src-info") { fail "source path-info differs" }
  if (path-info "{ca, narHash, narSize, references}" $drv | into binary) != (bytes-of $"($dir)/nix.drv-info") { fail "drv path-info differs" }
  if not (quiet-ok test -x $"($src)/run.sh") { fail "restored tree lost the executable bit" }
  if (^readlink $"($src)/link" | chomp) != "a" { fail "restored tree lost the symlink" }
  if not (same-file $"($dir)/fetch/($out | path basename)" $out) { fail "--fetch-to copy differs from the output" }
  print $"ok: ($out)"
}

def run-tests [bin: string, nixpkgs: string, work: string] {
  let system = (^nix-instantiate --eval -E builtins.currentSystem | str replace -a '"' '' | chomp)

  print "== 1. custom derivation with a source tree, --builder daemon"
  make-case $"($work)/c1" ietsp-daemon-e2e $system
  run-custom $bin $"($work)/c1" daemon

  print "== 2. hello through the local daemon (closure already valid)"
  let hello = (^nix-instantiate -I $"nixpkgs=($nixpkgs)" '<nixpkgs>' -A hello err> /dev/null | chomp)
  let expected = (out-of nix-store -q --outputs $hello | chomp)
  let r = (^$"($bin)/ietsp" build --builder daemon --timings --no-cache -I $"nixpkgs=($nixpkgs)" -E '(import <nixpkgs> {}).hello' | complete)
  $r.stderr | save -f $"($work)/client2.log"
  if $r.exit_code != 0 { print ($r.stderr | chomp); fail "client exited non-zero" }
  let out = ($r.stdout | chomp)
  show-matching '^(eval|plan|connect|query|add|build|outputs|worker):' $"($work)/client2.log"
  if ($r.stderr | str contains "read from /nix/store") { fail "eval path fell back to the local store" }
  if $out != $expected { fail $"hello output '($out)' != expected '($expected)'" }
  print $"ok: ($out)"

  print "== 3. hello cold against a scratch chroot daemon, --no-build"
  let scratch = $"local?root=($work)/store"
  let log = $"($work)/scratch.log"
  job spawn { ^nix-daemon --store $scratch o+e> $log } | ignore
  let sock = $"($work)/store/nix/var/nix/daemon-socket/socket"
  for _ in 1..200 {
    if ($sock | path exists) { break }
    sleep 50ms
  }
  if not ($sock | path exists) { print (open --raw $log | chomp); fail "scratch daemon did not start" }
  let r = (^$"($bin)/ietsp" build --builder $"unix://($sock)" --timings --no-cache --no-build -I $"nixpkgs=($nixpkgs)" -E '(import <nixpkgs> {}).hello' | complete)
  $r.stderr | save -f $"($work)/client3.log"
  if $r.exit_code != 0 { print ($r.stderr | chomp); fail "client exited non-zero" }
  show-matching '^(eval|plan|connect|query|add|build|outputs|worker):' $"($work)/client3.log"
  if not ($r.stderr | str contains "query: 776 paths, 776 missing") { fail "cold scratch store should miss every path" }
  if not ($r.stderr | str contains "worker: 3 ops") { fail "expected SetOptions + QueryValidPaths + AddMultipleToStore" }
  if (out-of nix-store --store $scratch -q --hash $hello) != (out-of nix-store -q --hash $hello) { fail "hello drv hash differs in the scratch store" }
  if not (same-file $"($work)/store($hello)" $hello) { fail "hello drv bytes differ in the scratch store" }
  let shown = (^nix derivation show --store $scratch $hello err> /dev/null | complete)
  let name = ($shown.stdout | from json | get derivations | transpose k v | where k == ($hello | path basename) | get -o 0.v.name)
  if $shown.exit_code != 0 or $name != "hello-2.12.3" { fail "nix derivation show failed in the scratch store" }
  for p in (out-of nix-store -q --references $hello | lines | where not ($it | str ends-with ".drv")) {
    let a = (^nix path-info --store $scratch --json $p err> /dev/null | ^jq -c '.[] | {ca, narHash, references}' | chomp)
    let b = (^nix path-info --json $p err> /dev/null | ^jq -c '.[] | {ca, narHash, references}' | chomp)
    if $a != $b { fail $"path-info differs for ($p): ($a) vs ($b)" }
  }
  print "ok: 776 paths registered, hashes and ca match the local store"

  print "== 4. hello warm against the scratch daemon"
  let r = (^$"($bin)/ietsp" build --builder $"unix://($sock)" --timings --no-cache --no-build -I $"nixpkgs=($nixpkgs)" -E '(import <nixpkgs> {}).hello' | complete)
  $r.stderr | save -f $"($work)/client4.log"
  if $r.exit_code != 0 { print ($r.stderr | chomp); fail "client exited non-zero" }
  show-matching '^(eval|plan|connect|query|add|build|outputs|worker):' $"($work)/client4.log"
  if not ($r.stderr | str contains "query: 776 paths, 0 missing") { fail "warm run still had missing paths" }
  print "ok"

  print "== 5. custom derivation, --builder unix:///nix/var/nix/daemon-socket/socket"
  make-case $"($work)/c5" ietsp-daemon-e2e-unix $system
  run-custom $bin $"($work)/c5" unix:///nix/var/nix/daemon-socket/socket

  print "== 6. custom derivation, --builder ssh-ng://shim (ssh shim runs nix-daemon --stdio)"
  mkdir $"($work)/bin"
  "#!/bin/sh\n# ignore the host and the remote command: run the daemon on stdio here\nexec nix-daemon --stdio\n" | save $"($work)/bin/ssh"
  ^chmod +x $"($work)/bin/ssh"
  make-case $"($work)/c6" ietsp-daemon-e2e-ssh $system
  with-env {PATH: ($env.PATH | prepend $"($work)/bin")} { run-custom $bin $"($work)/c6" ssh-ng://shim }
  print "PASS"
}

def main [] {
  cd ($env.FILE_PWD | path join "..")
  let bin = ($env.BIN? | default "target/release")
  let nixpkgs = ($env.NIXPKGS? | default "/nix/store/ah3pxz4xa52hd6vhbx17vcsci0b8bmbr-source")
  $env.NIX_REMOTE = ($env.NIX_REMOTE? | default "daemon")
  $env.TMPDIR = ($env.TMPDIR? | default "/tmp")
  let work = (mktemp -d --tmpdir ietsp-daemon-e2e.XXXXXX)
  let code = (try { run-tests $bin $nixpkgs $work; 0 } catch {|e| if $e.msg != "fail" { print -e $e.rendered }; 1 })
  # cleanup, as the bash EXIT trap: the scratch daemon is the only job
  for j in (job list | get id) { job kill $j }
  # the scratch store's paths are read-only, as in a real store
  ^chmod -R u+w $"($work)/store" err> /dev/null | complete | ignore
  rm -rf $work
  exit $code
}

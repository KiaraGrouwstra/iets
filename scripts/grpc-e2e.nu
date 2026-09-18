#!/usr/bin/env nu
# End-to-end test of the `grpc://` transport against a local nix-grpc-daemon
# in plaintext non-farm mode (`--listen 127.0.0.1:PORT`, no TLS, no niks3), so
# `?insecure=1` is the URL.
#
# 1. custom derivation with a source tree, drv and source deleted first:
#    builds on the daemon's chroot store, checks --fetch-to and the bytes.
# 2. hello cold, --no-build: every path uploaded in one AddMultipleToStore.
# 3. hello warm: 0 missing.
# 4. classic `nix build --store grpc://…?insecure=1 --eval-store auto`
#    compared with the real nix plus its nix-grpc-store plugin.
#
# Farm mode is not covered: --niks3 needs a niks3 instance with build claims
# and a `niks3 push` child, which this host has not got (src/daemon/farm.hh).
#
# usage: nu scripts/grpc-e2e.nu            # uses target/release
#        BIN=target/debug nu scripts/grpc-e2e.nu
#        DAEMON=/nix/store/…-nix-grpc-store-0.1.0 nu scripts/grpc-e2e.nu
use lib.nu [chomp show]

const FLAKE = "github:Mic92/nix-grpc-store/8f3a2ecb58117448f28bae90304c3e24e8e42b79"

def fail [msg: string] {
  print -e $"FAIL: ($msg)"
  error make -u {msg: "fail"}
}

def --wrapped out-of [...cmd: string]: nothing -> string {
  let r = (^($cmd | first) ...($cmd | skip 1) | complete)
  if $r.exit_code != 0 { error make -u {msg: $"($cmd | str join ' ') failed \(exit ($r.exit_code)\)"} }
  $r.stdout
}
def --wrapped quiet-ok [...cmd: string]: nothing -> bool { (^($cmd | first) ...($cmd | skip 1) | complete | get exit_code) == 0 }
def bytes-of [f: string]: nothing -> binary { open --raw $f | into binary }
def same-file [a: string, b: string]: nothing -> bool { (bytes-of $a) == (bytes-of $b) }
def show-matching [re: string, file: string] {
  let l = (open --raw $file | lines | where $it =~ $re)
  if ($l | is-empty) { error make -u {msg: $"no ($re) lines in ($file)"} }
  $l | show
}

# The nix-grpc-store build with bin/nix-grpc-daemon and lib/nix/plugins.
def daemon-package []: nothing -> string {
  if ($env.DAEMON? | is-not-empty) { return $env.DAEMON }
  print $"building ($FLAKE)#default (may take a few minutes)"
  out-of nix build --no-link --print-out-paths $"($FLAKE)#default" | chomp | lines | last
}

# make-case DIR NAME SYSTEM: DIR/src and DIR/case.nix, a drv that copies the tree.
def make-case [dir: string, name: string, system: string] {
  mkdir $"($dir)/src/sub"
  $"hello (random int 0..32767) ($nu.pid) (date now | into int)\n" | save $"($dir)/src/a"
  "zzz\n" | save $"($dir)/src/sub/b"
  r#'derivation {
  name = "@NAME@";
  system = "@SYSTEM@";
  builder = "/bin/sh";
  args = [ "-c" "while read -r l; do echo \"$l\"; done < ${@DIR@/src}/a > $out; while read -r l; do echo \"$l\"; done < ${@DIR@/src}/sub/b >> $out" ];
}
'# | str replace -a "@NAME@" $name | str replace -a "@SYSTEM@" $system | str replace -a "@DIR@" $dir | save $"($dir)/case.nix"
}

# Starts nix-grpc-daemon in front of a scratch chroot store and returns its URL.
def start-daemon [pkg: string, work: string, port: int]: nothing -> string {
  let root = $"($work)/store"
  let sock = $"($work)/nix-daemon.sock"
  mkdir $root
  # The gRPC daemon proxies builds through a worker-protocol socket.
  # NIX_DAEMON_SOCKET_PATH is what uds-remote-store.cc:getDaemonSocketPath
  # reads; a chroot store alone leaves the daemon on the system socket.
  # `builders =` keeps the build on this host: a remote builder from the
  # system nix.conf would need a plugin the scratch daemon cannot load.
  job spawn {
    with-env {NIX_DAEMON_SOCKET_PATH: $sock, NIX_CONFIG: "builders =\nmax-jobs = 4"} {
      ^nix-daemon --store $"local?root=($root)" o+e> $"($work)/nix-daemon.log"
    }
  } | ignore
  for _ in 1..200 {
    if ($sock | path exists) { break }
    sleep 50ms
  }
  if not ($sock | path exists) { print (open --raw $"($work)/nix-daemon.log" | chomp); fail "scratch nix-daemon did not start" }
  # --proxy-store is what the native RPCs read and write. It must be the
  # chroot store itself: through the socket, a LocalFSStore resolves real
  # paths under /nix/store, which this store is not.
  job spawn {
    ^$"($pkg)/bin/nix-grpc-daemon" --listen $"127.0.0.1:($port)" --proxy-socket $sock --proxy-store $"local?root=($root)" o+e> $"($work)/grpc.log"
  } | ignore
  for _ in 1..200 {
    if (quiet-ok bash -c $"exec 3<>/dev/tcp/127.0.0.1/($port)") { break }
    sleep 50ms
  }
  if not (quiet-ok bash -c $"exec 3<>/dev/tcp/127.0.0.1/($port)") {
    print (open --raw $"($work)/grpc.log" | chomp); fail "nix-grpc-daemon did not start"
  }
  $"grpc://127.0.0.1:($port)?insecure=1"
}

def run-tests [bin: string, nixpkgs: string, work: string] {
  let system = (^nix-instantiate --eval -E builtins.currentSystem | str replace -a '"' '' | chomp)
  let pkg = (daemon-package)
  print $"daemon: ($pkg)"
  let port = (9550 + ($nu.pid mod 400))
  let url = (start-daemon $pkg $work $port)
  print $"url: ($url)"
  let scratch = $"local?root=($work)/store"

  print "== 1. custom derivation with a source tree, built on the gRPC daemon"
  make-case $"($work)/c1" ietsp-grpc-e2e $system
  let drv = (^nix-instantiate $"($work)/c1/case.nix" err> /dev/null | chomp)
  let src = (out-of nix-store -q --references $drv | chomp)
  let expected = (out-of nix-store -q --outputs $drv | chomp)
  print $"drv: ($drv)"
  cp $drv $"($work)/c1/nix.drv"
  if not (quiet-ok nix-store --delete $drv $src) { fail $"cannot delete ($drv) and ($src)" }
  let r = (^$"($bin)/ietsp" build --builder $url --timings --no-cache --fetch-to $"($work)/c1/fetch" $"($work)/c1/case.nix" | complete)
  $r.stderr | save -f $"($work)/client1.log"
  if $r.exit_code != 0 { print ($r.stderr | chomp); print (open --raw $"($work)/grpc.log" | chomp); fail "client exited non-zero" }
  let out = ($r.stdout | chomp)
  show-matching '^(eval|plan|connect|query|add|build|outputs|fetch|grpc):' $"($work)/client1.log"
  if $out != $expected { fail $"output '($out)' != expected '($expected)'" }
  if not (quiet-ok nix-store --store $scratch --check-validity $drv $src) { fail "drv or source not valid in the scratch store" }
  if not (same-file $"($work)/store($drv)" $"($work)/c1/nix.drv") { fail "registered drv differs from nix-instantiate's" }
  if ((bytes-of $"($work)/c1/src/a") ++ (bytes-of $"($work)/c1/src/sub/b")) != (bytes-of $"($work)/c1/fetch/($out | path basename)") { fail "--fetch-to copy differs from the built output" }
  print $"ok: ($out), fetched"

  print "== 2. hello cold, --no-build"
  let hello = (^nix-instantiate -I $"nixpkgs=($nixpkgs)" '<nixpkgs>' -A hello err> /dev/null | chomp)
  let r = (^$"($bin)/ietsp" build --builder $url --timings --no-cache --no-build -I $"nixpkgs=($nixpkgs)" -E '(import <nixpkgs> {}).hello' | complete)
  $r.stderr | save -f $"($work)/client2.log"
  if $r.exit_code != 0 { print ($r.stderr | chomp); fail "client exited non-zero" }
  show-matching '^(eval|plan|connect|query|add|grpc):' $"($work)/client2.log"
  let missing = (open --raw $"($work)/client2.log" | lines | where $it =~ '^query:' | get 0)
  if not ($missing =~ 'missing') { fail $"no query line: ($missing)" }
  if ($missing | str contains ", 0 missing") { fail "cold store should miss paths" }
  if (out-of nix-store --store $scratch -q --hash $hello) != (out-of nix-store -q --hash $hello) { fail "hello drv hash differs in the scratch store" }
  if not (same-file $"($work)/store($hello)" $hello) { fail "hello drv bytes differ in the scratch store" }
  print $"ok: ($missing)"

  print "== 3. hello warm, --no-build"
  let r = (^$"($bin)/ietsp" build --builder $url --timings --no-cache --no-build -I $"nixpkgs=($nixpkgs)" -E '(import <nixpkgs> {}).hello' | complete)
  $r.stderr | save -f $"($work)/client3.log"
  if $r.exit_code != 0 { print ($r.stderr | chomp); fail "client exited non-zero" }
  show-matching '^(query|add|grpc):' $"($work)/client3.log"
  if not ($r.stderr | str contains ", 0 missing") { fail "warm run still had missing paths" }
  print "ok"

  print "== 4. classic nix build --store grpc://… --eval-store auto"
  # The nix-grpc-store plugin must load in the `nix` on PATH, or there is no
  # grpc:// reference run; then compare against the worker protocol instead.
  let probe = (^nix store info --store $url | complete)
  let plugin_ok = ($probe.exit_code == 0)
  make-case $"($work)/c4" ietsp-grpc-e2e-classic $system
  mkdir $"($work)/farm/bin"
  for n in [nix-instantiate nix-build nix-eval-jobs nix] { ^ln -sf ($"($bin)/ietsp" | path expand) $"($work)/farm/bin/($n)" }
  let tail = [--eval-store auto --no-link --print-out-paths -f $"($work)/c4/case.nix"]
  let ours = (with-env {IETSP_NO_FALLBACK: "1"} { ^$"($work)/farm/bin/nix" build --store $url ...$tail | complete })
  if $ours.exit_code != 0 { print ($ours.stderr | chomp); print (open --raw $"($work)/grpc.log" | chomp); fail "ours exited non-zero" }
  let ref = if $plugin_ok {
    ^nix build --store $url ...$tail | complete
  } else {
    print "note: the `nix` on PATH loads no matching nix-grpc-store plugin; comparing against the worker protocol on the same store instead"
    ^nix build --store $"unix://($work)/nix-daemon.sock" ...$tail | complete
  }
  if $ref.exit_code != 0 { print ($ref.stderr | chomp); fail "the reference nix exited non-zero" }
  if ($ours.stdout | into binary) != ($ref.stdout | into binary) {
    print $"  ours: ($ours.stdout | chomp)"
    print $"  real: ($ref.stdout | chomp)"
    fail "classic stdout differs from the reference nix"
  }
  print $"ok: same as the reference nix, ($ours.stdout | chomp)"

  print "== 5. classic out-link: the output is fetched back and registered locally"
  make-case $"($work)/c5" ietsp-grpc-e2e-link $system
  let link = $"($work)/c5/result"
  let r = (with-env {IETSP_NO_FALLBACK: "1"} { ^$"($work)/farm/bin/nix" build --store $url --eval-store auto -o $link --print-out-paths -f $"($work)/c5/case.nix" | complete })
  # A real farm signs its outputs with the cache key, so the local daemon
  # accepts them. This scratch daemon builds them unsigned, and registration
  # checks signatures exactly as `nix copy --from` does, so the refusal is the
  # expected outcome here; the fetch and the local import still ran.
  if $r.exit_code != 0 {
    if not ($r.stderr | str contains "lacks a signature by a trusted key") {
      print ($r.stderr | chomp); print (open --raw $"($work)/grpc.log" | chomp); fail "ours exited non-zero"
    }
    if not ((open --raw $"($work)/grpc.log") | str contains "method=FetchNars") { fail "the output was never fetched back" }
    if not ($r.stderr | str contains "trusted-public-keys") { fail "the signature error carries no hint" }
    print "ok: fetched back, and the local daemon refused the unsigned scratch output as `nix copy --from` would"
  } else {
    let out = ($r.stdout | chomp)
    if (^readlink $link | chomp) != $out { fail $"($link) points elsewhere" }
    if not (quiet-ok nix-store --check-validity $out) { fail $"($out) is not valid in the local store" }
    print $"ok: ($link) -> ($out), valid locally"
  }
  print "PASS"
}

def main [] {
  cd ($env.FILE_PWD | path join "..")
  let bin = ($env.BIN? | default "target/release")
  let nixpkgs = ($env.NIXPKGS? | default "/nix/store/ah3pxz4xa52hd6vhbx17vcsci0b8bmbr-source")
  $env.NIX_REMOTE = ($env.NIX_REMOTE? | default "daemon")
  $env.TMPDIR = ($env.TMPDIR? | default "/tmp")
  let work = (mktemp -d --tmpdir ietsp-grpc-e2e.XXXXXX)
  let code = (try { run-tests $bin $nixpkgs $work; 0 } catch {|e| if $e.msg != "fail" { print -e $e.rendered }; 1 })
  for j in (job list | get id) { job kill $j }
  ^chmod -R u+w $"($work)/store" err> /dev/null | complete | ignore
  rm -rf $work
  exit $code
}

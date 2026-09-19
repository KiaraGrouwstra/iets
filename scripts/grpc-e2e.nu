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
# 5. the out-link: the output is fetched back and registered locally.
# 6. `ietsp fast-build --store grpc://…`: the same result file as the same
#    flake built over the worker protocol on the same store.
# 7. a multi-root `ietsp build` over grpc: one progress line per attribute.
#
# Farm mode is not covered: --niks3 needs a niks3 instance with build claims
# and a `niks3 push` child, which this host has not got (src/daemon/farm.hh).
#
# usage: nu scripts/grpc-e2e.nu            # uses target/release
#        BIN=target/debug nu scripts/grpc-e2e.nu
#        DAEMON=/nix/store/…-nix-grpc-store-0.1.0 nu scripts/grpc-e2e.nu
use lib.nu [ROOT bytes-of chomp current-system fail fastbuild-flake ietsp-bin isolate make-farm nixpkgs out-of quiet-ok run-in same-file scratch-daemon show show-matching]

const FLAKE = "github:Mic92/nix-grpc-store/8f3a2ecb58117448f28bae90304c3e24e8e42b79"

# The nix-grpc-store build with bin/nix-grpc-daemon and lib/nix/plugins.
def daemon-package []: nothing -> string {
  if ($env.DAEMON? | is-not-empty) { return $env.DAEMON }
  print $"building ($FLAKE)#default, this can take a few minutes"
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

# Starts nix-grpc-daemon in front of a scratch chroot store: its URL, and the
# worker-protocol socket of the store behind it.
def start-daemon [pkg: string, work: string, port: int]: nothing -> record {
  let root = $"($work)/store"
  # `builders =` keeps the build on this host: a remote builder from the
  # system nix.conf would need a plugin the scratch daemon cannot load.
  let sock = (scratch-daemon $root --option builders "" --option max-jobs 4)
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
  {url: $"grpc://127.0.0.1:($port)?insecure=1", sock: $sock}
}

def run-tests [ietsp: string, nixpkgs: string, work: string] {
  let system = (current-system)
  let pkg = (daemon-package)
  print $"daemon: ($pkg)"
  let port = (9550 + ($nu.pid mod 400))
  let daemon = (start-daemon $pkg $work $port)
  let url = $daemon.url
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
  let r = (^$ietsp build --builder $url --timings --no-cache --fetch-to $"($work)/c1/fetch" $"($work)/c1/case.nix" | complete)
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
  let r = (^$ietsp build --builder $url --timings --no-cache --no-build -I $"nixpkgs=($nixpkgs)" -E '(import <nixpkgs> {}).hello' | complete)
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
  let r = (^$ietsp build --builder $url --timings --no-cache --no-build -I $"nixpkgs=($nixpkgs)" -E '(import <nixpkgs> {}).hello' | complete)
  $r.stderr | save -f $"($work)/client3.log"
  if $r.exit_code != 0 { print ($r.stderr | chomp); fail "client exited non-zero" }
  show-matching '^(query|add|grpc):' $"($work)/client3.log"
  if not ($r.stderr | str contains ", 0 missing") { fail "warm run still had missing paths" }
  print "ok"

  print "== 4. classic nix build --store grpc://… --eval-store auto"
  # The reference run needs a `nix` whose nix-grpc-store plugin matches its own
  # libnixstore. The dev shell's nix often does not; the system one may.
  let probe = (^nix store info --store $url | complete)
  let sys = "/run/current-system/sw/bin/nix"
  let sys_probe = if ($sys | path exists) { (^$sys store info --store $url | complete) } else { {exit_code: 1} }
  let ref_nix = if $probe.exit_code == 0 { "nix" } else if $sys_probe.exit_code == 0 { $sys } else { "" }
  let plugin_ok = ($ref_nix != "")
  make-case $"($work)/c4" ietsp-grpc-e2e-classic $system
  let farm = (make-farm $ietsp $"($work)/farm")
  let tail = [--eval-store auto --no-link --print-out-paths -f $"($work)/c4/case.nix"]
  let ours = (with-env {IETSP_NO_FALLBACK: "1"} { ^$"($farm)/nix" build --store $url ...$tail | complete })
  if $ours.exit_code != 0 { print ($ours.stderr | chomp); print (open --raw $"($work)/grpc.log" | chomp); fail "ours exited non-zero" }
  let ref = if $plugin_ok {
    print $"reference: ($ref_nix) with its nix-grpc-store plugin"
    ^$ref_nix build --store $url ...$tail | complete
  } else {
    print "note: the `nix` on PATH loads no matching nix-grpc-store plugin; comparing against the worker protocol on the same store instead"
    ^nix build --store $"unix://($daemon.sock)" ...$tail | complete
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
  let r = (with-env {IETSP_NO_FALLBACK: "1"} { ^$"($farm)/nix" build --store $url --eval-store auto -o $link --print-out-paths -f $"($work)/c5/case.nix" | complete })
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

  print "== 6. fast-build over grpc: the same verdicts as over the worker protocol"
  # Two flakes, so neither run finds the other's outputs already built. The
  # attribute names are the same, the nonces are not.
  fastbuild-flake $"($work)/fb-grpc"
  fastbuild-flake $"($work)/fb-worker"
  let verdicts = {|store: string, name: string|
    let f = $"($work)/($name).json"
    let r = (^$ietsp fast-build --store $store --flake $"path:($work)/($name)#checks"
      --systems $system --result-file $f | complete)
    if not ($f | path exists) { print ($r.stderr | chomp); fail $"($name): no result file" }
    {
      exit: $r.exit_code,
      stderr: $r.stderr,
      rows: (open --raw $f | from json | get results
        | each {|x| {type: $x.type, attr: $x.attr, success: $x.success, skipped: ($x.skipped? | default false)} }
        | sort-by type attr),
    }
  }
  let over_grpc = (do $verdicts $url "fb-grpc")
  let over_worker = (do $verdicts $"unix://($daemon.sock)" "fb-worker")
  if $over_grpc.exit != $over_worker.exit {
    fail $"exit ($over_grpc.exit) over grpc, ($over_worker.exit) over the worker protocol"
  }
  if $over_grpc.rows != $over_worker.rows {
    print $"  grpc:   ($over_grpc.rows | to json -r)"
    print $"  worker: ($over_worker.rows | to json -r)"
    fail "the result file differs between the transports"
  }
  if not ($over_grpc.stderr | str contains $"($system).bad") { fail "the failing attribute is not named" }
  if not ($over_grpc.stderr | str contains "about to fail") { fail "the failure log tail is missing" }
  if not ($over_grpc.stderr | str contains "building fb-chain") { fail "the dependent check did not build" }
  print $"ok: ($over_grpc.rows | length) records, exit ($over_grpc.exit), the same either way"

  print "== 7. a multi-root ietsp build over grpc names every attribute"
  make-case $"($work)/c7a" ietsp-grpc-e2e-multi-a $system
  make-case $"($work)/c7b" ietsp-grpc-e2e-multi-b $system
  let expr = $"[ \(import ($work)/c7a/case.nix\) \(import ($work)/c7b/case.nix\) ]"
  let r = (^$ietsp build --builder $url --no-cache -E $expr | complete)
  if $r.exit_code != 0 { print ($r.stderr | chomp); print (open --raw $"($work)/grpc.log" | chomp); fail "multi-root build exited non-zero" }
  for n in [ietsp-grpc-e2e-multi-a ietsp-grpc-e2e-multi-b] {
    if not ($r.stderr | str contains $"building ($n)") { print ($r.stderr | chomp); fail $"no progress line for ($n)" }
  }
  if (($r.stdout | chomp | lines | length) != 2) { fail $"expected two output paths, got ($r.stdout)" }
  print $"ok: ($r.stdout | chomp | lines | length) roots, one progress line each"
  print "PASS"
}

def main [] {
  let ietsp = (ietsp-bin)
  cd $ROOT
  let work = (isolate grpc-e2e)
  run-in $work { run-tests $ietsp (nixpkgs) $work }
}

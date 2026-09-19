# Helpers shared by the scripts in this directory.

# Strip trailing newlines, like bash's `$(...)`.
export def chomp []: string -> string { str trim -r -c "\n" }

# Print a list of lines; nothing for an empty list.
export def show []: list<string> -> nothing { each {|l| print $l } | ignore }

# The first line of NAME.flags split on whitespace, or [].
export def flags-of [n: string]: nothing -> list<string> {
  let f = $"($n).flags"
  if not ($f | path exists) { return [] }
  open --raw $f | lines | get -o 0 | default "" | str trim | split row -r '\s+' | where $it != ""
}

# `diff A B | head -N`. The `let` keeps diff's exit code 1 from raising.
export def diff-head [a: string, b: string, n: int]: nothing -> list<string> {
  let d = (^diff $a $b | lines | first $n)
  $d
}

# Exit code of CMD as bash reports it: 128+N when signal N killed it.
export def --wrapped exit-code [...cmd: string]: nothing -> int {
  let r = (try { ^($cmd | first) ...($cmd | skip 1) | complete } catch {|e| {exit_code: ($e.exit_code? | default 128)} })
  if $r.exit_code < 0 { 128 - $r.exit_code } else { $r.exit_code }
}

# $NAME split on spaces, so NIX="nix eval" style values keep working.
export def env-cmd [name: string, default: string]: nothing -> list<string> {
  $env | get -o $name | default $default | split row " " | where $it != ""
}

# Files matching PAT in bash glob order (strcoll of the current locale).
export def glob-sorted [pat: string]: nothing -> list<string> {
  let names = (ls ($pat | into glob) | get name | str join "\n" | ^sort | lines)
  $names
}

# Print "ok   NAME" or "FAIL DETAIL"; returns OK.
export def verdict [ok: bool, name: string, detail: string]: nothing -> bool {
  if $ok { print $"ok   ($name)" } else { print $"FAIL ($detail)" }
  $ok
}

# The repository root, whatever directory a script runs from.
export const ROOT = (path self | path dirname | path dirname)

# The nixpkgs tree this repo pins; $NIXPKGS overrides it.
export def nixpkgs []: nothing -> string {
  $env.NIXPKGS? | default "/nix/store/ah3pxz4xa52hd6vhbx17vcsci0b8bmbr-source"
}

# The ietsp under test, as an absolute path: $IETSP, else $BIN/ietsp, else
# target/release/ietsp.
export def ietsp-bin []: nothing -> string {
  let p = if ($env.IETSP? | is-not-empty) {
    $env.IETSP | path expand
  } else if ($env.BIN? | is-not-empty) {
    $env.BIN | path join ietsp | path expand
  } else {
    $ROOT | path join target/release/ietsp
  }
  if not ($p | path exists) { error make -u {msg: $"no ($p); cargo build --release first"} }
  $p
}

export def current-system []: nothing -> string {
  ^nix-instantiate --eval -E builtins.currentSystem | str replace -a '"' '' | chomp
}

# Every e2e script calls this first. It returns DIR, or a fresh temporary
# directory named after NAME, and points ietsp's config lookup into it: the
# host's ietsp.conf must not steer the runs (a remote `builder` changes the
# cores, the out-links and where things build).
export def --env isolate [name: string, dir?: string]: nothing -> string {
  $env.TMPDIR = ($env.TMPDIR? | default "/tmp")
  $env.NIX_REMOTE = ($env.NIX_REMOTE? | default "daemon")
  let work = ($dir | default (mktemp -d --tmpdir $"ietsp-($name).XXXXXX"))
  $env.IETSP_CONF_DIR = ($work | path join no-conf)
  $env.XDG_CONFIG_HOME = ($work | path join no-xdg)
  hide-env -i IETSP_CONFIG
  $work
}

# Runs BODY, then kills the jobs it spawned, removes WORK and exits with
# BODY's code: its int result, 0 for any other result, 1 on an error. `fail`
# already printed its message; any other error is printed here.
export def run-in [work: string, body: closure] {
  let code = (try {
    let r = (do $body)
    if ($r | describe) == "int" { $r } else { 0 }
  } catch {|e|
    if $e.msg != "fail" { print -e $e.rendered }
    1
  })
  for j in (job list | get id) { job kill $j }
  cd /
  # A scratch store's paths are read-only, as in a real store.
  ^chmod -R u+w $work err> /dev/null | complete | ignore
  rm -rf $work
  exit $code
}

# Like bash `set -e`: an error that unwinds to `run-in`, which exits 1.
export def fail [msg: string] {
  print -e $"FAIL: ($msg)"
  error make -u {msg: "fail"}
}

# stdout of CMD; a non-zero exit is an error.
export def --wrapped out-of [...cmd: string]: nothing -> string {
  let r = (^($cmd | first) ...($cmd | skip 1) | complete)
  if $r.exit_code != 0 { error make -u {msg: $"($cmd | str join ' ') failed \(exit ($r.exit_code)\)"} }
  $r.stdout
}

export def --wrapped quiet-ok [...cmd: string]: nothing -> bool {
  (^($cmd | first) ...($cmd | skip 1) | complete | get exit_code) == 0
}

# `grep -E RE FILE` under set -e: prints the matches, fails when there are none.
export def show-matching [re: string, file: string] {
  let l = (open --raw $file | lines | where $it =~ $re)
  if ($l | is-empty) { error make -u {msg: $"no ($re) lines in ($file)"} }
  $l | show
}

# `open --raw` is a byte stream; == against a string is always false, so compare bytes.
export def bytes-of [f: string]: nothing -> binary { open --raw $f | into binary }
export def same-file [a: string, b: string]: nothing -> bool { (bytes-of $a) == (bytes-of $b) }

# The directory of the real nix-instantiate on PATH.
export def real-bin-dir []: nothing -> string {
  (^readlink -f (^bash -c "command -v nix-instantiate" | chomp) | chomp | path dirname)
}

# The classic farm in DIR: BIN under Nix's command names, and the real nix it
# falls back to, as `nix build .#classic` lays it out. Returns DIR/bin.
export def make-farm [bin: string, dir: string]: nothing -> string {
  mkdir $"($dir)/bin"
  for n in [nix-instantiate nix-build nix-eval-jobs nix-shell nix-fast-build nix] {
    ^ln -sf $bin $"($dir)/bin/($n)"
  }
  mkdir $"($dir)/share/ietsp"
  (real-bin-dir) | save -f $"($dir)/share/ietsp/real-nix-bin"
  $"($dir)/bin"
}

# scratch-daemon ROOT [OPTIONS]: starts `nix-daemon --store local?root=ROOT` as a job and
# returns its socket path once it listens. `NIX_DAEMON_SOCKET_PATH` pins the socket under
# ROOT; Nix 2.34's daemon binds the system socket otherwise.
export def --wrapped scratch-daemon [root: string, ...options: string]: nothing -> string {
  let sock = $"($root)/nix/var/nix/daemon-socket/socket"
  let log = $"($root).log"
  let store = $"local?root=($root)"
  job spawn { with-env {NIX_DAEMON_SOCKET_PATH: $sock} { ^nix-daemon --store $store ...$options o+e> $log } } | ignore
  for _ in 1..200 {
    if ($sock | path exists) { break }
    sleep 50ms
  }
  if not ($sock | path exists) {
    print -e (open --raw $log | chomp)
    error make -u {msg: $"scratch daemon did not start in ($root)"}
  }
  $sock
}

# The shared build-test flake: four checks and a packages set, no nixpkgs. Every builder
# carries a nonce so each run builds fresh derivations instead of finding the
# previous run's outputs already in the store.
export def fastbuild-flake [dir: string] {
  mkdir $dir
  let nonce = $"(random int 0..999999)-(date now | into int)"
  r#'{
  outputs = { self }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAll = f: builtins.listToAttrs (map (s: { name = s; value = f s; }) systems);
      drv = system: name: args: derivation ({
        inherit name system;
        builder = "/bin/sh";
      } // args);
      nonce = "@NONCE@";
    in {
      checks = forAll (system: rec {
        ok = drv system "fb-ok" {
          args = [ "-c" "echo building fb-ok ${nonce}; echo done > $out" ];
        };
        dep = drv system "fb-dep" {
          args = [ "-c" "echo building fb-dep ${nonce}; echo dep > $out" ];
        };
        chain = drv system "fb-chain" {
          args = [ "-c" "echo building fb-chain ${nonce}; while read -r l; do echo \"$l\"; done < ${dep} > $out" ];
        };
        bad = drv system "fb-bad" {
          args = [ "-c" "echo about to fail ${nonce}; echo line2; exit 1" ];
        };
      });
      packages = forAll (system: {
        lone = drv system "fb-lone" {
          args = [ "-c" "echo building fb-lone ${nonce}; echo lone > $out" ];
        };
      });
    };
}
'# | str replace -a "@NONCE@" $nonce | save -f $"($dir)/flake.nix"
  # `nix flake lock` writes nothing for a flake with no inputs, and ietsp
  # only evaluates locked flakes, so the empty lock is written here.
  '{"nodes":{"root":{}},"root":"root","version":7}' | save -f $"($dir)/flake.lock"
}

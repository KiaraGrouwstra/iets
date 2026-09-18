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

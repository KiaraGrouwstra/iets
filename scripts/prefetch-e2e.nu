#!/usr/bin/env nu
# End-to-end test of `iets prefetch` (docs/reference/commands.md).
#
# 1. a file:// URL, flat: the hash `nix store prefetch-file` and `nix hash
#    file` print for the same bytes.
# 2. the same URL with --unpack: the hash `nix store prefetch-file --unpack`
#    prints, and --json's shape.
# 3. --hash-type sha512.
# 4. the expression form, outputHashMode flat: a derivation that copies what
#    another derivation made, built on a scratch daemon with the hash faked;
#    the answer is `nix hash file` of the same bytes, on stdout, alone.
# 5. the same derivation with outputHashMode recursive: `nix hash path`.
# 6. sparse members: the four GNU and PAX forms, and the fetch-max-size cap
#    on what a sparse member expands to.
# 7. the command lines that are errors: 2 for usage, 1 for a fetch that fails.
#
# Needs a live nix-daemon and `nix` on PATH (the dev shell has nix 2.34.8);
# `nix store prefetch-file` writes to the store, so the flake check runs it in
# a container (nix/e2e-test.nix).
#
# usage: scripts/prefetch-e2e.nu
#        BIN=target/debug scripts/prefetch-e2e.nu
use lib.nu [ROOT chomp current-system exit-code fail iets-bin isolate out-of require-real-nix run-in scratch-daemon verdict]

# The `hash` field of `nix store prefetch-file --json`.
def --wrapped prefetched [...args: string]: nothing -> string {
  (out-of nix store prefetch-file --json ...$args) | from json | get hash
}

# DIR/fod.nix: a fixed-output derivation that copies the same bytes as
# DIR/msg.txt, with a hash that is not the one it produces. Only shell
# builtins run: the sandbox has no coreutils. With SRC `dep` the bytes come
# from another derivation, so the daemon builds a resolved copy of the
# derivation and reports the mismatch in the log instead of the result.
def make-fod [dir: string, mode: string, system: string, src: string = "file"] {
  let from = if $src == "dep" {
    r#'(derivation {
    name = "prefetch-dep";
    system = "@SYSTEM@";
    builder = "/bin/sh";
    args = [ "-c" "echo hello prefetch > $out" ];
  })'#
  } else {
    "@DIR@/msg.txt"
  }
  r#'let src = @FROM@; in
derivation {
  name = "prefetch-msg";
  system = "@SYSTEM@";
  builder = "/bin/sh";
  args = [ "-c" "while IFS= read -r l; do echo \"$l\"; done < ${src} > $out" ];
  outputHash = "sha256-BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBA=";
  outputHashAlgo = "sha256";
  outputHashMode = "@MODE@";
}
'# | str replace -a "@FROM@" $from | str replace -a "@DIR@" $dir | str replace -a "@SYSTEM@" $system | str replace -a "@MODE@" $mode | save -f $"($dir)/fod.nix"
}

def run-tests [iets: string, work: string] {
  mkdir $"($work)/src/sub"
  "hello\n" | save -f $"($work)/src/a.txt"
  "sub\n" | save -f $"($work)/src/sub/b.txt"
  ^tar -czf $"($work)/src.tar.gz" -C $work src
  let url = $"file://($work)/src.tar.gz"
  mut ok = []

  print "== 1. a file:// URL, flat"
  let ours = (out-of $iets prefetch $url | chomp)
  let theirs = (prefetched $url)
  let hashed = (out-of nix hash file --sri $"($work)/src.tar.gz" | chomp)
  $ok = ($ok | append (verdict ($ours == $theirs and $ours == $hashed) "flat hash"
    $"flat: ours ($ours), prefetch-file ($theirs), nix hash ($hashed)"))

  print "== 2. the same URL, --unpack"
  let ours = (out-of $iets prefetch --unpack $url | chomp)
  let theirs = (prefetched --unpack $url)
  $ok = ($ok | append (verdict ($ours == $theirs) "unpacked NAR hash" $"unpack: ours ($ours), nix ($theirs)"))
  let j = (out-of $iets prefetch --json --unpack $url | from json)
  $ok = ($ok | append (verdict ($j == {hash: $ours, url: $url}) "--json object" $"json: ($j | to json -r)"))

  print "== 3. --hash-type sha512"
  let ours = (out-of $iets prefetch --hash-type sha512 $url | chomp)
  let hashed = (out-of nix hash file --sri --type sha512 $"($work)/src.tar.gz" | chomp)
  $ok = ($ok | append (verdict ($ours == $hashed) "sha512" $"sha512: ours ($ours), nix ($hashed)"))

  print "== 4. the expression form, flat, from a derivation"
  "hello prefetch\n" | save -f $"($work)/msg.txt"
  let sock = (scratch-daemon $"($work)/store" --option builders "" --option max-jobs 2)
  make-fod $work flat (current-system) dep
  let r = (with-env {IETS_CONFIG: $'builder = "unix://($sock)"'} { ^$iets prefetch $"($work)/fod.nix" | complete })
  let hashed = (out-of nix hash file --sri $"($work)/msg.txt" | chomp)
  let lines = ($r.stdout | lines)
  $ok = ($ok | append (verdict ($r.exit_code == 0 and $lines == [$hashed]) "a faked fixed-output build"
    $"expr: exit ($r.exit_code), stdout ($r.stdout | to json -r), want ($hashed), stderr ($r.stderr | chomp)"))

  print "== 5. the expression form, recursive"
  make-fod $work recursive (current-system)
  let r = (with-env {IETS_CONFIG: $'builder = "unix://($sock)"'} { ^$iets prefetch --json $"($work)/fod.nix" | complete })
  let hashed = (out-of nix hash path --sri $"($work)/msg.txt" | chomp)
  let j = (try { $r.stdout | from json } catch { {} })
  $ok = ($ok | append (verdict ($r.exit_code == 0 and $j.hash? == $hashed and ($j.drvPath? | default "" | str ends-with ".drv")) "a recursive fixed-output build"
    $"expr: exit ($r.exit_code), stdout ($r.stdout | to json -r), want ($hashed)"))

  print "== 6. sparse members"
  for form in [[tag, args]; ["gnu", ["--format=gnu"]], ["pax0.0", ["--format=posix", "--sparse-version=0.0"]],
               ["pax0.1", ["--format=posix", "--sparse-version=0.1"]], ["pax1.0", ["--format=posix", "--sparse-version=1.0"]]] {
    rm -rf $"($work)/sp"
    mkdir $"($work)/sp/top"
    ^truncate -s 4194304 $"($work)/sp/top/hole"
    "end\n" | save --append $"($work)/sp/top/hole"
    ^tar -S ...$form.args -cf $"($work)/sp-($form.tag).tar" -C $"($work)/sp" top
    let url = $"file://($work)/sp-($form.tag).tar"
    let ours = (out-of $iets prefetch --unpack $url | chomp)
    let theirs = (prefetched --unpack $url)
    $ok = ($ok | append (verdict ($ours == $theirs) $"sparse ($form.tag)"
      $"sparse ($form.tag): ours ($ours), nix ($theirs)"))
  }
  # the archive is 10 KiB and the file it names is 4 MiB: the cap counts the
  # bytes the expansion writes, not the bytes that arrived
  let capped = (^$iets prefetch --unpack --iets-option fetch-max-size 1048576 $"file://($work)/sp-gnu.tar" | complete)
  $ok = ($ok | append (verdict ($capped.exit_code != 0 and ($capped.stderr | str contains "fetch-max-size")) "sparse fetch-max-size"
    $"cap: exit ($capped.exit_code), stderr ($capped.stderr | chomp)"))

  print "== 7. the command lines that are errors"
  let codes = [
    (exit-code $iets prefetch --bogus)
    (exit-code $iets prefetch --hash-type sha7 $url)
    (exit-code $iets prefetch --unpack -E "1")
    (exit-code $iets prefetch $"file://($work)/nothing.tar.gz")
  ]
  $ok = ($ok | append (verdict ($codes == [2 2 2 1]) "exit codes" $"codes: ($codes | to json -r)"))

  if ($ok | any {|v| not $v }) { fail "prefetch e2e" }
  print "PASS"
}

def main [] {
  require-real-nix nix
  let iets = (iets-bin)
  cd $ROOT
  let work = (isolate prefetch-e2e)
  run-in $work { run-tests $iets $work }
}

#!/usr/bin/env nu
# Differential test: every amber object key ietsp computes for a tree must
# equal what amber-store-core (core-rs) computes with amberfs's parameters.
#
# usage: nu tools/amber-diff/run.nu [PATH ...]
#   CORE_RS  a checkout of core-rs (default: the rev amberfs pins under
#            ~/forge-dev/forge-core). It is copied to ~/tmp/core-rs-scratch,
#            `amber-keys.rs` goes into its examples/, and the example is built
#            offline in its nix dev shell.
#   BIN      ietsp binary directory (default: target/release)
#
# Without PATH arguments the script compares a synthetic tree (built under
# ~/tmp/amber-diff/synth) with `--lstat` metadata, then the nixpkgs source
# tree and one hello and one firefox-unwrapped output with canonical metadata; store
# paths carry Nix's canonical metadata, so both sides must agree there
# without the flag.
use ../../scripts/lib.nu [chomp]

def build-reference [core_rs: string]: nothing -> string {
  let scratch = ($env.HOME | path join "tmp" "core-rs-scratch")
  if not ($scratch | path join "Cargo.toml" | path exists) {
    mkdir $scratch
    ^cp -r $"($core_rs)/." $scratch
    rm -rf ($scratch | path join ".git")
  }
  cp ($env.FILE_PWD | path join "amber-keys.rs") ($scratch | path join "examples" "amber-keys.rs")
  cd $scratch
  ^nix develop --extra-experimental-features "nix-command flakes" -c cargo build --release --offline --example amber-keys
  $scratch | path join "target" "release" "examples" "amber-keys"
}

# Random bytes from /dev/urandom, N of them, into FILE.
def random-file [file: string, n: int] {
  ^head -c $n /dev/urandom | save -f $file
}

def make-synthetic [dir: string] {
  rm -rf $dir
  mkdir $dir
  "" | save -f $"($dir)/empty"
  "x" | save -f $"($dir)/one-byte"
  random-file $"($dir)/exactly-8k" 8192
  random-file $"($dir)/exactly-32k" 32768
  random-file $"($dir)/exactly-64k" 65536
  random-file $"($dir)/random-200k" 204800
  random-file $"($dir)/random-5m" 5242880
  # 40 MiB: more than 512 chunks, so the file index gets a second level
  random-file $"($dir)/random-40m" 41943040
  ^head -c 300000 /dev/zero | save -f $"($dir)/zeros-300k"
  "#!/bin/sh\necho hi\n" | save -f $"($dir)/run.sh"
  ^chmod +x $"($dir)/run.sh"
  ^ln -s one-byte $"($dir)/link"
  ^ln -s ../does/not/exist $"($dir)/dangling"
  mkdir $"($dir)/nested/deeper/deepest"
  "deep\n" | save -f $"($dir)/nested/deeper/deepest/file"
  mkdir $"($dir)/empty-dir"
  # sorting edge cases: bytewise order, upper before lower, punctuation
  for n in ["A-upper" "a-lower" "a" "a.b" "a-b" "a_b" "a b" "aa" "a~" "Z" "z" "é-utf8" "日本語" "ß"] {
    $"content of ($n)\n" | save -f $"($dir)/($n)"
  }
  # 3000 entries: more than 512, so the directory index gets DirNode levels
  mkdir $"($dir)/bigdir"
  for i in 0..2999 {
    let name = ($i | fill -a right -c "0" -w 6)
    ^head -c ($i mod 50) /dev/urandom | save -f $"($dir)/bigdir/e($name)"
  }
}

# Compare one tree; prints a summary line and fails on any difference.
def compare [reference: string, bin: string, path: string, lstat: bool]: nothing -> record {
  let work = ($env.HOME | path join "tmp" "amber-diff")
  mkdir $work
  let ref_out = $"($work)/ref.txt"
  let our_out = $"($work)/ours.txt"
  ^$reference $path | save -f $ref_out
  if $lstat {
    ^$"($bin)/ietsp" amber-keys --lstat $path | save -f $our_out
  } else {
    ^$"($bin)/ietsp" amber-keys $path | save -f $our_out
  }
  let same = ((^cmp -s $ref_out $our_out | complete | get exit_code) == 0)
  let n = (open --raw $ref_out | lines | length) - 1
  let root = (open --raw $ref_out | lines | last)
  if not $same {
    print -e $"DIFF on ($path):"
    print -e (^diff $ref_out $our_out | lines | first 20 | str join "\n")
  }
  {path: $path, keys: $n, root: $root, same: $same}
}

def main [...paths: string] {
  cd ($env.FILE_PWD | path join ".." "..")
  $env.TMPDIR = ($env.TMPDIR? | default ($env.HOME | path join "tmp" "nixtmp"))
  $env.NIX_REMOTE = ($env.NIX_REMOTE? | default "daemon")
  let core_rs = ($env.CORE_RS? | default ($env.HOME | path join "forge-dev" "forge-core" "core-rs-72f38e9e49847704e65c808e16258a9bad27a54f"))
  let bin = ($env.BIN? | default "target/release")
  let reference = (build-reference $core_rs)
  mut results = []
  if ($paths | is-empty) {
    let synth = ($env.HOME | path join "tmp" "amber-diff" "synth")
    make-synthetic $synth
    $results = ($results | append (compare $reference $bin $synth true))
    let store = [
      "/nix/store/ah3pxz4xa52hd6vhbx17vcsci0b8bmbr-source"
      (ls -d /nix/store/*-hello-2.12* | get name | where {|p| not ($p | str ends-with ".drv") and not ($p | str contains "tar.gz") } | first)
      (ls -d /nix/store/*-firefox-unwrapped-1* | get name | where {|p| not ($p | str ends-with ".drv") and not ($p | str contains "symbols") } | first)
    ]
    for p in $store {
      $results = ($results | append (compare $reference $bin $p false))
      # canonical and lstat must agree on a store path: Nix left mtime 1, uid 0, 0444/0555
      $results = ($results | append (compare $reference $bin $p true))
    }
  } else {
    for p in $paths {
      $results = ($results | append (compare $reference $bin $p false))
    }
  }
  print ($results | select path keys same | table --width 120)
  let total = ($results | get keys | math sum)
  let bad = ($results | where same == false | length)
  print $"($total) keys compared, ($bad) trees differ"
  if $bad > 0 { exit 1 }
  print "PASS"
}

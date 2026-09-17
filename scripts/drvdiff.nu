#!/usr/bin/env nu
# Usage: scripts/drvdiff.nu [-I nixpkgs=PATH] (-A ATTR | -E EXPR | FILE)
#   -A ATTR evaluates (import <nixpkgs> {}).ATTR.drvPath; EXPR/FILE must
#   evaluate to a drvPath string.
#
# Evaluates with nix-instantiate (read-write, so the .drv files land in
# /nix/store) and with ietsp (--dump-drvs). On mismatch, walks the Nix
# derivation graph from the root and reports the first derivation whose
# inputs ietsp reproduced exactly but which ietsp itself did not, then diffs
# its ATerm against ietsp's derivation of the same name.
use lib.nu [chomp show diff-head env-cmd]

# Input .drv paths of a Nix derivation: the second ATerm section.
def inputs [drv: string]: nothing -> list<string> {
  open --raw $drv | chomp
    | str replace -r '^Derive\(\[.*\],\[(.*)\],\[[^\]]*\],"[^"]*",.*$' '$1'
    | parse -r '"(/nix/store/[^"]*\.drv)"'
    | each {|r| $r.capture0 }
}

def compare [ietsp: string, nix: list<string>, dump: string, argv: list<string>]: nothing -> int {
  mut args = []
  mut i = 0
  while $i < ($argv | length) {
    if ($argv | get $i) == "-A" {
      $args ++= ["-E" $"\(import <nixpkgs> {}\).($argv | get -o ($i + 1) | default '').drvPath"]
      $i += 2
    } else {
      $args ++= [($argv | get $i)]
      $i += 1
    }
  }

  let n = (^($nix | first) ...($nix | skip 1) --eval --strict --read-write-mode --option pure-eval false ...$args | complete)
  let nixdrv = ($n.stdout | str replace -a '"' '' | chomp)
  let r = (^$ietsp eval --strict --dump-drvs $dump ...$args | complete)
  let ietspdrv = ($r.stdout | str replace -a '"' '' | chomp)
  print $"nix:   ($nixdrv)"
  print $"ietsp: ($ietspdrv)"
  if $nixdrv == "" or $ietspdrv == "" { print "one evaluator failed"; return 1 }
  if $nixdrv == $ietspdrv { print "ok"; return 0 }

  # Descend from the root until every input is reproduced by ietsp.
  mut cur = $nixdrv
  mut seen = []
  loop {
    $seen ++= [$cur]
    mut next = ""
    for i in (inputs $cur) {
      if ($"($dump)/($i | path basename)" | path exists) { continue }
      if $i in $seen { continue }
      $next = $i
      break
    }
    if $next == "" { break }
    $cur = $next
  }

  print $"first differing derivation: ($cur)"
  let name = ($cur | str replace -r '^[^-]*-' '')
  let mine = (ls $dump | get name | path basename | sort | where ($it | str ends-with $"-($name)") | get -o 0 | default "")
  if $mine == "" {
    print $"ietsp produced no derivation named '($name)'"
    return 1
  }
  print $"ietsp counterpart: ($dump)/($mine)"
  let tmp = (mktemp -d --tmpdir drvdiff-diff.XXXXXX)
  open --raw $cur | str replace -a "," "\n" | save -f $"($tmp)/nix"
  open --raw $"($dump)/($mine)" | str replace -a "," "\n" | save -f $"($tmp)/ietsp"
  diff-head $"($tmp)/nix" $"($tmp)/ietsp" ($env.SHOW? | default 60 | into int) | show
  rm -rf $tmp
  1
}

def --wrapped main [...argv: string] {
  cd ($env.FILE_PWD | path join "..")
  let ietsp = ($env.IETSP? | default "./target/release/ietsp")
  let nix = (env-cmd NIX nix-instantiate)
  let dump = (mktemp -d --tmpdir drvdiff.XXXXXX)
  let code = (try { compare $ietsp $nix $dump $argv } catch {|e| print -e $e.rendered; 1 })
  rm -rf $dump
  exit $code
}

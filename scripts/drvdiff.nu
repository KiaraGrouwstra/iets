#!/usr/bin/env nu
# Usage: scripts/drvdiff.nu [-I nixpkgs=PATH] (-A ATTR | -E EXPR | FILE)
#   -A ATTR evaluates (import <nixpkgs> {}).ATTR.drvPath; EXPR/FILE must
#   evaluate to a drvPath string.
#
# Evaluates with nix-instantiate (read-write, so the .drv files land in
# /nix/store) and with iets (--dump-drvs). On mismatch, walks the Nix
# derivation graph from the root and reports the first derivation whose
# inputs iets reproduced exactly but which iets itself did not, then diffs
# its ATerm against iets's derivation of the same name.
use lib.nu [chomp diff-head env-cmd iets-bin require-real-nix show]

# Input .drv paths of a Nix derivation: the second ATerm section.
def inputs [drv: string]: nothing -> list<string> {
  open --raw $drv | chomp
    | str replace -r '^Derive\(\[.*\],\[(.*)\],\[[^\]]*\],"[^"]*",.*$' '$1'
    | parse -r '"(/nix/store/[^"]*\.drv)"'
    | each {|r| $r.capture0 }
}

def compare [iets: string, nix: list<string>, dump: string, argv: list<string>]: nothing -> int {
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
  let r = (^$iets eval --strict --dump-drvs $dump ...$args | complete)
  let ietsdrv = ($r.stdout | str replace -a '"' '' | chomp)
  print $"nix:   ($nixdrv)"
  print $"iets: ($ietsdrv)"
  if $nixdrv == "" or $ietsdrv == "" { print "one evaluator failed"; return 1 }
  if $nixdrv == $ietsdrv { print "ok"; return 0 }

  # Descend from the root until every input is reproduced by iets.
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
    print $"iets produced no derivation named '($name)'"
    return 1
  }
  print $"iets counterpart: ($dump)/($mine)"
  let tmp = (mktemp -d --tmpdir drvdiff-diff.XXXXXX)
  open --raw $cur | str replace -a "," "\n" | save -f $"($tmp)/nix"
  open --raw $"($dump)/($mine)" | str replace -a "," "\n" | save -f $"($tmp)/iets"
  diff-head $"($tmp)/nix" $"($tmp)/iets" ($env.SHOW? | default 60 | into int) | show
  rm -rf $tmp
  1
}

def --wrapped main [...argv: string] {
  cd ($env.FILE_PWD | path join "..")
  let iets = (iets-bin)
  let nix = (env-cmd NIX nix-instantiate)
  require-real-nix ($nix | first)
  let dump = (mktemp -d --tmpdir drvdiff.XXXXXX)
  let code = (try { compare $iets $nix $dump $argv } catch {|e| print -e $e.rendered; 1 })
  rm -rf $dump
  exit $code
}

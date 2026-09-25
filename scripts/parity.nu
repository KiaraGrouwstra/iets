#!/usr/bin/env nu
# Usage: scripts/parity.nu [name...]     (default: every corpus)
# Differential test of the builtins whose answers come from a C++ library:
# builtins.match and builtins.split (libstdc++ std::regex in POSIX extended
# mode) and builtins.fromTOML (toml11 at spec 1.0.0). Runs the real
# nix-instantiate and ietsp over tests/parity/* and requires byte-equal JSON.
#
# It needs a working nix-instantiate, so it is not a flake check; run it by
# hand in the dev shell. NIX=... picks another nix, IETSP=... another binary.
use lib.nu [chomp diff-head env-cmd ietsp-bin show verdict]

# Corpora evaluated in one process each and compared as JSON.
const VALUE_CORPORA = [
  { name: regex, file: "tests/parity/regex.nix", flags: [] }
  { name: toml, file: "tests/parity/toml.nix", flags: ["--extra-experimental-features", "parse-toml-timestamps"] }
]

# Corpora of inputs both sides must reject. builtins.tryEval cannot catch
# these, so each entry gets its own process and only the exit status is
# compared; the messages are allowed to differ (docs/reference/divergences.md).
const REJECT_CORPORA = [
  {
    name: regex-invalid, file: "tests/parity/regex-invalid.nix", flags: []
    calls: ['builtins.match ENTRY "aaa"', 'builtins.split ENTRY "aaa"']
  }
  {
    name: toml-invalid, file: "tests/parity/toml-invalid.nix"
    flags: ["--extra-experimental-features", "parse-toml-timestamps"]
    calls: ['builtins.fromTOML ENTRY']
  }
]

def nix-cmd []: nothing -> list<string> { env-cmd NIX nix-instantiate }

def --wrapped run-nix [...args: string]: nothing -> record {
  let n = (nix-cmd)
  ^($n | first) ...($n | skip 1) --eval --strict --option pure-eval false ...$args | complete
}

def --wrapped run-ietsp [...args: string]: nothing -> record {
  let i = (ietsp-bin)
  ^$i eval --strict ...$args | complete
}

# Compare the whole corpus as one JSON document.
def check-values [c: record]: nothing -> bool {
  let n = (run-nix --json ...$c.flags $c.file)
  if $n.exit_code != 0 {
    return (verdict false $c.name $"($c.name): nix-instantiate failed\n($n.stderr | lines | last 3 | str join (char nl))")
  }
  let i = (run-ietsp --json ...$c.flags $c.file)
  if $i.exit_code != 0 {
    return (verdict false $c.name $"($c.name): ietsp failed\n($i.stderr | lines | last 3 | str join (char nl))")
  }
  let exp = ($n.stdout | chomp)
  let got = ($i.stdout | chomp)
  if $exp == $got { return (verdict true $c.name "") }
  let tmp = (mktemp -d --tmpdir parity.XXXXXX)
  $"($exp)\n" | save -f $"($tmp)/exp"
  $"($got)\n" | save -f $"($tmp)/got"
  print $"FAIL ($c.name): JSON differs"
  diff-head $"($tmp)/exp" $"($tmp)/got" 20 | show
  rm -rf $tmp
  false
}

# Every entry must be rejected by both, one process per entry.
def check-rejects [c: record]: nothing -> bool {
  let imp = "(import ./" + $c.file + ")"
  let len = (run-nix -E ("builtins.length " + $imp) | get stdout | chomp | into int)
  mut ok = true
  for i in 0..<$len {
    let entry = "(builtins.elemAt " + $imp + " " + ($i | into string) + ")"
    for call in $c.calls {
      let e = ($call | str replace "ENTRY" $entry)
      let n = (run-nix ...$c.flags -E $e)
      let i2 = (run-ietsp ...$c.flags -E $e)
      if $n.exit_code == 0 or $i2.exit_code == 0 {
        print $"FAIL ($c.name)[($i)]: nix=($n.exit_code) ietsp=($i2.exit_code) for ($call)"
        $ok = false
      }
    }
  }
  verdict $ok $c.name $"($c.name): see above"
}

def main [...names: string] {
  cd ($env.FILE_PWD | path join "..")
  let want = {|n| ($names | is-empty) or ($n in $names) }
  mut bad = 0
  for c in $VALUE_CORPORA {
    if (do $want $c.name) and not (check-values $c) { $bad += 1 }
  }
  for c in $REJECT_CORPORA {
    if (do $want $c.name) and not (check-rejects $c) { $bad += 1 }
  }
  if $bad > 0 {
    print $"($bad) corpus/corpora differ"
    exit 1
  }
  print "0 diffs"
}

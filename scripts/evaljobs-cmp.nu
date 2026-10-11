#!/usr/bin/env nu
# Usage: scripts/evaljobs-cmp.nu NEJ.jsonl IETS.jsonl [--meta]
# Compares two nix-eval-jobs style outputs attr by attr: job and error
# counts on each side, attrs only one side has, attrs whose drvPath (or
# whole job line minus the error text, with --meta) differs. Order does
# not matter: both files are keyed on attrPath.
use lib.nu [show]

# attr, kind, payload per line of FILE; a job's payload is its drvPath, or
# the whole sorted line with --meta.
def keys [file: string, meta: bool]: nothing -> table {
  let out = if $meta {
    open --raw $file | ^jq -S -c '[(.attrPath|join("\u0001")), (if .error then "error" else "job" end), (if .error then "" else (del(.error, .stats)|tojson) end)] | join("\t")' -r
  } else {
    open --raw $file | ^jq -c '[(.attrPath|join("\u0001")), (if .error then "error" else "job" end), (.drvPath // "")] | join("\t")' -r
  }
  $out | lines | split column -n 3 "\t" attr kind payload | sort-by attr
}

def dots []: string -> string { str replace -a "\u{1}" "." }

def main [a: string, b: string, --meta] {
  let show = ($env.SHOW? | default 10 | into int)
  let ta = (keys $a $meta)
  let tb = (keys $b $meta)
  print $"($a | path basename): ($ta | where kind == 'job' | length) jobs, ($ta | where kind == 'error' | length) errors"
  print $"($b | path basename): ($tb | where kind == 'job' | length) jobs, ($tb | where kind == 'error' | length) errors"
  let tb2 = ($tb | rename attr kind2 payload2)
  let onlya = ($ta | join -l $tb2 attr | where ($it | get -o kind2) == null | get attr)
  let onlyb = ($tb2 | join -l $ta attr | where ($it | get -o kind) == null | get attr)
  print $"only in first: ($onlya | length), only in second: ($onlyb | length)"
  # Same attr on both sides: compare kind and payload.
  let j = ($ta | join $tb2 attr | sort-by attr)
  let kind = ($j | where {|r| $r.kind != $r.kind2 })
  let drv = ($j | where {|r| $r.kind == "job" and $r.kind2 == "job" and $r.payload != $r.payload2 })
  print $"kind differs \(job vs error\): ($kind | length)"
  print $"payload differs \(both jobs\): ($drv | length)"
  if ($kind | length) > 0 {
    print "--- first kind mismatches"
    $kind | first $show | each {|r| $"($r.attr): ($r.kind) vs ($r.kind2)" | dots } | show
  }
  if ($drv | length) > 0 {
    print "--- first payload mismatches"
    $drv | first $show | each {|r| $"($r.attr)\n  ($r.payload)\n  ($r.payload2)" | dots } | show
  }
  if ($onlya | length) > 0 or ($onlyb | length) > 0 {
    print "--- only in first"
    $onlya | first $show | each { dots } | show
    print "--- only in second"
    $onlyb | first $show | each { dots } | show
  }
  if ($kind | length) != 0 or ($drv | length) != 0 or ($onlya | length) != 0 or ($onlyb | length) != 0 { exit 1 }
}

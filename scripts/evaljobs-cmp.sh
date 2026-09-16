#!/usr/bin/env bash
# Usage: scripts/evaljobs-cmp.sh NEJ.jsonl IETSP.jsonl [--meta]
# Compares two nix-eval-jobs style outputs attr by attr: job and error
# counts on each side, attrs only one side has, attrs whose drvPath (or
# whole job line minus the error text, with --meta) differs. Order does
# not matter: both files are keyed on attrPath.
set -uo pipefail
a=$1; b=$2; meta=${3:-}
tmp=$(mktemp -d "${TMPDIR:-/tmp}/evaljobs-cmp.XXXXXX")
trap 'rm -rf "$tmp"' EXIT

key() {
  # attr TAB kind TAB payload; a job's payload is its drvPath, or the whole
  # sorted line with --meta.
  if [ -n "$meta" ]; then
    jq -S -c '[(.attrPath|join("")), (if .error then "error" else "job" end), (if .error then "" else (del(.error)|tojson) end)] | join("\t")' -r "$1"
  else
    jq -c '[(.attrPath|join("")), (if .error then "error" else "job" end), (.drvPath // "")] | join("\t")' -r "$1"
  fi | LC_ALL=C sort
}
key "$a" > "$tmp/a"
key "$b" > "$tmp/b"
count() { awk -F'\t' -v k="$2" '$2==k' "$1" | wc -l; }
echo "$(basename "$a"): $(count "$tmp/a" job) jobs, $(count "$tmp/a" error) errors"
echo "$(basename "$b"): $(count "$tmp/b" job) jobs, $(count "$tmp/b" error) errors"
cut -f1 "$tmp/a" > "$tmp/ka"; cut -f1 "$tmp/b" > "$tmp/kb"
onlya=$(LC_ALL=C comm -23 "$tmp/ka" "$tmp/kb" | wc -l)
onlyb=$(LC_ALL=C comm -13 "$tmp/ka" "$tmp/kb" | wc -l)
echo "only in first: $onlya, only in second: $onlyb"
# Same attr on both sides: compare kind and payload.
LC_ALL=C join -t $'\t' -j 1 "$tmp/a" "$tmp/b" > "$tmp/j"
kind=$(awk -F'\t' '$2!=$4' "$tmp/j" | wc -l)
drv=$(awk -F'\t' '$2=="job" && $4=="job" && $3!=$5' "$tmp/j" | wc -l)
echo "kind differs (job vs error): $kind"
echo "payload differs (both jobs): $drv"
if [ "$kind" -gt 0 ]; then echo "--- first kind mismatches"; awk -F'\t' '$2!=$4 {print $1": "$2" vs "$4}' "$tmp/j" | tr '\001' '.' | head -${SHOW:-10}; fi
if [ "$drv" -gt 0 ]; then echo "--- first payload mismatches"; awk -F'\t' '$2=="job" && $4=="job" && $3!=$5 {print $1"\n  "$3"\n  "$5}' "$tmp/j" | tr '\001' '.' | head -$((3 * ${SHOW:-10})); fi
if [ "$onlya" -gt 0 ] || [ "$onlyb" -gt 0 ]; then
  echo "--- only in first"; LC_ALL=C comm -23 "$tmp/ka" "$tmp/kb" | tr '\001' '.' | head -${SHOW:-10}
  echo "--- only in second"; LC_ALL=C comm -13 "$tmp/ka" "$tmp/kb" | tr '\001' '.' | head -${SHOW:-10}
fi
[ "$kind" -eq 0 ] && [ "$drv" -eq 0 ] && [ "$onlya" -eq 0 ] && [ "$onlyb" -eq 0 ]

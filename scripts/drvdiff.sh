#!/usr/bin/env bash
# Usage: scripts/drvdiff.sh [-I nixpkgs=PATH] (-A ATTR | -E EXPR | FILE)
#   -A ATTR evaluates (import <nixpkgs> {}).ATTR.drvPath; EXPR/FILE must
#   evaluate to a drvPath string.
#
# Evaluates with nix-instantiate (read-write, so the .drv files land in
# /nix/store) and with ietsp (--dump-drvs). On mismatch, walks the Nix
# derivation graph from the root and reports the first derivation whose
# inputs ietsp reproduced exactly but which ietsp itself did not, then diffs
# its ATerm against ietsp's derivation of the same name.
set -uo pipefail
cd "$(dirname "$0")/.."
IETSP=${IETSP:-./target/release/ietsp}
NIX=${NIX:-nix-instantiate}
dump=$(mktemp -d "${TMPDIR:-/tmp}/drvdiff.XXXXXX")
trap 'rm -rf "$dump"' EXIT

args=()
while [ $# -gt 0 ]; do
  case "$1" in
    -A) args+=(-E "(import <nixpkgs> {}).$2.drvPath"); shift 2 ;;
    *) args+=("$1"); shift ;;
  esac
done

nixdrv=$($NIX --eval --strict --read-write-mode --option pure-eval false "${args[@]}" 2>/dev/null | tr -d '"')
ietspdrv=$($IETSP eval --strict --dump-drvs "$dump" "${args[@]}" 2>/dev/null | tr -d '"')
echo "nix:   $nixdrv"
echo "ietsp: $ietspdrv"
if [ -z "$nixdrv" ] || [ -z "$ietspdrv" ]; then echo "one evaluator failed"; exit 1; fi
if [ "$nixdrv" = "$ietspdrv" ]; then echo "ok"; exit 0; fi

# Input .drv paths of a Nix derivation: the second ATerm section.
inputs() {
  sed 's/^Derive(\[.*\],\[\(.*\)\],\[[^]]*\],"[^"]*",.*$/\1/' "$1" \
    | grep -o '"/nix/store/[^"]*\.drv"' | tr -d '"'
}

# Descend from the root until every input is reproduced by ietsp.
cur=$nixdrv
declare -A seen
while :; do
  seen[$cur]=1
  next=""
  for i in $(inputs "$cur"); do
    [ -e "$dump/$(basename "$i")" ] && continue
    [ -n "${seen[$i]:-}" ] && continue
    next=$i
    break
  done
  [ -z "$next" ] && break
  cur=$next
done

echo "first differing derivation: $cur"
name=${cur#*-}
mine=$(ls "$dump" | grep -- "-${name}$" | head -1)
if [ -z "$mine" ]; then
  echo "ietsp produced no derivation named '$name'"
  exit 1
fi
echo "ietsp counterpart: $dump/$mine"
diff <(tr ',' '\n' < "$cur") <(tr ',' '\n' < "$dump/$mine") | head -${SHOW:-60}
exit 1

#!/usr/bin/env bash
# Checks tests/corpus/*.nix against their .exp files (see corpus-update.sh).
set -uo pipefail
cd "$(dirname "$0")/.."
IETSP=${IETSP:-$PWD/target/release/ietsp}
export TEST_VAR=foo
pass=0; fail=0; failed=()
for f in tests/corpus/*.nix; do
  n=${f%.nix}
  flags=()
  [ -f "$n.flags" ] && read -r -a flags < "$n.flags"
  out=$("$IETSP" eval --strict "${flags[@]}" "$f" 2>/dev/null)
  if [ "$out" == "$(cat "$n.exp")" ]; then pass=$((pass+1)); else fail=$((fail+1)); failed+=("$(basename "$n")"); fi
done
echo "pass=$pass fail=$fail"
printf '%s\n' "${failed[@]}"
[ "$fail" -eq 0 ]

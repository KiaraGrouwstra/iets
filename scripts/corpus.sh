#!/usr/bin/env bash
# Runs tests/corpus/*.nix against ietsp and compares with the .exp captured
# by scripts/corpus-update.sh. Runs from tests/ so paths print as /pwd/....
set -uo pipefail
cd "$(dirname "$0")/.."
IETSP=${IETSP:-$PWD/target/release/ietsp}
export HOME=/fake-home
export TEST_VAR=foo
cd tests
pass=0; fail=0; failed=()
for f in corpus/*.nix; do
  n=${f%.nix}
  [ -f "$n.exp" ] || continue
  flags=()
  [ -f "$n.flags" ] && read -r -a flags < "$n.flags"
  out=$("$IETSP" eval --strict "${flags[@]}" "$f" 2>/dev/null | sed "s|$PWD|/pwd|g")
  if [ "$out" == "$(cat "$n.exp")" ]; then pass=$((pass+1)); else fail=$((fail+1)); failed+=("$(basename "$n")"); fi
done
echo "pass=$pass fail=$fail"
printf '%s\n' "${failed[@]}"
[ "$fail" -eq 0 ]

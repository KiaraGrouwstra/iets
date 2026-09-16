#!/usr/bin/env bash
# Runs tests/lang/eval-okay-*.nix and eval-fail-*.nix against ietsp.
# Prints pass/fail counts and the names of failures.
set -uo pipefail
cd "$(dirname "$0")/.."
IETSP=${IETSP:-./target/release/ietsp}
pass=0; fail=0; failed=()
for f in tests/lang/eval-okay-*.nix; do
  n=${f%.nix}
  flags=()
  [ -f "$n.flags" ] && read -r -a flags < "$n.flags"
  if [ -f "$n.exp" ]; then
    out=$(cd tests/lang && ../../$IETSP eval --strict "${flags[@]}" "$(basename "$f")" 2>/dev/null)
    if [ "$out" == "$(cat "$n.exp")" ]; then pass=$((pass+1)); else fail=$((fail+1)); failed+=("$(basename "$n")"); fi
  fi
done
for f in tests/lang/eval-fail-*.nix; do
  n=${f%.nix}
  if (cd tests/lang && ../../$IETSP eval --strict "$(basename "$f")" >/dev/null 2>&1); then
    fail=$((fail+1)); failed+=("$(basename "$n") (should fail)")
  else pass=$((pass+1)); fi
done
echo "pass=$pass fail=$fail"
printf '%s\n' "${failed[@]}" | head -${SHOW:-40}

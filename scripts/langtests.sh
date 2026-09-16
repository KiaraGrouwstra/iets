#!/usr/bin/env bash
# Runs tests/lang/eval-okay-*.nix and eval-fail-*.nix against ietsp, from
# the tests/ directory like Nix's own harness (paths print as /pwd/lang/...).
set -uo pipefail
cd "$(dirname "$0")/.."
IETSP=${IETSP:-$PWD/target/release/ietsp}
export HOME=/fake-home
export TEST_VAR=foo # for eval-okay-getenv.nix
cd tests
pass=0; fail=0; failed=()
for f in lang/eval-okay-*.nix; do
  n=${f%.nix}
  flags=()
  [ -f "$n.flags" ] && read -r -a flags < "$n.flags"
  if [ -f "$n.exp" ]; then
    out=$("$IETSP" eval --strict "${flags[@]}" "$f" 2>/dev/null | sed "s|$PWD|/pwd|g")
    if [ "$out" == "$(cat "$n.exp")" ]; then pass=$((pass+1)); else fail=$((fail+1)); failed+=("$(basename "$n")"); fi
  fi
done
for f in lang/eval-fail-*.nix; do
  n=${f%.nix}
  flags=()
  [ -f "$n.flags" ] && read -r -a flags < "$n.flags"
  "$IETSP" eval --strict "${flags[@]}" "$f" >/dev/null 2>&1
  rc=$?
  if [ $rc -eq 0 ]; then fail=$((fail+1)); failed+=("$(basename "$n") (should fail)")
  elif [ $rc -ge 128 ]; then fail=$((fail+1)); failed+=("$(basename "$n") (crashed rc=$rc)")
  else pass=$((pass+1)); fi
done
echo "pass=$pass fail=$fail"
printf '%s\n' "${failed[@]}" | head -${SHOW:-40}

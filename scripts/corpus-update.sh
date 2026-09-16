#!/usr/bin/env bash
# Regenerates tests/corpus/*.exp from nix-instantiate --eval --strict.
# A sibling NAME.flags file holds extra flags for both evaluators.
set -euo pipefail
cd "$(dirname "$0")/.."
NIX=${NIX:-nix-instantiate}
export TEST_VAR=foo
for f in tests/corpus/*.nix; do
  n=${f%.nix}
  flags=()
  [ -f "$n.flags" ] && read -r -a flags < "$n.flags"
  "$NIX" --eval --strict "${flags[@]}" "$f" > "$n.exp"
done
echo "updated $(ls tests/corpus/*.exp | wc -l) files"

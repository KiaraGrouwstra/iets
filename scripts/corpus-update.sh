#!/usr/bin/env bash
# Regenerates tests/corpus/*.exp from nix-instantiate. Pass test names to
# update a subset. Runs from tests/ so paths print as /pwd/....
set -uo pipefail
cd "$(dirname "$0")/.."
NIX=${NIX:-nix-instantiate}
export HOME=/fake-home
cd tests
if [ $# -gt 0 ]; then files=(); for t in "$@"; do files+=("corpus/${t%.nix}.nix"); done
else files=(corpus/*.nix); fi
for f in "${files[@]}"; do
  n=${f%.nix}
  flags=()
  [ -f "$n.flags" ] && read -r -a flags < "$n.flags"
  if $NIX --eval --strict --option pure-eval false "${flags[@]}" "$f" 2>/dev/null | sed "s|$PWD|/pwd|g" > "$n.exp"; then
    echo "updated $n.exp"
  else
    rm -f "$n.exp"; echo "FAILED $f (no .exp written)"; $NIX --eval --strict "${flags[@]}" "$f" 2>&1 | tail -3
  fi
done

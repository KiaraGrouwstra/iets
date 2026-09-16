#!/usr/bin/env bash
# Usage: scripts/difftest.sh [-I nixpkgs=PATH] (-E EXPR | FILE)
# Runs nix-instantiate and ietsp on the same input and diffs stdout.
set -uo pipefail
cd "$(dirname "$0")/.."
IETSP=${IETSP:-./target/release/ietsp}
NIX=${NIX:-nix-instantiate}
exp=$($NIX --eval --strict --option pure-eval false "$@" 2>/tmp/difftest.nix.err)
nrc=$?
got=$($IETSP eval --strict "$@" 2>/tmp/difftest.ietsp.err)
irc=$?
if [ "$nrc" -ne 0 ] && [ "$irc" -ne 0 ]; then echo "both failed"; exit 0; fi
if [ "$nrc" -ne "$irc" ]; then
  echo "exit code differs: nix=$nrc ietsp=$irc"
  echo "--- nix stderr"; tail -5 /tmp/difftest.nix.err
  echo "--- ietsp stderr"; tail -5 /tmp/difftest.ietsp.err
  exit 1
fi
if [ "$exp" != "$got" ]; then
  echo "output differs"
  diff <(echo "$exp") <(echo "$got") | head -40
  exit 1
fi
echo "ok"

#!/usr/bin/env bash
# Runs nix-eval-jobs and ietsp eval-jobs with every supported flag
# combination on the small jobsets in tests/evaljobs/ and diffs the sorted
# output: `jq -S` per line, sorted by attrPath, error text replaced by
# `true`. Also compares the exit codes and, with --gc-roots-dir, the roots.
#
# usage: scripts/evaljobs-flags.sh          # NEJ=~/tmp/nej/bin/nix-eval-jobs
set -uo pipefail
cd "$(dirname "$0")/../tests/evaljobs"
IETSP=${IETSP:-$PWD/../../target/release/ietsp}
NEJ=${NEJ:-$HOME/tmp/nej/bin/nix-eval-jobs}
NIXPKGS=${NIXPKGS:-/nix/store/ah3pxz4xa52hd6vhbx17vcsci0b8bmbr-source}
export NIX_REMOTE=${NIX_REMOTE:-daemon}
work=$(mktemp -d "${TMPDIR:-/tmp}/ietsp-evaljobs-flags.XXXXXX")
trap 'rm -rf "$work"' EXIT
pass=0; fail=0

norm() {
  jq -S -c 'del(.error) + (if has("error") then {error: true} else {} end)' | LC_ALL=C sort
}

# check NAME [ARGS...]: both tools get ARGS; GCROOTS in ARGS is replaced by
# a directory per tool whose entries are compared too.
check() {
  local name=$1; shift
  local nargs=() iargs=() a
  for a in "$@"; do
    nargs+=("${a//GCROOTS/$work/$name.nej-gc}"); iargs+=("${a//GCROOTS/$work/$name.ietsp-gc}")
  done
  mkdir -p "$work/$name.nej-gc" "$work/$name.ietsp-gc"
  "$NEJ" --extra-experimental-features 'nix-command flakes' "${nargs[@]}" <"${STDIN:-/dev/null}" >"$work/$name.nej" 2>"$work/$name.nej.err"; local nrc=$?
  "$IETSP" eval-jobs "${iargs[@]}" <"${STDIN:-/dev/null}" >"$work/$name.ietsp" 2>"$work/$name.ietsp.err"; local irc=$?
  norm <"$work/$name.nej" >"$work/$name.nej.norm"
  norm <"$work/$name.ietsp" >"$work/$name.ietsp.norm"
  (cd "$work/$name.nej-gc" && ls -1 | LC_ALL=C sort) >"$work/$name.nej.roots"
  (cd "$work/$name.ietsp-gc" && ls -1 | LC_ALL=C sort) >"$work/$name.ietsp.roots"
  if [ "$nrc" -eq "$irc" ] && cmp -s "$work/$name.nej.norm" "$work/$name.ietsp.norm" && cmp -s "$work/$name.nej.roots" "$work/$name.ietsp.roots"; then
    pass=$((pass+1)); echo "ok   $name ($(wc -l <"$work/$name.nej.norm") lines, exit $nrc)"
  else
    fail=$((fail+1)); echo "FAIL $name (exit nej=$nrc ietsp=$irc)"
    diff "$work/$name.nej.norm" "$work/$name.ietsp.norm" | head -${SHOW:-6}
    diff "$work/$name.nej.roots" "$work/$name.ietsp.roots" | head -4
    tail -3 "$work/$name.ietsp.err"
  fi
}

# chunks NAME [ARGS...]: ietsp in two chunks against one nix-eval-jobs run.
chunks() {
  local name=$1; shift
  "$NEJ" --extra-experimental-features 'nix-command flakes' "$@" >"$work/$name.nej" 2>/dev/null; local nrc=$?
  { "$IETSP" eval-jobs --chunks 2 --chunk 0 "$@" && "$IETSP" eval-jobs --chunks 2 --chunk 1 "$@"; } >"$work/$name.ietsp" 2>/dev/null; local irc=$?
  norm <"$work/$name.nej" >"$work/$name.nej.norm"
  norm <"$work/$name.ietsp" >"$work/$name.ietsp.norm"
  if [ "$nrc" -eq "$irc" ] && cmp -s "$work/$name.nej.norm" "$work/$name.ietsp.norm"; then
    pass=$((pass+1)); echo "ok   $name ($(wc -l <"$work/$name.nej.norm") lines)"
  else
    fail=$((fail+1)); echo "FAIL $name (exit nej=$nrc ietsp=$irc)"; diff "$work/$name.nej.norm" "$work/$name.ietsp.norm" | head -6
  fi
}

# expect_fail NAME [ARGS...]: both tools must exit non-zero.
expect_fail() {
  local name=$1; shift
  "$NEJ" --extra-experimental-features 'nix-command flakes' "$@" >/dev/null 2>&1; local nrc=$?
  "$IETSP" eval-jobs "$@" >/dev/null 2>&1; local irc=$?
  if [ "$nrc" -ne 0 ] && [ "$irc" -ne 0 ]; then pass=$((pass+1)); echo "ok   $name (both fail)"; else fail=$((fail+1)); echo "FAIL $name (exit nej=$nrc ietsp=$irc)"; fi
}

check basic ci.nix
check meta --meta ci.nix
check force-recurse --meta --force-recurse ci.nix
check no-instantiate --meta --no-instantiate --gc-roots-dir GCROOTS ci.nix
check input-drvs --show-input-drvs ci.nix
check gc-roots --gc-roots-dir GCROOTS --meta ci.nix
check apply --apply 'drv: { the-name = drv.name; version = drv.version or null; outs = drv.outputs or [ "out" ]; }' ci.nix
check apply-error --apply 'drv: drv.name' ci.nix
check select --select 'jobs: { inherit (jobs) builtJob recurse; }' --meta ci.nix
check expr --expr 'import ./ci.nix { system = "aarch64-linux"; }' --meta
check expr-E -E '(import ./ci.nix { }).recurse' --meta
check argstr --argstr system aarch64-linux ci.nix
check arg --arg system '"i686-linux"' ci.nix
check arg-from-file --arg-from-file system system.txt ci.nix
STDIN=system.txt check arg-from-stdin --arg-from-stdin system ci.nix
check include --include nixpkgs="$NIXPKGS" --expr '{ inherit ((import <nixpkgs> { }).hello) drvPath; }' --no-instantiate
check option --option allow-import-from-derivation false --show-trace --quiet --max-jobs 1 --log-format raw ci.nix
check impure --impure --expr '{ x = derivation { name = "x-${builtins.currentSystem}"; system = builtins.currentSystem; builder = ":"; }; }'
check flake --flake ./flake#hydraJobs --meta --gc-roots-dir GCROOTS
check flake-whole --flake ./flake
check flake-select --flake ./flake --select 'flake: flake.outputs.hydraJobs' --meta
check flake-select-fragment --flake ./flake#hydraJobs --select 'outputs: { inherit (outputs) builtJob; }'
check flake-legacy --flake ./flake#legacyPackages.x86_64-linux.success --meta
check constituents --constituents --gc-roots-dir GCROOTS --meta aggregates.nix
check constituents-flake --constituents --flake ./flake#legacyPackages.x86_64-linux --meta
check constituents-select --constituents --select 'j: { inherit (j) success; }' aggregates.nix
check cache-status --check-cache-status --meta -I nixpkgs="$NIXPKGS" cache.nix
check cache-status-constituents --check-cache-status --constituents --gc-roots-dir GCROOTS aggregates.nix
check restart --max-memory-size 1 --workers 2 --meta --gc-roots-dir GCROOTS ci.nix
check restart-1 --max-memory-size 1 --workers 1 --meta ci.nix
check restart-big --max-memory-size 100000 --workers 3 --meta --show-input-drvs ci.nix
check restart-constituents --max-memory-size 1 --workers 2 --constituents --check-cache-status --gc-roots-dir GCROOTS aggregates.nix
check restart-flake --max-memory-size 1 --workers 2 --flake ./flake#hydraJobs --apply 'd: { n = d.name; }'
check restart-no-instantiate --max-memory-size 1 --workers 2 --no-instantiate --meta ci.nix
chunks chunks --meta ci.nix
expect_fail no-instantiate-constituents --no-instantiate --constituents aggregates.nix
expect_fail no-instantiate-cache --no-instantiate --check-cache-status ci.nix
expect_fail no-instantiate-input-drvs --no-instantiate --show-input-drvs ci.nix
expect_fail select-not-attrs --select 'x: "str"' ci.nix
expect_fail no-expr
echo "passed $pass, failed $fail"
[ "$fail" -eq 0 ]

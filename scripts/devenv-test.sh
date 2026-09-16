#!/usr/bin/env bash
# `ietsp print-dev-env` against `nix print-dev-env --option eval-cache false`:
#  a. byte-identical bash for /home/zimbatm/home and /home/zimbatm/iets-prime;
#  b. --json identical modulo key order (jq -S);
#  c. warm run (cache hit) time, printed;
#  d. --gc-root creates the symlink and nix-store --gc --print-roots lists it;
#  e. a garbage-collected env output is rebuilt on the next (cached) run;
#  f. a shell whose package is absent from /nix/store gets it substituted;
#  g. `#attr` selects a shell.
# e-g use a scratch flake on the nixpkgs iets-prime pins (lock written by hand,
# so nothing is fetched: both evaluators find the tree in the store by narHash).
#
# usage: scripts/devenv-test.sh            # uses target/release
#        IETSP=target/debug/ietsp scripts/devenv-test.sh
set -uo pipefail
cd "$(dirname "$0")/.."
IETSP=${IETSP:-$PWD/target/release/ietsp}
NIX=(nix --extra-experimental-features "nix-command flakes")
export NIX_REMOTE=${NIX_REMOTE:-daemon}
work=$(mktemp -d "${TMPDIR:-/tmp}/ietsp-devenv.XXXXXX")
trap 'rm -rf "$work"' EXIT
export IETSP_CACHE_DIR=$work/cache
pass=0; fail=0
ok() { pass=$((pass+1)); echo "ok   $1"; }
bad() { fail=$((fail+1)); echo "FAIL $1"; shift; for l in "$@"; do echo "  $l"; done; }
ms() { echo $(( ($2 - $1) / 1000000 )); }

# nix_env FLAKE FILE [ARGS]: nix print-dev-env from inside FLAKE (the script names <cwd>/outputs).
nix_env() { (cd "$1" && "${NIX[@]}" print-dev-env --option eval-cache false "${@:3}" "$1" > "$2" 2>"$work/nix.err"); }
ietsp_env() { (cd "$1" && "$IETSP" print-dev-env "${@:3}" "$1" > "$2" 2>"$work/ietsp.err"); }

for flake in /home/zimbatm/home /home/zimbatm/iets-prime; do
  name=$(basename "$flake")
  nix_env "$flake" "$work/nix.sh" || bad "nix print-dev-env $name" "$(tail -3 "$work/nix.err")"
  ietsp_env "$flake" "$work/ietsp.sh" --no-cache || bad "ietsp print-dev-env $name" "$(tail -3 "$work/ietsp.err")"
  if cmp -s "$work/nix.sh" "$work/ietsp.sh"; then
    ok "a. bash identical: $name ($(wc -c < "$work/nix.sh") bytes)"
  else
    bad "a. bash differs: $name" "$(diff "$work/nix.sh" "$work/ietsp.sh" | head -8)"
  fi

  nix_env "$flake" "$work/nix.json" --json
  ietsp_env "$flake" "$work/ietsp.json" --no-cache --json
  if cmp -s <(jq -S . "$work/nix.json") <(jq -S . "$work/ietsp.json"); then
    ok "b. json identical: $name"
  else
    bad "b. json differs: $name" "$(diff <(jq -S . "$work/nix.json") <(jq -S . "$work/ietsp.json") | head -8)"
  fi

  ietsp_env "$flake" "$work/miss.sh"          # cache miss, writes the entry
  t0=$(date +%s%N); ietsp_env "$flake" "$work/hit.sh"; t1=$(date +%s%N)
  if cmp -s "$work/nix.sh" "$work/hit.sh"; then
    ok "c. warm run identical: $name, $(ms "$t0" "$t1") ms"
  else
    bad "c. warm run differs: $name" "$(diff "$work/nix.sh" "$work/hit.sh" | head -5)"
  fi
done

# ---- scratch flake -----------------------------------------------------------
locked=$(jq -c '.nodes.nixpkgs.locked' /home/zimbatm/iets-prime/flake.lock)
rev=$(jq -r .rev <<<"$locked")
absent=
for p in cowsay hello sl figlet lolcat; do
  out=$("${NIX[@]}" eval --raw --impure --expr "(builtins.getFlake \"/home/zimbatm/iets-prime\").inputs.nixpkgs.legacyPackages.x86_64-linux.$p.outPath" 2>/dev/null) || continue
  if [ ! -e "$out" ]; then absent=$p; absent_out=$out; break; fi
done
[ -n "$absent" ] || { absent=hello; absent_out=; echo "note: every candidate package is present; f. cannot show a substitution"; }
mkdir -p "$work/scratch"
cat > "$work/scratch/flake.nix" <<NIX
{
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/$rev";
  outputs = { self, nixpkgs }:
    let pkgs = nixpkgs.legacyPackages.x86_64-linux; in {
      devShells.x86_64-linux.default = pkgs.mkShell { packages = [ pkgs.$absent ]; };
      devShells.x86_64-linux.other = pkgs.mkShell { FOO = "bar"; };
    };
}
NIX
jq -n --argjson locked "$locked" --arg rev "$rev" '{
  nodes: {
    nixpkgs: { locked: $locked, original: { owner: "NixOS", repo: "nixpkgs", rev: $rev, type: "github" } },
    root: { inputs: { nixpkgs: "nixpkgs" } }
  }, root: "root", version: 7 }' > "$work/scratch/flake.lock"
scratch=$work/scratch
root=$work/roots/env

ietsp_env "$scratch" "$work/scratch.sh" --gc-root "$root" || bad "ietsp print-dev-env scratch" "$(tail -5 "$work/ietsp.err")"
outpath=$(readlink "$root" || true)
case $outpath in
  /nix/store/*-nix-shell-env) ok "d. gc root symlink: $root -> $outpath" ;;
  *) bad "d. gc root symlink missing or wrong: '$outpath'" ;;
esac
if nix-store --gc --print-roots 2>/dev/null | grep -qF "$root"; then
  ok "d. nix-store --gc --print-roots lists the root"
else
  bad "d. root not registered with the daemon"
fi
if [ -n "$absent_out" ]; then
  if [ -e "$absent_out" ]; then ok "f. $absent was substituted: $absent_out"; else bad "f. $absent_out is still absent"; fi
fi
nix_env "$scratch" "$work/scratch-nix.sh"
if cmp -s "$work/scratch-nix.sh" "$work/scratch.sh"; then ok "a. bash identical: scratch"; else bad "a. bash differs: scratch" "$(diff "$work/scratch-nix.sh" "$work/scratch.sh" | head -8)"; fi

# e. simulate a GC of the env output: drop the root, delete the path, rerun on the warm cache.
rm -f "$root"
if [ -n "$outpath" ] && nix-store --delete "$outpath" >/dev/null 2>&1 && [ ! -e "$outpath" ]; then
  t0=$(date +%s%N); ietsp_env "$scratch" "$work/scratch2.sh" --gc-root "$root" --timings; t1=$(date +%s%N)
  if [ -e "$outpath" ] && [ "$(readlink "$root")" = "$outpath" ] && cmp -s "$work/scratch.sh" "$work/scratch2.sh"; then
    ok "e. deleted env output rebuilt on the next run, $(ms "$t0" "$t1") ms ($(grep -c 'cache: env output' "$work/ietsp.err") fallback notice)"
  else
    bad "e. env output not rebuilt" "$(tail -5 "$work/ietsp.err")"
  fi
else
  bad "e. could not delete $outpath (another root holds it?)"
fi

# g. #attr
ietsp_env "$scratch" "$work/other.sh" "" 2>/dev/null
(cd "$scratch" && "$IETSP" print-dev-env "$scratch#other" > "$work/other.sh" 2>"$work/ietsp.err")
(cd "$scratch" && "${NIX[@]}" print-dev-env --option eval-cache false "$scratch#other" > "$work/other-nix.sh" 2>/dev/null)
if grep -q "^FOO='bar'$" "$work/other.sh" && cmp -s "$work/other.sh" "$work/other-nix.sh"; then
  ok "g. #other selects devShells.x86_64-linux.other, identical to nix"
else
  bad "g. #other" "$(diff "$work/other-nix.sh" "$work/other.sh" | head -5)" "$(tail -3 "$work/ietsp.err")"
fi
if (cd "$scratch" && "$IETSP" print-dev-env "$scratch#missing" >/dev/null 2>"$work/ietsp.err"); then
  bad "g. #missing should fail"
else
  grep -q "does not provide attribute 'devShells.x86_64-linux.missing'" "$work/ietsp.err" && ok "g. #missing reports the attribute paths tried" || bad "g. #missing error text" "$(tail -2 "$work/ietsp.err")"
fi

echo "$pass passed, $fail failed"
[ "$fail" -eq 0 ]

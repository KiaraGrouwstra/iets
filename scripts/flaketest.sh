#!/usr/bin/env bash
# End-to-end flake evaluation: `ietsp eval --raw FLAKE#attr` must print what
# `nix eval --raw --option eval-cache false FLAKE#attr` prints, on real
# flakes and on a scratch git flake with a `path:` input, a `follows`, a
# clean tree and a dirty tree.
set -uo pipefail
cd "$(dirname "$0")/.."
IETSP=${IETSP:-$PWD/target/release/ietsp}
NIX=(nix --extra-experimental-features "nix-command flakes")
work=$(mktemp -d "${TMPDIR:-/tmp}/ietsp-flaketest.XXXXXX")
trap 'rm -rf "$work"' EXIT
export IETSP_CACHE_DIR=$work/cache
pass=0; fail=0

# check NAME REF [VAR=VALUE...]: both evaluators must agree and succeed.
check() {
  local name=$1 ref=$2; shift 2
  local exp got
  exp=$("${NIX[@]}" eval --raw --option eval-cache false "$ref" 2>"$work/nix.err")
  got=$(env "$@" "$IETSP" eval --raw --no-cache "$ref" 2>"$work/ietsp.err")
  if [ -n "$exp" ] && [ "$exp" == "$got" ]; then
    pass=$((pass+1)); echo "ok   $name"
  else
    fail=$((fail+1)); echo "FAIL $name"
    echo "  nix:   $exp"; grep -v "is dirty" "$work/nix.err" | tail -3
    echo "  ietsp: $got"; tail -3 "$work/ietsp.err"
  fi
}

home=/home/zimbatm/home
host=$("${NIX[@]}" eval --json "$home#nixosConfigurations" --apply builtins.attrNames 2>/dev/null | jq -r '.[0]')
check "home devShell" "$home#devShells.x86_64-linux.default.drvPath"
check "home devShell, IETSP_WORKERS=8" "$home#devShells.x86_64-linux.default.drvPath" IETSP_WORKERS=8
check "home nixosConfigurations.$host toplevel" "$home#nixosConfigurations.$host.config.system.build.toplevel.drvPath"
check "hairesis muster" "/home/zimbatm/hairesis#packages.x86_64-linux.muster.drvPath"
check "iets-prime default" "/home/zimbatm/iets-prime#packages.x86_64-linux.default.drvPath"

# ---- scratch flake ---------------------------------------------------------
git=(git -c user.name=ietsp -c user.email=ietsp@example.com -c init.defaultBranch=main)
mkdir -p "$work/sub" "$work/dep" "$work/repo"
cat > "$work/sub/flake.nix" <<'NIX'
{ outputs = { self }: { value = "sub-" + self.narHash; }; }
NIX
cat > "$work/dep/flake.nix" <<NIX
{
  inputs.sub.url = "path:$work/sub";
  outputs = { self, sub }: { value = "dep-" + sub.value; inherit sub; };
}
NIX
cat > "$work/repo/flake.nix" <<NIX
{
  inputs.sub.url = "path:$work/sub";
  inputs.dep.url = "path:$work/dep";
  inputs.dep.inputs.sub.follows = "sub";
  outputs = { self, dep, sub }: {
    packages.x86_64-linux.default = derivation {
      name = "scratch";
      system = "x86_64-linux";
      builder = "/bin/sh";
      args = [ "-c" "echo \${self.narHash} \${self.rev or self.dirtyRev or "norev"} \${self.shortRev or self.dirtyShortRev or "norev"} \${toString self.lastModified} \${self.lastModifiedDate} \${toString (self.revCount or 0)} \${dep.value} \${dep.sub.value} \${dep.outPath} \${sub.outPath} \${self}/file \${builtins.readFile (self + "/file")} > \$out" ];
    };
  };
}
NIX
echo one > "$work/repo/file"
mkdir -p "$work/repo/nested"
echo two > "$work/repo/nested/tracked"
"${git[@]}" -C "$work/repo" init -q
"${git[@]}" -C "$work/repo" add -A
"${git[@]}" -C "$work/repo" commit -q -m init
( cd "$work/repo" && "${NIX[@]}" flake lock 2>/dev/null )
"${git[@]}" -C "$work/repo" add flake.lock
"${git[@]}" -C "$work/repo" commit -q -m lock
scratch="$work/repo#packages.x86_64-linux.default"

check "scratch: clean tree" "$scratch.drvPath"
check "scratch: --raw on a derivation prints its outPath" "$scratch"
echo untracked > "$work/repo/untracked"
check "scratch: untracked file is ignored" "$scratch.drvPath"
echo three > "$work/repo/file"
check "scratch: dirty tree (modified tracked file)" "$scratch.drvPath"
rm "$work/repo/nested/tracked"
check "scratch: dirty tree (deleted tracked file)" "$scratch.drvPath"
echo four > "$work/repo/added"; "${git[@]}" -C "$work/repo" add added
check "scratch: dirty tree (staged new file)" "$scratch.drvPath"
"${git[@]}" -C "$work/repo" commit -q -am next
check "scratch: clean again after commit" "$scratch.drvPath"
check "scratch: subdirectory flake ref" "$work/repo/.#packages.x86_64-linux.default.drvPath"
check "scratch: path: ref copies the whole tree" "path:$work/repo#packages.x86_64-linux.default.drvPath"

exp=$("${NIX[@]}" eval --json --option eval-cache false "$scratch.drvPath" 2>/dev/null)
got=$("$IETSP" eval --json --no-cache "$scratch.drvPath" 2>/dev/null)
if [ -n "$exp" ] && [ "$exp" == "$got" ]; then pass=$((pass+1)); echo "ok   scratch: --json"; else fail=$((fail+1)); echo "FAIL scratch: --json ($exp vs $got)"; fi

# tracing cache: hit after a miss, miss after the tree changes
"$IETSP" eval --raw "$scratch.drvPath" >/dev/null 2>&1
st=$("$IETSP" eval --raw --timings "$scratch.drvPath" 2>&1 >/dev/null | grep '^cache:')
if [[ $st == *"cache: hit"* ]]; then pass=$((pass+1)); echo "ok   cache: second run hits"; else fail=$((fail+1)); echo "FAIL cache: second run hits ($st)"; fi
echo five > "$work/repo/file"
st=$("$IETSP" eval --raw --timings "$scratch.drvPath" 2>&1 >/dev/null | grep '^cache:' | head -1)
if [[ $st == *"miss ("* ]]; then pass=$((pass+1)); echo "ok   cache: editing a tracked file misses"; else fail=$((fail+1)); echo "FAIL cache: editing a tracked file misses ($st)"; fi
"${git[@]}" -C "$work/repo" commit -q -am five
st=$("$IETSP" eval --raw --timings "$scratch.drvPath" 2>&1 >/dev/null | grep '^cache:' | head -1)
if [[ $st == *"miss ("*"git"* ]]; then pass=$((pass+1)); echo "ok   cache: a commit misses on a git command"; else fail=$((fail+1)); echo "FAIL cache: a commit misses on a git command ($st)"; fi

rm "$work/repo/flake.lock"
err=$("$IETSP" eval --raw --no-cache "$scratch.drvPath" 2>&1 >/dev/null)
if [[ $err == *"flake.lock missing or incomplete, run nix flake lock"* ]]; then pass=$((pass+1)); echo "ok   missing flake.lock is an error"; else fail=$((fail+1)); echo "FAIL missing flake.lock is an error ($err)"; fi

echo "pass=$pass fail=$fail"
[ "$fail" -eq 0 ]

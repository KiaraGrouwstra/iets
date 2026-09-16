#!/usr/bin/env bash
# End-to-end test of the remote builder: start ietsp-builderd on localhost,
# build a small derivation with a local source tree (exercises Put of
# chunks, a manifest, and a drv), then build hello from the pinned nixpkgs
# (exercises Put of a 600-drv closure, then a warm run with nothing missing).
# Cases 1-3 go through the legacy `.drv` path; cases 4-6 evaluate in memory
# and never read the closure from the local store.
#
# usage: scripts/remote-e2e.sh            # uses target/release
#        BIN=target/debug scripts/remote-e2e.sh
set -euo pipefail
cd "$(dirname "$0")/.."

BIN=${BIN:-target/release}
NIXPKGS=${NIXPKGS:-/nix/store/ah3pxz4xa52hd6vhbx17vcsci0b8bmbr-source}
export NIX_REMOTE=${NIX_REMOTE:-daemon}
export TMPDIR=${TMPDIR:-/tmp}

work=$(mktemp -d "$TMPDIR/ietsp-e2e.XXXXXX")
srv=
cleanup() {
    [ -n "$srv" ] && kill "$srv" 2>/dev/null || true
    rm -rf "$work"
}
trap cleanup EXIT

port=$(( 20000 + RANDOM % 20000 ))
url=http://127.0.0.1:$port
"$BIN/ietsp-builderd" --listen "127.0.0.1:$port" --store-dir "$work/builder" 2>"$work/server.log" &
srv=$!
for _ in $(seq 100); do
    grep -q listening "$work/server.log" 2>/dev/null && break
    sleep 0.05
done
grep -q listening "$work/server.log" || { echo "server did not start"; cat "$work/server.log"; exit 1; }

fail() { echo "FAIL: $*" >&2; echo "--- server log:" >&2; cat "$work/server.log" >&2; exit 1; }

echo "== 1. custom derivation with a source tree"
mkdir -p "$work/src/sub"
echo "hello $RANDOM $$ $(date +%s%N)" > "$work/src/a"
printf '#!/bin/sh\necho hi\n' > "$work/src/run.sh"; chmod +x "$work/src/run.sh"
echo zzz > "$work/src/sub/b"
ln -s a "$work/src/link"
system=$(nix-instantiate --eval -E builtins.currentSystem | tr -d '"')
cat > "$work/e2e.nix" <<NIX
derivation {
  name = "ietsp-e2e";
  system = "$system";
  builder = "/bin/sh";
  # busybox sh has no cat; use builtins only
  args = [ "-c" "while read -r l; do echo \"\$l\"; done < \${$work/src}/a > \$out; while read -r l; do echo \"\$l\"; done < \${$work/src}/sub/b >> \$out" ];
}
NIX
drv=$(nix-instantiate "$work/e2e.nix")
src=$(nix-store -q --references "$drv")
expected=$(nix-store -q --outputs "$drv")
echo "drv: $drv"
echo "src: $src"
cp "$drv" "$work/"
# Forget both so the builder has to upload and register them.
if nix-store --delete "$drv" "$src" >/dev/null 2>&1; then
    echo "deleted drv and source from the local store"
    ! nix-store --check-validity "$src" 2>/dev/null || fail "source still valid after delete"
else
    echo "note: could not delete $drv/$src from the store; upload still runs, registration is a no-op"
fi
out=$("$BIN/ietsp" build --builder "$url" --timings --source "$src=$work/src" "$work/$(basename "$drv")" 2>"$work/client1.log") || { cat "$work/client1.log"; fail "client exited non-zero"; }
cat "$work/client1.log"
[ "$out" = "$expected" ] || fail "output '$out' != expected '$expected'"
[ -e "$out" ] || fail "output path does not exist"
cat "$work/src/a" "$work/src/sub/b" | cmp - "$out" || fail "output content differs"
nix-store --check-validity "$drv" "$src" || fail "drv or source not valid after build"
[ -x "$src/run.sh" ] || fail "restored tree lost the executable bit"
[ "$(readlink "$src/link")" = a ] || fail "restored tree lost the symlink"
echo "ok: $out"

echo "== 2. hello from nixpkgs (cold builder, warm local store)"
hello=$(nix-instantiate -I nixpkgs="$NIXPKGS" '<nixpkgs>' -A hello 2>/dev/null)
expected=$(nix-store -q --outputs "$hello")
out=$("$BIN/ietsp" build --builder "$url" --timings "$hello" 2>"$work/client2.log") || { cat "$work/client2.log"; fail "client exited non-zero"; }
grep -E '^(plan|connect|have|put|build):' "$work/client2.log"
[ "$out" = "$expected" ] || fail "hello output '$out' != expected '$expected'"
echo "ok: $out"

echo "== 3. hello again (warm builder)"
out=$("$BIN/ietsp" build --builder "$url" --timings "$hello" 2>"$work/client3.log") || { cat "$work/client3.log"; fail "client exited non-zero"; }
grep -E '^(plan|connect|have|put|build):' "$work/client3.log"
grep -q 'have: .* 0 missing' "$work/client3.log" || fail "warm run still had missing objects"
[ "$out" = "$expected" ] || fail "hello output '$out' != expected '$expected'"
echo "ok: $out"

echo "== 4. eval path: custom derivation, drv and source absent on the builder"
mkdir -p "$work/src2"
echo "eval $RANDOM $$ $(date +%s%N)" > "$work/src2/somefile"
cat > "$work/e2e-eval.nix" <<NIX
derivation {
  name = "ietsp-e2e-eval";
  system = "$system";
  builder = "/bin/sh";
  args = [ "-c" "while read -r l; do echo \"\$l\"; done < \${$work/src2/somefile} > \$out" ];
}
NIX
drv=$(nix-instantiate "$work/e2e-eval.nix")
src=$(nix-store -q --references "$drv")
expected=$(nix-store -q --outputs "$drv")
nix-store --delete "$drv" "$src" >/dev/null 2>&1 || echo "note: could not delete $drv/$src from the store"
out=$("$BIN/ietsp" build --builder "$url" --timings "$work/e2e-eval.nix" 2>"$work/client4.log") || { cat "$work/client4.log"; fail "client exited non-zero"; }
grep -E '^(eval|plan|connect|have|put|build):' "$work/client4.log"
grep -q 'read from /nix/store' "$work/client4.log" && fail "eval path fell back to the local store"
[ "$out" = "$expected" ] || fail "output '$out' != expected '$expected'"
cmp "$work/src2/somefile" "$out" || fail "output content differs"
echo "ok: $out"

echo "== 5. eval path: hello (cold builder)"
rm -rf "$work/builder"
kill "$srv"; wait "$srv" 2>/dev/null || true
"$BIN/ietsp-builderd" --listen "127.0.0.1:$port" --store-dir "$work/builder" 2>>"$work/server.log" &
srv=$!
sleep 0.3
expected=$(nix-store -q --outputs "$hello")
out=$("$BIN/ietsp" build --builder "$url" --timings -I nixpkgs="$NIXPKGS" -E '(import <nixpkgs> {}).hello' 2>"$work/client5.log") || { cat "$work/client5.log"; fail "client exited non-zero"; }
grep -E '^(eval|plan|connect|have|put|build):' "$work/client5.log"
grep -q 'read from /nix/store' "$work/client5.log" && fail "eval path fell back to the local store"
[ "$out" = "$expected" ] || fail "hello output '$out' != expected '$expected'"
echo "ok: $out"

echo "== 6. eval path: hello (warm builder)"
out=$("$BIN/ietsp" build --builder "$url" --timings -I nixpkgs="$NIXPKGS" -E '(import <nixpkgs> {}).hello' 2>"$work/client6.log") || { cat "$work/client6.log"; fail "client exited non-zero"; }
grep -E '^(eval|plan|connect|have|put|build):' "$work/client6.log"
grep -q 'have: .* 0 missing' "$work/client6.log" || fail "warm run still had missing objects"
[ "$out" = "$expected" ] || fail "hello output '$out' != expected '$expected'"
echo "ok: $out"
echo "PASS"

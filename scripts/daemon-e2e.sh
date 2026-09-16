#!/usr/bin/env bash
# End-to-end test of the worker-protocol fallback (`--builder daemon`,
# `unix://`): the same cases as remote-e2e.sh against a Nix daemon.
#
# 1. custom derivation with a source tree through the local daemon, drv and
#    source deleted first; checks that the registered paths are byte-identical
#    to what nix-instantiate created (nix-store -q --hash, nix derivation show,
#    nix path-info ca), and --fetch-to.
# 2. hello through the local daemon: the closure is GC-rooted here, so every
#    path is valid and only QueryValidPaths and the realise run.
# 3. hello cold against a scratch chroot daemon (nix-daemon --store
#    local?root=DIR on its own socket), --no-build: 776 paths uploaded in one
#    AddMultipleToStore; hashes and ca checked in that store.
# 4. hello warm against the scratch daemon: 0 missing.
# 5. custom derivation through unix:///nix/var/nix/daemon-socket/socket.
#
# usage: scripts/daemon-e2e.sh            # uses target/release
#        BIN=target/debug scripts/daemon-e2e.sh
set -euo pipefail
cd "$(dirname "$0")/.."

BIN=${BIN:-target/release}
NIXPKGS=${NIXPKGS:-/nix/store/ah3pxz4xa52hd6vhbx17vcsci0b8bmbr-source}
export NIX_REMOTE=${NIX_REMOTE:-daemon}
export TMPDIR=${TMPDIR:-/tmp}

work=$(mktemp -d "$TMPDIR/ietsp-daemon-e2e.XXXXXX")
scratch_pid=
cleanup() {
    [ -n "$scratch_pid" ] && kill "$scratch_pid" 2>/dev/null || true
    rm -rf "$work"
}
trap cleanup EXIT

fail() { echo "FAIL: $*" >&2; exit 1; }
system=$(nix-instantiate --eval -E builtins.currentSystem | tr -d '"')

# make_case DIR NAME: writes DIR/src and DIR/case.nix, a derivation that copies the tree.
make_case() {
    local dir=$1 name=$2
    mkdir -p "$dir/src/sub"
    echo "hello $RANDOM $$ $(date +%s%N)" > "$dir/src/a"
    printf '#!/bin/sh\necho hi\n' > "$dir/src/run.sh"; chmod +x "$dir/src/run.sh"
    echo zzz > "$dir/src/sub/b"
    ln -s a "$dir/src/link"
    cat > "$dir/case.nix" <<NIX
derivation {
  name = "$name";
  system = "$system";
  builder = "/bin/sh";
  args = [ "-c" "while read -r l; do echo \"\$l\"; done < \${$dir/src}/a > \$out; while read -r l; do echo \"\$l\"; done < \${$dir/src}/sub/b >> \$out" ];
}
NIX
}

# run_custom DIR URL: instantiate, record what Nix created, delete, build via URL, compare.
run_custom() {
    local dir=$1 url=$2
    local drv src expected out
    drv=$(nix-instantiate "$dir/case.nix" 2>/dev/null)
    src=$(nix-store -q --references "$drv")
    expected=$(nix-store -q --outputs "$drv")
    echo "drv: $drv"
    echo "src: $src"
    cp "$drv" "$dir/nix.drv"
    nix-store -q --hash "$drv" "$src" > "$dir/nix.hashes"
    nix derivation show "$drv" > "$dir/nix.show" 2>/dev/null
    nix path-info --json "$src" 2>/dev/null | jq -c '.[] | {ca, narHash, narSize, references}' > "$dir/nix.src-info"
    nix path-info --json "$drv" 2>/dev/null | jq -c '.[] | {ca, narHash, narSize, references}' > "$dir/nix.drv-info"
    nix-store --delete "$drv" "$src" >/dev/null 2>&1 || fail "cannot delete $drv and $src from the store"
    ! nix-store --check-validity "$src" 2>/dev/null || fail "source still valid after delete"
    out=$("$BIN/ietsp" build --builder "$url" --timings --no-cache --fetch-to "$dir/fetch" "$dir/case.nix" 2>"$dir/client.log") || { cat "$dir/client.log"; fail "client exited non-zero"; }
    grep -E '^(eval|plan|connect|query|add|build|outputs|fetch|worker):' "$dir/client.log"
    grep -q 'read from /nix/store' "$dir/client.log" && fail "eval path fell back to the local store"
    [ "$out" = "$expected" ] || fail "output '$out' != expected '$expected'"
    cat "$dir/src/a" "$dir/src/sub/b" | cmp - "$out" || fail "output content differs"
    nix-store --check-validity "$drv" "$src" || fail "drv or source not valid after build"
    cmp "$drv" "$dir/nix.drv" || fail "registered drv differs from nix-instantiate's"
    nix-store -q --hash "$drv" "$src" | cmp - "$dir/nix.hashes" || fail "nix-store -q --hash differs"
    nix derivation show "$drv" 2>/dev/null | cmp - "$dir/nix.show" || fail "nix derivation show differs"
    nix path-info --json "$src" 2>/dev/null | jq -c '.[] | {ca, narHash, narSize, references}' | cmp - "$dir/nix.src-info" || fail "source path-info differs"
    nix path-info --json "$drv" 2>/dev/null | jq -c '.[] | {ca, narHash, narSize, references}' | cmp - "$dir/nix.drv-info" || fail "drv path-info differs"
    [ -x "$src/run.sh" ] || fail "restored tree lost the executable bit"
    [ "$(readlink "$src/link")" = a ] || fail "restored tree lost the symlink"
    cmp "$dir/fetch/$(basename "$out")" "$out" || fail "--fetch-to copy differs from the output"
    echo "ok: $out"
}

echo "== 1. custom derivation with a source tree, --builder daemon"
make_case "$work/c1" ietsp-daemon-e2e
run_custom "$work/c1" daemon

echo "== 2. hello through the local daemon (closure already valid)"
hello=$(nix-instantiate -I nixpkgs="$NIXPKGS" '<nixpkgs>' -A hello 2>/dev/null)
expected=$(nix-store -q --outputs "$hello")
out=$("$BIN/ietsp" build --builder daemon --timings --no-cache -I nixpkgs="$NIXPKGS" -E '(import <nixpkgs> {}).hello' 2>"$work/client2.log") || { cat "$work/client2.log"; fail "client exited non-zero"; }
grep -E '^(eval|plan|connect|query|add|build|outputs|worker):' "$work/client2.log"
grep -q 'read from /nix/store' "$work/client2.log" && fail "eval path fell back to the local store"
[ "$out" = "$expected" ] || fail "hello output '$out' != expected '$expected'"
echo "ok: $out"

echo "== 3. hello cold against a scratch chroot daemon, --no-build"
scratch="local?root=$work/store"
nix-daemon --store "$scratch" >"$work/scratch.log" 2>&1 &
scratch_pid=$!
sock=$work/store/nix/var/nix/daemon-socket/socket
for _ in $(seq 200); do [ -S "$sock" ] && break; sleep 0.05; done
[ -S "$sock" ] || { cat "$work/scratch.log"; fail "scratch daemon did not start"; }
"$BIN/ietsp" build --builder "unix://$sock" --timings --no-cache --no-build -I nixpkgs="$NIXPKGS" -E '(import <nixpkgs> {}).hello' 2>"$work/client3.log" >/dev/null || { cat "$work/client3.log"; fail "client exited non-zero"; }
grep -E '^(eval|plan|connect|query|add|build|outputs|worker):' "$work/client3.log"
grep -q 'query: 776 paths, 776 missing' "$work/client3.log" || fail "cold scratch store should miss every path"
grep -q 'worker: 3 ops' "$work/client3.log" || fail "expected SetOptions + QueryValidPaths + AddMultipleToStore"
[ "$(nix-store --store "$scratch" -q --hash "$hello")" = "$(nix-store -q --hash "$hello")" ] || fail "hello drv hash differs in the scratch store"
cmp "$work/store$hello" "$hello" || fail "hello drv bytes differ in the scratch store"
nix derivation show --store "$scratch" "$hello" 2>/dev/null | jq -e --arg k "$(basename "$hello")" '.derivations[$k].name == "hello-2.12.3"' >/dev/null || fail "nix derivation show failed in the scratch store"
for p in $(nix-store -q --references "$hello" | grep -v '\.drv$'); do
    a=$(nix path-info --store "$scratch" --json "$p" 2>/dev/null | jq -c '.[] | {ca, narHash, references}')
    b=$(nix path-info --json "$p" 2>/dev/null | jq -c '.[] | {ca, narHash, references}')
    [ "$a" = "$b" ] || fail "path-info differs for $p: $a vs $b"
done
echo "ok: 776 paths registered, hashes and ca match the local store"

echo "== 4. hello warm against the scratch daemon"
"$BIN/ietsp" build --builder "unix://$sock" --timings --no-cache --no-build -I nixpkgs="$NIXPKGS" -E '(import <nixpkgs> {}).hello' 2>"$work/client4.log" >/dev/null || { cat "$work/client4.log"; fail "client exited non-zero"; }
grep -E '^(eval|plan|connect|query|add|build|outputs|worker):' "$work/client4.log"
grep -q 'query: 776 paths, 0 missing' "$work/client4.log" || fail "warm run still had missing paths"
echo "ok"

echo "== 5. custom derivation, --builder unix:///nix/var/nix/daemon-socket/socket"
make_case "$work/c5" ietsp-daemon-e2e-unix
run_custom "$work/c5" unix:///nix/var/nix/daemon-socket/socket
echo "PASS"

#!/usr/bin/env bash
# Semantics of the tracing evaluation cache (docs/tracing-cache.md): hits
# print identical output, every kind of change misses, uncacheable runs and
# failed runs write nothing, --no-cache touches nothing.
set -uo pipefail
cd "$(dirname "$0")/.."
IETSP=${IETSP:-$PWD/target/release/ietsp}
work=$(mktemp -d "${TMPDIR:-/tmp}/ietsp-cachetest.XXXXXX")
trap 'rm -rf "$work"' EXIT
export IETSP_CACHE_DIR=$work/cache
export CACHETEST_VAR=one
mkdir -p "$work/src/dir" "$work/inc1" "$work/inc2"
cat > "$work/src/main.nix" <<'NIX'
{ a = import ./lib.nix; b = builtins.attrNames (builtins.readDir ./dir); c = builtins.getEnv "CACHETEST_VAR"; d = import <inc/x.nix>; }
NIX
echo '40 + 2' > "$work/src/lib.nix"
echo 'x' > "$work/src/dir/one"
echo '"inc1"' > "$work/inc1/x.nix"
echo '"inc2"' > "$work/inc2/x.nix"

fail=0
pass=0
check() { # name condition-description
  if [ "$1" = ok ]; then pass=$((pass+1)); echo "ok   $2"; else fail=1; echo "FAIL $2"; fi
}
# run [args...]: stdout to $out, cache status line to $status
run() {
  out=$("$IETSP" eval --strict --timings "$@" 2>"$work/err"); rc=$?
  status=$(grep -E '^cache:' "$work/err" | tr '\n' ' ')
}
main=(-I "inc=$work/inc1" "$work/src/main.nix")

run "${main[@]}"; first=$out
[[ $rc -eq 0 && $status == *"miss (no entry)"*"wrote"* ]] && check ok "first run misses and writes" || check no "first run misses and writes ($status)"
run "${main[@]}"
[[ $rc -eq 0 && $status == *"cache: hit"* && $out == "$first" ]] && check ok "second run hits with identical output" || check no "second run hits with identical output ($status)"

echo '40 + 3' > "$work/src/lib.nix"
run "${main[@]}"
[[ $status == *"miss ("*"lib.nix changed"* && $out == *"a = 43"* ]] && check ok "editing an imported file misses" || check no "editing an imported file misses ($status)"

touch -d '2000-01-01' "$work/src/lib.nix"
run "${main[@]}"
[[ $status == *"cache: hit"*"1 rehashed"* ]] && check ok "touch on an imported file hits after one rehash" || check no "touch hits after one rehash ($status)"
run "${main[@]}"
[[ $status == *"cache: hit"*"0 rehashed"* ]] && check ok "refreshed stat fields skip the rehash next time" || check no "refresh persisted ($status)"

echo 'y' > "$work/src/dir/two"
run "${main[@]}"
[[ $status == *"miss ("*"directory"* && $out == *'"two"'* ]] && check ok "adding a file to a readDir'd directory misses" || check no "readDir change misses ($status)"

CACHETEST_VAR=two run "${main[@]}"
[[ $status == *'miss ($CACHETEST_VAR changed)'* && $out == *'c = "two"'* ]] && check ok "changing a getEnv variable misses" || check no "getEnv change misses ($status)"
run "${main[@]}"
[[ $status == *"miss ("*'CACHETEST_VAR'* ]] && check ok "changing it back misses again (one entry per key)" || check no "one entry per key ($status)"

run -I "inc=$work/inc2" "$work/src/main.nix"
[[ $status == *"miss (no entry)"* && $out == *'d = "inc2"'* ]] && check ok "-I change misses (new key)" || check no "-I change misses ($status)"

run -E 'builtins.currentTime > 0'
[[ $status == *"not written"* ]] && check ok "currentTime never caches" || check no "currentTime never caches ($status)"
run -E 'builtins.currentTime > 0'
[[ $status == *"miss (no entry)"* ]] && check ok "currentTime: still no entry on the second run" || check no "currentTime second run ($status)"

run -E '(builtins.tryEval (builtins.fetchTarball "https://x/y.tar.gz")).success'
[[ $rc -ne 0 && $status != *"wrote"* ]] && check ok "a fetcher call fails and never caches" || check no "fetcher never caches ($status)"

before=$(find "$IETSP_CACHE_DIR" -type f | sort | xargs -r stat -c '%n %Y %s' | sha256sum)
run --no-cache "${main[@]}"
[[ $status == "" ]] && check ok "--no-cache prints no cache line" || check no "--no-cache prints no cache line ($status)"
run --no-cache -E 'import /nonexistent/nocache-probe.nix'
after=$(find "$IETSP_CACHE_DIR" -type f | sort | xargs -r stat -c '%n %Y %s' | sha256sum)
[[ $before == "$after" ]] && check ok "--no-cache never reads or writes" || check no "--no-cache changed the cache dir"
IETSP_CACHE=0 run "${main[@]}"
[[ $status == "" ]] && check ok "IETSP_CACHE=0 disables the cache" || check no "IETSP_CACHE=0 ($status)"

echo 'throw "boom"' > "$work/src/lib.nix"
run "${main[@]}"
[[ $rc -ne 0 && $status != *"wrote"* ]] && check ok "a failed evaluation writes nothing" || check no "failed evaluation wrote ($status)"
echo '1' > "$work/src/lib.nix"
run "${main[@]}"
[[ $rc -eq 0 && $status == *"miss ("* ]] && check ok "after the failure the fixed file misses, not a stale hit" || check no "after failure ($status)"

# identical output on a hit for a lang test with paths and positions
t=tests/lang/eval-okay-getattrpos.nix
o1=$("$IETSP" eval --strict "$t" 2>/dev/null); o2=$("$IETSP" eval --strict --timings "$t" 2>"$work/err")
[[ $o1 == "$o2" && $(grep -c 'cache: hit' "$work/err") -eq 1 ]] && check ok "lang test hit prints identical output" || check no "lang test hit"

echo "pass=$pass fail=$fail"
exit $fail

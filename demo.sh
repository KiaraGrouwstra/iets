export PATH=$HOME/tmp/ietsp-demo/result-classic/bin:$PATH
export IETSP_NIX_BIN_DIR=/run/current-system/sw/bin
NIXPKGS=/nix/store/ah3pxz4xa52hd6vhbx17vcsci0b8bmbr-source
cd $HOME/tmp/ietsp-classic-demo
run() { printf '\n\033[1;34m$ %s\033[0m\n' "$*"; "$@"; }
step() { printf '\n\033[1;33m# %s\033[0m\n' "$*"; }

step "ietsp answers under the classic names"
run readlink -f "$(command -v nix-instantiate)"

step "1. nix-instantiate --eval, same output as nix"
run nix-instantiate --eval --strict --json -E '{ a = 1; b = [ "x" ]; c = 2.5; }'

step "2. nix-instantiate a nixpkgs attribute: ietsp, then the real nix"
run bash -c "time nix-instantiate -I nixpkgs=$NIXPKGS '<nixpkgs>' -A hello"
run bash -c "time /run/current-system/sw/bin/nix-instantiate -I nixpkgs=$NIXPKGS '<nixpkgs>' -A hello"

step "3. multi-output derivation, the !output suffix as nix prints it"
run nix-instantiate -I nixpkgs=$NIXPKGS '<nixpkgs>' -A glibc.dev

step "4. nix-build through the daemon, result link as a GC root"
rm -f result
run nix-build . -A ok
run readlink result
run cat result

step "5. a failing build: provenance names the attr and the local file"
run nix-build . -A boom

step "6. nix eval on a locked flake"
run nix eval --json .#answer
run nix eval --raw .#answer.question

step "7. fallback: what ietsp does not do, the real nix does"
run nix-instantiate --parse -E '1 + 1'
run nix flake metadata . --json
run env IETSP_NO_FALLBACK=1 nix-instantiate --parse -E 1

printf '\n\033[1;32mdone. PATH has the classic names; play.\033[0m\n'

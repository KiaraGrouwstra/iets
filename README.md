# ietsp

A from-scratch Nix evaluator in Rust. Evaluation only. See ARCHITECTURE.md.

```
nix develop -c cargo build --release
./target/release/ietsp eval --strict -E '1 + 1'
scripts/langtests.sh
scripts/difftest.sh -I nixpkgs=/path -E '(import <nixpkgs> {}).hello.drvPath'
bench/run.sh nixos-desktop
```

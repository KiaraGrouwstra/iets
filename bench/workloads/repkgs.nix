# A package set that is not nixpkgs, for parity and for the bench.
#
# repkgs builds from its own bootstrap: its derivations are content addressed
# (`__contentAddressed = true`) and its store directory is empty, so an output
# path is a bare `/<hash>` with no name. Nothing in nixpkgs exercises that, and
# the evaluator's store-path code has no other test with those shapes.
#
# Resolve it through the search path, so the workload is portable and is
# skipped where the tree is absent:
#
#   ietsp eval-jobs --impure --no-instantiate -I repkgs=../repkgs \
#     bench/workloads/repkgs.nix
#
# `eval-jobs` is the driver, not `eval`: a handful of attributes abort, in this
# set and in the reference tools alike, so a whole-set `eval` cannot finish.
# Measured on 2026-09-29 against Nix 2.34.8 and nix-eval-jobs: `attr` and
# `bash` give byte-identical `drvPath` and `outPath`, and every attribute that
# fails fails with the same message in ietsp and in nix-eval-jobs.
import <repkgs> { system = builtins.currentSystem; }

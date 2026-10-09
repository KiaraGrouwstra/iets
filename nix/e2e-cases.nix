# The e2e checks: scripts/NAME.nu in a NixOS test container (nix/e2e-test.nix),
# with what each one needs. flake.nix runs them as checks, nix/coverage.nix
# runs them with an instrumented iets.
{ pkgs }:
let
  nix-grpc-daemon = import ./nix-grpc-store.nix { inherit pkgs; };
in
{
  cachetest = { };
  classic-e2e = { };
  daemon-e2e = { packages = [ pkgs.jq ]; };
  evaljobs-flags = {
    packages = [ pkgs.jq pkgs.nix-eval-jobs ];
    env = "NEJ=${pkgs.nix-eval-jobs}/bin/nix-eval-jobs";
  };
  build-e2e = { };
  flaketest = { };
  grpc-e2e = {
    packages = [ nix-grpc-daemon ];
    env = "DAEMON=${nix-grpc-daemon}";
  };
  plumbing-e2e = { };
  prefetch-e2e = { };
}

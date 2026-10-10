# Crane builds the dependencies as a derivation of their own, from the
# manifests alone, so a source edit rebuilds only the iets crate.
# `extraArgs` is for variants, such as the instrumented build of nix/coverage.nix,
# that need their own dependency build.
{ lib, craneLib, commonArgs, sources, extraArgs ? { } }:
let
  args = commonArgs // { src = sources.bin; } // extraArgs;
in
craneLib.buildPackage (args // {
  cargoArtifacts = craneLib.buildDepsOnly args;
  meta.license = lib.licenses.lgpl21Plus;
  # checks.test runs the suite; the package build stays a build.
  doCheck = false;
})

# Crane builds the dependencies as a derivation of their own, from the
# manifests alone, so a source edit rebuilds only the iets crate.
{ lib, craneLib, commonArgs, sources }:
let
  args = commonArgs // { src = sources.bin; };
in
craneLib.buildPackage (args // {
  cargoArtifacts = craneLib.buildDepsOnly args;
  meta.license = lib.licenses.lgpl21Plus;
  # checks.test runs the suite; the package build stays a build.
  doCheck = false;
})

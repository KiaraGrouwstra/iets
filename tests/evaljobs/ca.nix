# Floating content-addressed outputs, which no other jobset here has. Their
# path is not known before the build, so `outputs.<name>` is `null` and not a
# store path. nix-eval-jobs gets there by retrying `queryOutputs` without the
# paths once the first try throws (drv.cc); repkgs is a whole package set of
# this shape and nothing else in the suite reaches that retry.
{
  system ? "x86_64-linux",
}:
let
  ca =
    name: outputs:
    derivation {
      inherit name system outputs;
      builder = "/bin/sh";
      args = [
        "-c"
        "echo done > $out"
      ];
      __contentAddressed = true;
      outputHashMode = "recursive";
      outputHashAlgo = "sha256";
    };
  ia =
    name:
    derivation {
      inherit name system;
      builder = "/bin/sh";
      args = [
        "-c"
        "echo done > $out"
      ];
    };
in
{
  ca-one = ca "ca-one" [ "out" ];
  ca-two = ca "ca-two" [
    "out"
    "debug"
  ];
  # a text-hashed output: its path is known, so it keeps it
  ca-fixed = derivation {
    name = "ca-fixed";
    inherit system;
    builder = "/bin/sh";
    args = [
      "-c"
      "echo done > $out"
    ];
    outputHashMode = "flat";
    outputHashAlgo = "sha256";
    outputHash = "sha256-47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU=";
  };
  # two input-addressed jobs beside them, one nested, to show the walk is
  # unchanged and their outputs keep their paths
  input-addressed = ia "input-addressed";
  nested = {
    recurseForDerivations = true;
    inner = ia "nested-inner";
    mixed = ca "nested-ca" [ "out" ];
  };
}

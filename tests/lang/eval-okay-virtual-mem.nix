let
  t = builtins.toFile "virtual-mem" "hello";
  n = builtins.toFile "virtual-mem.nix" "1 + 1";
  d = derivation {
    name = "virtual-mem";
    builder = "/bin/sh";
    system = "x86_64-linux";
  };
in
[
  (builtins.readFile t)
  (builtins.pathExists t)
  (builtins.pathExists "${t}/")
  (builtins.readFileType t)
  (builtins.hashFile "sha256" t)
  (import n)
  (builtins.substring 0 7 (builtins.readFile d.drvPath))
  (import d.drvPath).outPath
]

# readFile carries the references of the file's store path that occur in
# the text (primops.cc:prim_readFile).
let
  a = builtins.toFile "a" "hello";
  b = builtins.toFile "b" "ref ${a}";
  c = builtins.toFile "c" "refs ${a} ${b}";
  d = builtins.toFile "d" "a hash-like run but no ref: 0123456789abcdfghijklmnpqrsvwxyz";
  # the hash part inside a longer nix32 run still counts
  e = builtins.toFile "e" "zz${builtins.substring 11 32 a}zz";
  drv =
    name: args:
    derivation {
      inherit name args;
      system = "x86_64-linux";
      builder = "/bin/sh";
    };
  f = drv "f" [ "${a}" ];
  g = drv "g" [ "${f}" ];
in
[
  (builtins.getContext (builtins.readFile a))
  (builtins.getContext (builtins.readFile b))
  (builtins.getContext (builtins.readFile c))
  (builtins.getContext (builtins.readFile d))
  (builtins.getContext (builtins.readFile e))
  (builtins.getContext (builtins.readFile f.drvPath))
  (builtins.getContext (builtins.readFile g.drvPath))
  (builtins.readFile c == "refs ${a} ${b}")
  (builtins.hasContext (builtins.readFile ./eval-okay-readfile.nix))
]

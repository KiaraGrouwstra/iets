# readFile of a store path carries the references of that path that occur
# in the text (primops.cc:prim_readFile rescans them).
let
  a = builtins.toFile "a" "hello";
  b = builtins.toFile "b" "ref ${a}";
  c = builtins.toFile "c" "refs ${a} ${b}";
  d = builtins.toFile "d" "a hash-like run but no ref: 0123456789abcdfghijklmnpqrsvwxyz";
  # the hash part inside a longer nix32 run still counts
  e = builtins.toFile "e" "zz${builtins.substring 11 32 a}zz";
in
[
  (builtins.getContext (builtins.readFile a))
  (builtins.getContext (builtins.readFile b))
  (builtins.getContext (builtins.readFile c))
  (builtins.getContext (builtins.readFile d))
  (builtins.getContext (builtins.readFile e))
  (builtins.readFile c == "refs ${a} ${b}")
  (builtins.hasContext (builtins.readFile ./eval-okay-readfile.nix))
]

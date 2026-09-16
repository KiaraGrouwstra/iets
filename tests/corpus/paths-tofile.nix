let
  a = builtins.toFile "a" "hello";
  b = builtins.toFile "b" "ref ${a}";
  c = builtins.toFile "c" "refs ${a} ${b} ${a}";
in
[
  a b c
  (builtins.hasContext a)
  (builtins.getContext a)
  (builtins.getContext b)
  (builtins.getContext c)
  (builtins.toFile "empty" "")
  (builtins.toFile "with-dash-1.0" "x")
]

let
  f = builtins.getFlake (toString ./getflake);
in
[
  f.x
  f.sub
  f._type
  f.me
  (f.outPath == f.me)
  f.narHash
  (builtins.attrNames f)
  (builtins.attrNames f.sourceInfo)
]

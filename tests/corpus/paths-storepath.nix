let
  nixpkgs = "/nix/store/ah3pxz4xa52hd6vhbx17vcsci0b8bmbr-source";
  p = builtins.storePath "${nixpkgs}/lib";
  q = builtins.storePath nixpkgs;
in
[
  p q
  (builtins.typeOf p)
  (builtins.hasContext p)
  (builtins.getContext p)
  (builtins.getContext q)
  (builtins.getContext "${p}${q}")
  (builtins.readFile (builtins.storePath "${nixpkgs}/.version"))
  (builtins.pathExists "${nixpkgs}/lib/")
]

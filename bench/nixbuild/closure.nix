{ marker, nixpkgs ? <nixpkgs> }:
let pkgs = import nixpkgs { system = "x86_64-linux"; };
in derivation {
  name = "iets-nixbuild-closure-${marker}";
  system = "x86_64-linux";
  builder = "${pkgs.bash}/bin/bash";
  args = [ "-c" ''
    ${pkgs.coreutils}/bin/mkdir $out
    ${pkgs.coreutils}/bin/head -c 16M /dev/urandom > $out/blob
    echo $python > $out/ref
  '' ];
  python = pkgs.python3;
  inherit marker;
}

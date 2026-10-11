{ marker, nixpkgs ? <nixpkgs> }:
let pkgs = import nixpkgs { system = "x86_64-linux"; };
in derivation {
  name = "iets-nixbuild-${marker}";
  system = "x86_64-linux";
  builder = "${pkgs.bash}/bin/bash";
  args = [ "-c" "echo $marker > $out" ];
  inherit marker;
}

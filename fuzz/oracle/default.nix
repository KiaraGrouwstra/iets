{ stdenv, pkg-config, nix }:
stdenv.mkDerivation {
  name = "iets-parse-oracle";
  src = ./parse-oracle.cc;
  dontUnpack = true;
  nativeBuildInputs = [ pkg-config ];
  buildInputs = [ nix.libs.nix-expr nix.libs.nix-store nix.libs.nix-fetchers nix.libs.nix-util ];
  buildPhase = ''
    $CXX -std=c++23 -O2 -o parse-oracle $src $(pkg-config --cflags --libs nix-expr nix-store nix-fetchers nix-util)
  '';
  installPhase = "install -Dm755 parse-oracle $out/bin/parse-oracle";
}

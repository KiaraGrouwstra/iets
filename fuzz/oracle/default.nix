{ stdenv, pkg-config, nix }:
stdenv.mkDerivation {
  name = "iets-fuzz-oracles";
  srcs = [ ./parse-oracle.cc ./nar-oracle.cc ];
  dontUnpack = true;
  nativeBuildInputs = [ pkg-config ];
  buildInputs = [ nix.libs.nix-expr nix.libs.nix-store nix.libs.nix-fetchers nix.libs.nix-util ];
  buildPhase = ''
    $CXX -std=c++23 -O2 -o parse-oracle ${./parse-oracle.cc} $(pkg-config --cflags --libs nix-expr nix-store nix-fetchers nix-util)
    $CXX -std=c++23 -O2 -o nar-oracle ${./nar-oracle.cc} $(pkg-config --cflags --libs nix-util)
  '';
  installPhase = "install -Dm755 -t $out/bin parse-oracle nar-oracle";
}

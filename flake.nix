{
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAll = f: nixpkgs.lib.genAttrs systems (s: f nixpkgs.legacyPackages.${s});
    in {
      devShells = forAll (pkgs: {
        default = pkgs.mkShell {
          packages = [ pkgs.cargo pkgs.rustc pkgs.clippy pkgs.rustfmt pkgs.hyperfine pkgs.nix pkgs.perf pkgs.jq ];
        };
      });
      packages = forAll (pkgs: {
        default = pkgs.rustPlatform.buildRustPackage {
          pname = "ietsp";
          version = "0.1.0";
          src = self;
          cargoLock.lockFile = ./Cargo.lock;
        };
      });
    };
}

{
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAll = f: nixpkgs.lib.genAttrs systems (s: f nixpkgs.legacyPackages.${s});
    in {
      devShells = forAll (pkgs: {
        default = pkgs.mkShell {
          packages = [ pkgs.cargo pkgs.rustc pkgs.clippy pkgs.rustfmt pkgs.hyperfine pkgs.nix pkgs.perf pkgs.jq pkgs.nushell pkgs.socat ];
        };
      });
      packages = forAll (pkgs: {
        default = pkgs.rustPlatform.buildRustPackage {
          pname = "ietsp";
          version = "0.1.0";
          src = self;
          cargoLock.lockFile = ./Cargo.lock;
          # the store tests read real .drv files from /nix/store; the
          # sandbox hides them. `cargo test` runs in the dev shell and CI.
          doCheck = false;
        };
      });
    };
}

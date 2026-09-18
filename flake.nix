{
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAll = f: nixpkgs.lib.genAttrs systems (s: f nixpkgs.legacyPackages.${s});
      ietsp = pkgs: pkgs.rustPlatform.buildRustPackage {
        pname = "ietsp";
        version = "0.1.0";
        src = self;
        cargoLock.lockFile = ./Cargo.lock;
        # checks.test runs the suite; the package build stays a build.
        doCheck = false;
      };
      # `classic`: the same binary under Nix's own command names. Each one
      # handles the subset ietsp implements and execs the real nix for the
      # rest, so this only belongs on a PATH that also has nix (docs/classic.md).
      classic = pkgs: pkgs.runCommand "ietsp-classic" { } ''
        mkdir -p $out/bin
        for n in nix-instantiate nix-build nix-eval-jobs nix; do
          ln -s ${ietsp pkgs}/bin/ietsp $out/bin/$n
        done
      '';
    in {
      devShells = forAll (pkgs: {
        default = pkgs.mkShell {
          packages = [ pkgs.cargo pkgs.rustc pkgs.clippy pkgs.rustfmt pkgs.hyperfine pkgs.nix pkgs.perf pkgs.jq pkgs.nushell ];
        };
      });
      nixosModules.default = import ./nix/module.nix self;
      packages = forAll (pkgs: { default = ietsp pkgs; classic = classic pkgs; });
      checks = forAll (pkgs:
        let
          pkg = ietsp pkgs;
          lint = name: tool: args: pkg.overrideAttrs (old: {
            pname = "ietsp-${name}";
            nativeBuildInputs = (old.nativeBuildInputs or [ ]) ++ [ tool ];
            buildPhase = "cargo ${args}";
            installPhase = "touch $out";
            doCheck = false;
            dontFixup = true;
          });
          # scripts/<name>.nu against the built binary. Only the scripts that
          # need no nix-daemon, no network and no live /nix/store; the rest stay manual.
          nuScript = name: pkgs.runCommand "ietsp-${name}" { nativeBuildInputs = [ pkgs.nushell ]; } ''
            cp -r ${self}/scripts ${self}/tests .
            chmod -R u+w scripts tests
            export IETSP=${pkg}/bin/ietsp
            nu scripts/${name}.nu
            touch $out
          '';
        in {
          # the tests that need a live /nix/store are #[ignore]d; run them
          # with `cargo test -- --ignored` in the dev shell.
          test = pkg.overrideAttrs { pname = "ietsp-test"; doCheck = true; };
          clippy = lint "clippy" pkgs.clippy "clippy --all-targets -- -D warnings";
          fmt = lint "fmt" pkgs.rustfmt "fmt --check";
          langtests = nuScript "langtests";
          partest = nuScript "partest";
        });
    };
}

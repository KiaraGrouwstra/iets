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
        meta.license = pkgs.lib.licenses.lgpl21Plus;
        env.IETSP_GIT_REV = self.shortRev or self.dirtyShortRev or "unknown";
        # checks.test runs the suite; the package build stays a build.
        doCheck = false;
      };
      # `classic`: the same binary under Nix's own command names. Each one
      # handles the subset ietsp implements and execs the real nix for the
      # rest; `share/ietsp/real-nix-bin` names that nix, so the fallback works
      # even when the farm shadows it out of PATH (docs/reference/classic.md).
      classic = pkgs: ietspPkg: nix: pkgs.runCommand "ietsp-classic" { } ''
        mkdir -p $out/bin
        mkdir -p $out/share/ietsp
        echo -n "${nix}/bin" > $out/share/ietsp/real-nix-bin
        for n in nix-instantiate nix-build nix-eval-jobs nix-shell nix-fast-build nix; do
          ln -s ${ietspPkg}/bin/ietsp $out/bin/$n
        done
      '';
    in {
      devShells = forAll (pkgs: {
        default = pkgs.mkShell {
          packages = [ pkgs.cargo pkgs.rustc pkgs.clippy pkgs.rustfmt pkgs.hyperfine pkgs.nix pkgs.perf pkgs.jq pkgs.nushell ];
        };
      });
      nixosModules.default = import ./nix/module.nix self;
      lib.mkClassic = classic;
      packages = forAll (pkgs: { default = ietsp pkgs; classic = classic pkgs (ietsp pkgs) pkgs.nix; });
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
          # scripts/<name>.nu against the built binary, in the build sandbox: the
          # scripts that need no nix-daemon and no live /nix/store.
          nuScript = name: pkgs.runCommand "ietsp-${name}" { nativeBuildInputs = [ pkgs.nushell ]; } ''
            cp -r ${self}/scripts ${self}/tests .
            chmod -R u+w scripts tests
            export IETSP=${pkg}/bin/ietsp
            nu scripts/${name}.nu
            touch $out
          '';
          # The scripts that need a nix-daemon and a writable store, in a NixOS test
          # container (nix/e2e-test.nix). Their derivations name x86_64-linux.
          e2e = name: import ./nix/e2e-test.nix { inherit self nixpkgs pkgs name; ietsp = pkg; };
        in {
          # the tests that need a live /nix/store are #[ignore]d; run them
          # with `cargo test -- --ignored` in the dev shell.
          test = pkg.overrideAttrs { pname = "ietsp-test"; doCheck = true; };
          clippy = lint "clippy" pkgs.clippy "clippy --all-targets -- -D warnings";
          # the library facade: every example links the library only (docs/reference/library.md)
          examples = lint "examples" pkgs.cargo "build --examples --release";
          fmt = lint "fmt" pkgs.rustfmt "fmt --check";
          diff-e2e = nuScript "diff-e2e";
          langtests = nuScript "langtests";
          linttest = nuScript "linttest";
          partest = nuScript "partest";
          # `ietsp.settings` reaches /etc/ietsp/ietsp.conf in the shape the
          # reader wants: one `name = value` line, lists joined by spaces.
          module =
            let
              sys = nixpkgs.lib.nixosSystem {
                system = pkgs.stdenv.hostPlatform.system;
                modules = [
                  self.nixosModules.default
                  {
                    boot.loader.grub.enable = false;
                    fileSystems."/" = { device = "none"; fsType = "tmpfs"; };
                    system.stateVersion = "25.11";
                    ietsp.settings = {
                      builder = "ssh-ng://bld1";
                      workers = 8;
                      provenance = false;
                      source-map = [ "/nix/store/x=/a" "/nix/store/y=/b" ];
                    };
                  }
                ];
              };
            in
            pkgs.runCommand "ietsp-module"
              { conf = sys.config.environment.etc."ietsp/ietsp.conf".text; }
              ''
                printf %s "$conf"
                printf %s "$conf" | grep -qx 'builder = ssh-ng://bld1'
                printf %s "$conf" | grep -qx 'workers = 8'
                printf %s "$conf" | grep -qx 'provenance = false'
                printf %s "$conf" | grep -qx 'source-map = /nix/store/x=/a /nix/store/y=/b'
                touch $out
              '';
        } // pkgs.lib.optionalAttrs (pkgs.stdenv.hostPlatform.system == "x86_64-linux") {
          classic-e2e = e2e "classic-e2e";
        });
    };
}

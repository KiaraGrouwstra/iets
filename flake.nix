{
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  # the reference stays on its own nixpkgs, where nix is 2.34.8
  inputs.nix-conformance.url = "git+ssh://git@github.com/numtide/nix-conformance";
  outputs = { self, nixpkgs, nix-conformance }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAll = f: nixpkgs.lib.genAttrs systems (s: f nixpkgs.legacyPackages.${s});
      # One source per consumer, so an edit rebuilds only what reads it. The
      # binary embeds docs/reference (src/cli/docs.rs) and no other page.
      fs = nixpkgs.lib.fileset;
      sourceOf = files: fs.toSource { root = ./.; fileset = fs.unions files; };
      binFiles = [ ./Cargo.toml ./Cargo.lock ./build.rs ./proto ./src ./docs/reference ];
      binSrc = sourceOf binFiles;
      testSrc = sourceOf (binFiles ++ [ ./tests ./examples ]);
      fuzzSrc = sourceOf (binFiles ++ [ ./fuzz ]);
      scriptsSrc = sourceOf [ ./scripts ./tests ];
      astGrepSrc = sourceOf [ ./sgconfig.yml ./nix/ast-grep (fs.fileFilter (f: f.hasExt "rs") ./.) ];
      # the e2e containers evaluate this flake as a git checkout
      e2eSrc = sourceOf (binFiles ++ [ ./flake.nix ./flake.lock ./tests ./examples ./scripts ./nix ./fuzz ./sgconfig.yml ]);
      iets = pkgs: pkgs.rustPlatform.buildRustPackage {
        pname = "iets";
        version = "0.1.0";
        src = binSrc;
        cargoLock.lockFile = ./Cargo.lock;
        meta.license = pkgs.lib.licenses.lgpl21Plus;
        # checks.test runs the suite; the package build stays a build.
        doCheck = false;
      };
      # `classic`: the same binary under Nix's own command names. Each one
      # handles the subset iets implements and execs the real nix for the
      # rest; `share/iets/real-nix-bin` names that nix, so the fallback works
      # even when the farm shadows it out of PATH (docs/reference/classic.md).
      classic = pkgs: ietsPkg: nix: pkgs.runCommand "iets-classic" { } ''
        mkdir -p $out/bin
        mkdir -p $out/share/iets
        echo -n "${nix}/bin" > $out/share/iets/real-nix-bin
        for n in nix-instantiate nix-build nix-eval-jobs nix-shell nix; do
          ln -s ${ietsPkg}/bin/iets $out/bin/$n
        done
      '';
      # the differential fuzzer against Nix (docs/how-to/fuzz.md)
      fuzz = pkgs: import ./fuzz {
        inherit pkgs;
        src = fuzzSrc;
        nixpkgsSrc = nixpkgs;
        conformance = nix-conformance;
        iets = iets pkgs;
      };
    in {
      devShells = forAll (pkgs: {
        default = pkgs.mkShell {
          packages = [ pkgs.cargo pkgs.rustc pkgs.clippy pkgs.rustfmt pkgs.hyperfine pkgs.nix pkgs.perf pkgs.jq pkgs.nushell pkgs.direnv pkgs.ast-grep ];
        };
      });
      nixosModules.default = import ./nix/module.nix self;
      lib.mkClassic = classic;
      packages = forAll (pkgs: {
        default = iets pkgs;
        classic = classic pkgs (iets pkgs) pkgs.nix;
        # one musl file that needs nothing on the machine (docs/explanation/static-build.md)
        static = iets pkgs.pkgsStatic;
        fuzz-oracles = (fuzz pkgs).oracles;
        inherit (fuzz pkgs) fuzz-diff fuzzers;
        fuzz-parse-campaign = (fuzz pkgs).campaign { target = "parse"; };
        fuzz-nar-campaign = (fuzz pkgs).campaign { target = "nar-gen"; };
        fuzz-wire-campaign = (fuzz pkgs).campaign { target = "wire"; };
        fuzz-eval-campaign = (fuzz pkgs).campaign { target = "eval"; };
        fuzz-drv-campaign = (fuzz pkgs).campaign { target = "drv-gen"; };
        fuzz-drv-eval-campaign = (fuzz pkgs).campaign { target = "drv-eval"; };
      } // pkgs.lib.optionalAttrs (pkgs.stdenv.hostPlatform.system == "x86_64-linux") {
        # line coverage of the e2e tests in NixOS containers (nix/coverage.nix)
        coverage-e2e = import ./nix/coverage.nix { inherit self nixpkgs pkgs; src = e2eSrc; iets = iets pkgs; };
      });
      checks = forAll (pkgs:
        let
          pkg = iets pkgs;
          lint = name: tool: args: pkg.overrideAttrs (old: {
            pname = "iets-${name}";
            src = testSrc;
            nativeBuildInputs = (old.nativeBuildInputs or [ ]) ++ [ tool ];
            buildPhase = "cargo ${args}";
            installPhase = "touch $out";
            doCheck = false;
            dontFixup = true;
          });
          # scripts/<name>.nu against the built binary, in the build sandbox: the
          # scripts that need no nix-daemon and no live /nix/store.
          nuScript = name: pkgs.runCommand "iets-${name}" { nativeBuildInputs = [ pkgs.nushell ]; } ''
            cp -r ${scriptsSrc}/scripts ${scriptsSrc}/tests .
            chmod -R u+w scripts tests
            export IETS=${pkg}/bin/iets
            nu scripts/${name}.nu
            touch $out
          '';
          # The scripts that need a nix-daemon and a writable store, in a NixOS test
          # container (nix/e2e-test.nix). Their derivations name x86_64-linux.
          e2e = name: args: import ./nix/e2e-test.nix ({ inherit self nixpkgs pkgs name; src = e2eSrc; iets = pkg; } // args);
        in {
          # the tests that need a live /nix/store are #[ignore]d; run them
          # with `cargo test -- --ignored` in the dev shell.
          # the dev profile, as `cargo test` in the dev shell: the release
          # profile is fat LTO, minutes for no extra coverage
          test = lint "test" pkgs.cargo "test";
          clippy = lint "clippy" pkgs.clippy "clippy --all-targets -- -D warnings";
          # the library facade: every example links the library only (docs/reference/library.md)
          examples = lint "examples" pkgs.cargo "build --examples";
          fmt = lint "fmt" pkgs.rustfmt "fmt --check";
          # nix/ast-grep, through sgconfig.yml
          ast-grep = pkgs.runCommand "iets-ast-grep" { nativeBuildInputs = [ pkgs.ast-grep ]; } "cd ${astGrepSrc} && ast-grep scan && touch $out";
          diff-e2e = nuScript "diff-e2e";
          langtests = nuScript "langtests";
          # the differential fuzzers against Nix (fuzz/default.nix)
          inherit ((fuzz pkgs).checks) fuzz-parse fuzz-nar fuzz-wire fuzz-eval fuzz-drv fuzz-drv-eval conformance;
          linttest = nuScript "linttest";
          partest = nuScript "partest";
          static = iets pkgs.pkgsStatic;
        } // pkgs.lib.optionalAttrs (pkgs.stdenv.hostPlatform.system == "x86_64-linux") (
          pkgs.lib.mapAttrs e2e (import ./nix/e2e-cases.nix { inherit pkgs; })
        ));
    };
}

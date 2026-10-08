# The fuzz targets as packages, checks and campaigns (docs/how-to/fuzz.md).
{ pkgs, src, nixpkgsSrc }:
let
  inherit (pkgs) lib;
  oracles = pkgs.callPackage ./oracle { };
  common = {
    version = "0.0.0";
    inherit src;
    cargoRoot = "fuzz";
    buildAndTestSubdir = "fuzz";
    cargoLock.lockFile = ./Cargo.lock;
    doCheck = false;
    meta.license = lib.licenses.lgpl21Plus;
  };
  # what each libFuzzer binary is run with; the seeds go to a directory `seeds`
  targets = {
    parse = {
      flags = "-max_len=4096 -dict=${./parse.dict}";
      # an eighth of nixpkgs in a check: the time budget includes reading them
      seeds = every: ''
        find ${nixpkgsSrc} -name '*.nix' -size -32k | awk 'NR % ${toString every} == 0' | while read -r f; do
          ln -s "$f" "seeds/$(echo "$f" | sha1sum | cut -c1-40)"
        done
      '';
    };
    nar-read = { flags = "-max_len=65536 -dict=${./nar.dict}"; seeds = _: ""; };
    nar-gen = { flags = "-max_len=4096"; seeds = _: ""; };
    nar-tree = { flags = "-max_len=4096"; seeds = _: ""; };
  };
  env = ''
    export IETS_PARSE_ORACLE=${oracles}/bin/parse-oracle
    export IETS_NAR_ORACLE=${oracles}/bin/nar-oracle
  '';
  # the replay tool, built plain: it is fast and needs no libFuzzer
  fuzz-diff = pkgs.rustPlatform.buildRustPackage (common // {
    pname = "iets-fuzz-diff";
    cargoBuildFlags = [ "--bin" "fuzz-diff" ];
  });
  # the libFuzzer binaries, instrumented by cargo-fuzz on the stable compiler
  fuzzers = pkgs.rustPlatform.buildRustPackage (common // {
    pname = "iets-fuzzers";
    nativeBuildInputs = [ pkgs.cargo-fuzz ];
    buildPhase = ''
      runHook preBuild
      (cd fuzz && for t in ${toString (builtins.attrNames targets)}; do
        cargo fuzz build --sanitizer none --release $t
      done)
      runHook postBuild
    '';
    installPhase = ''
      for t in ${toString (builtins.attrNames targets)}; do
        install -Dm755 fuzz/target/*/release/$t $out/bin/fuzz-$t
      done
    '';
    dontFixup = true;
  });
  regressions = name: lib.optionalString (builtins.pathExists (./regressions + "/${name}")) "${./regressions + "/${name}"}";
  # one target for a fixed time; a divergence fails the build and prints the input
  run = name: seconds: ''
    mkdir -p seeds new-${name}
    rm -f seeds/*
    ${targets.${name}.seeds 8}
    fuzz-${name} -max_total_time=${toString seconds} ${targets.${name}.flags} \
      -artifact_prefix=art-${name}- new-${name} seeds ${regressions name} || {
        for a in art-${name}-*; do echo "the input, in base64: $(base64 -w0 "$a")" >&2; done
        exit 1
      }
  '';
  check = { name, script }: pkgs.runCommand "iets-fuzz-${name}-check" {
    nativeBuildInputs = [ fuzz-diff fuzzers oracles ];
  } ''
    ${env}
    ${script}
    mkdir -p $out
  '';
  # a long run of one target on every core: the grown corpus, minimised, and
  # every input that diverged, replayed and classified in triage.jsonl
  campaign = { target ? "parse", seconds ? 3600, corpus ? null }: pkgs.runCommand "iets-fuzz-${target}-campaign" {
    nativeBuildInputs = [ fuzz-diff fuzzers oracles ];
  } ''
    ${env}
    mkdir seeds grown $out $out/corpus $out/artifacts
    ${targets.${target}.seeds 1}
    fuzz-${target} -merge=1 ${targets.${target}.flags} grown seeds ${regressions target} ${toString corpus} 2> $out/seed.log
    fuzz-${target} -fork=$NIX_BUILD_CORES -ignore_crashes=1 -max_total_time=${toString seconds} \
      ${targets.${target}.flags} -artifact_prefix=$out/artifacts/ grown 2> $out/fuzz.log || true
    fuzz-${target} -merge=1 ${targets.${target}.flags} $out/corpus grown 2> $out/merge.log
    fuzz-diff --json ${target} $out/artifacts > $out/triage.jsonl || true
  '';
in
{
  inherit oracles fuzz-diff fuzzers campaign;
  checks = {
    # every .nix file of nixpkgs through both parsers, then a minute of libFuzzer
    fuzz-parse = check {
      name = "parse";
      script = ''
        fuzz-diff --nix-only parse ${nixpkgsSrc}
        ${run "parse" 60}
      '';
    };
    # twenty seconds of each NAR target
    fuzz-nar = check {
      name = "nar";
      script = lib.concatMapStrings (t: run t 20) [ "nar-read" "nar-gen" "nar-tree" ];
    };
  };
}

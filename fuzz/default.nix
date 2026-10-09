# The fuzz targets as packages, checks and campaigns (docs/how-to/fuzz.md).
{ pkgs, src, nixpkgsSrc, conformance, iets }:
let
  inherit (pkgs) lib;
  oracles = conformance.packages.${pkgs.stdenv.hostPlatform.system}.oracles;
  common = {
    version = "0.0.0";
    inherit src;
    cargoRoot = "fuzz";
    buildAndTestSubdir = "fuzz";
    cargoLock.lockFile = ./Cargo.lock;
    doCheck = false;
    meta.license = lib.licenses.lgpl21Plus;
  };
  # the cases and the grown corpora of nix-conformance, as libFuzzer seeds
  link = dirs: name: ''
    find ${toString dirs} -name '${name}' | while read -r f; do
      ln -sf "$f" "seeds/$(echo "$f" | sha1sum | cut -c1-40)"
    done
  '';
  # what each libFuzzer binary is run with; the seeds go to a directory `seeds`
  targets = {
    parse = {
      flags = "-max_len=4096 -dict=${./parse.dict}";
      # an eighth of nixpkgs in a check: the time budget includes reading them
      seeds = every: ''
        find ${nixpkgsSrc} -name '*.nix' -size -32k | awk 'NR % ${toString every} == 0' | while read -r f; do
          ln -s "$f" "seeds/$(echo "$f" | sha1sum | cut -c1-40)"
        done
        ${link [ "${conformance}/parse" "${conformance}/corpus/parse" ] "*.nix"}
      '';
    };
    nar-read = {
      flags = "-max_len=65536 -dict=${./nar.dict}";
      seeds = _: link [ "${conformance}/nar" "${conformance}/corpus/nar" ] "*.nar";
    };
    nar-gen = { flags = "-max_len=4096"; seeds = _: ""; };
    nar-tree = { flags = "-max_len=4096"; seeds = _: ""; };
    wire = { flags = "-max_len=4096"; seeds = _: ""; };
  };
  env = ''
    export IETS_PARSE_ORACLE=${oracles}/bin/parse-oracle
    export IETS_NAR_ORACLE=${oracles}/bin/nar-oracle
    export IETS_WIRE_ORACLE=${oracles}/bin/wire-oracle
  '';
  # the replay tool, built plain: it is fast and needs no libFuzzer
  fuzz-diff = pkgs.rustPlatform.buildRustPackage (common // {
    pname = "iets-fuzz-diff";
    cargoBuildFlags = [ "--bin" "fuzz-diff" "--bin" "conformance-adapter" ];
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
  # one target for a fixed time; a divergence fails the build and prints the input
  run = name: seconds: ''
    mkdir -p seeds new-${name}
    rm -f seeds/*
    ${targets.${name}.seeds 8}
    fuzz-${name} -max_total_time=${toString seconds} ${targets.${name}.flags} \
      -artifact_prefix=art-${name}- new-${name} seeds || {
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
    fuzz-${target} -merge=1 ${targets.${target}.flags} grown seeds ${toString corpus} 2> $out/seed.log
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
    # every case of nix-conformance through iets, with the trees; the cases
    # in conformance-known fail on purpose
    conformance = conformance.lib.${pkgs.stdenv.hostPlatform.system}.check {
      name = "iets";
      adapter = "${pkgs.writeShellScript "iets-conformance-adapter" ''
        export IETS=${iets}/bin/iets
        exec ${fuzz-diff}/bin/conformance-adapter "$@"
      ''}";
      flags = [ "--tree" "--known" "${./conformance-known}" ];
    };
    # half a minute of iets's daemon client against Nix's, on scripted daemons
    fuzz-wire = check {
      name = "wire";
      script = run "wire" 30;
    };
    # twenty seconds of each NAR target
    fuzz-nar = check {
      name = "nar";
      script = lib.concatMapStrings (t: run t 20) [ "nar-read" "nar-gen" "nar-tree" ];
    };
  };
}

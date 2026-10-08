# The parse fuzzer as packages and a check (docs/how-to/fuzz.md).
{ pkgs, src, nixpkgsSrc }:
let
  oracle = pkgs.callPackage ./oracle { };
  common = {
    version = "0.0.0";
    inherit src;
    cargoRoot = "fuzz";
    buildAndTestSubdir = "fuzz";
    cargoLock.lockFile = ./Cargo.lock;
    doCheck = false;
    meta.license = pkgs.lib.licenses.lgpl21Plus;
  };
  # the replay tool, built plain: it is fast and needs no libFuzzer
  parse-diff = pkgs.rustPlatform.buildRustPackage (common // {
    pname = "iets-parse-diff";
    cargoBuildFlags = [ "--bin" "parse-diff" ];
  });
  # the libFuzzer binary, instrumented by cargo-fuzz on the stable compiler
  fuzzer = pkgs.rustPlatform.buildRustPackage (common // {
    pname = "iets-fuzz-parse";
    nativeBuildInputs = [ pkgs.cargo-fuzz ];
    buildPhase = ''
      runHook preBuild
      (cd fuzz && cargo fuzz build --sanitizer none --release parse)
      runHook postBuild
    '';
    installPhase = ''
      install -Dm755 fuzz/target/*/release/parse $out/bin/fuzz-parse
      install -Dm644 fuzz/parse.dict $out/share/iets-fuzz/parse.dict
    '';
    dontFixup = true;
  });
  # every .nix file of nixpkgs replayed, then libFuzzer for a fixed time
  check = { seconds ? 60 }: pkgs.runCommand "iets-fuzz-parse-check" {
    nativeBuildInputs = [ parse-diff fuzzer oracle ];
  } ''
    export IETS_PARSE_ORACLE=${oracle}/bin/parse-oracle
    parse-diff --nix-only ${nixpkgsSrc}
    mkdir seeds new
    # an eighth of nixpkgs: the time budget includes reading the seeds
    find ${nixpkgsSrc} -name '*.nix' -size -32k | awk 'NR % 8 == 0' | while read -r f; do
      ln -s "$f" "seeds/$(echo "$f" | sha1sum | cut -c1-40)"
    done
    fuzz-parse -max_total_time=${toString seconds} -max_len=4096 \
      -dict=${fuzzer}/share/iets-fuzz/parse.dict -artifact_prefix=art- \
      new seeds ${./regressions/parse} || {
        for a in art-*; do echo "the input, in base64: $(base64 -w0 "$a")" >&2; done
        exit 1
      }
    mkdir -p $out
  '';
  # a long run on every core: the grown corpus, minimised, and every input
  # that diverged, replayed and classified in triage.jsonl
  campaign = { seconds ? 3600, corpus ? null }: pkgs.runCommand "iets-fuzz-parse-campaign" {
    nativeBuildInputs = [ parse-diff fuzzer oracle ];
  } ''
    export IETS_PARSE_ORACLE=${oracle}/bin/parse-oracle
    mkdir seeds grown $out $out/corpus $out/artifacts
    find ${nixpkgsSrc} -name '*.nix' -size -32k | while read -r f; do
      ln -s "$f" "seeds/$(echo "$f" | sha1sum | cut -c1-40)"
    done
    fuzz-parse -merge=1 -max_len=4096 grown seeds ${./regressions/parse} ${toString corpus} 2> $out/seed.log
    fuzz-parse -fork=$NIX_BUILD_CORES -ignore_crashes=1 -max_total_time=${toString seconds} \
      -max_len=4096 -dict=${fuzzer}/share/iets-fuzz/parse.dict \
      -artifact_prefix=$out/artifacts/ grown 2> $out/fuzz.log || true
    fuzz-parse -merge=1 -max_len=4096 $out/corpus grown 2> $out/merge.log
    parse-diff --json $out/artifacts > $out/triage.jsonl || true
  '';
in
{
  inherit parse-diff check campaign;
  parse-oracle = oracle;
  fuzz-parse = fuzzer;
}

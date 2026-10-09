# How to fuzz iets against Nix

`fuzz/` gives the same input to iets and to Nix 2.34.8, and tells you
when their answers differ. This page shows you how to run it and what to
do with a difference.

Each target, its kinds of difference and the file formats are in
[reference/fuzz.md](../reference/fuzz.md).

## Run the checks

The quickest start is the flake checks:

```sh
nix build .#checks.x86_64-linux.fuzz-parse -L
nix build .#checks.x86_64-linux.fuzz-nar -L
nix build .#checks.x86_64-linux.fuzz-wire -L
nix build .#checks.x86_64-linux.fuzz-eval -L
nix build .#checks.x86_64-linux.fuzz-drv -L
nix build .#checks.x86_64-linux.conformance -L
```

A check fails when it finds a difference that `fuzz/divergences.toml`
does not list, and it prints the input in base64.

## Run a fuzzer by hand

The flake's stable Rust is enough. `cargo fuzz` runs on it with
`--sanitizer none`; AddressSanitizer would need nightly.

```sh
nix build .#fuzz-oracles -o oracles
nix build .#fuzzers -o fuzzers
export IETS_PARSE_ORACLE=$PWD/oracles/bin/parse-oracle
export IETS_NAR_ORACLE=$PWD/oracles/bin/nar-oracle
mkdir -p corpus && cp tests/lang/*.nix corpus/
fuzzers/bin/fuzz-parse -dict=fuzz/parse.dict -max_len=4096 corpus
fuzzers/bin/fuzz-nar-gen -max_len=4096 nar-corpus
```

`fuzz/default.nix` has the flags of each target. To use N cores and keep
going after a difference, add `-fork=N -ignore_crashes=1`. Each difference
is then saved as a file `crash-<sha1>`.

While you change iets, build one target with
`cargo fuzz build -s none -O TARGET` in `fuzz/`.

## Replay inputs

`fuzz-diff` runs files through a target without libFuzzer. Give it a crash
file, a corpus, or a tree of `.nix` files:

```sh
nix build .#fuzz-diff -o fuzz-diff
fuzz-diff/bin/fuzz-diff parse crash-0123abcd
fuzz-diff/bin/fuzz-diff --nix-only parse ~/src/nixpkgs
fuzz-diff/bin/fuzz-diff --json nar-gen corpus/ > triage.jsonl
```

## Deal with a difference

A difference looks like this:

```
parse divergence (position)
  nix:  error at 1:1: syntax error, unexpected end of path, expecting '${'
  iets: error at 1:4: syntax error, unexpected end of path, expecting '${'
```

1. Make the input small:
   `fuzz-parse -minimize_crash=1 -runs=10000 crash-0123abcd`.
2. Check it with the real Nix. For the parser, write the input to a file
   and run `nix-instantiate --parse FILE`. Do not use `-E` or standard
   input: there Nix finds lines only in the text before the first NUL byte
   (`Pos::getSource`, position.cc). For a NAR, use
   `nix-store --restore DIR < FILE` and `nix-store --dump DIR`.
3. Find the rule in the Nix source (`src/libexpr/parser.y`, `lexer.l`,
   `parser-state.hh`, `src/libutil/archive.cc`). Then decide:
   - **iets is wrong.** Fix it, and add a test next to the code. Put the
     small input in `parse/` or `nar/` of nix-conformance, named by its
     sha1, with the answer `./expect` writes.
   - **iets differs on purpose.** Add a section to
     `docs/reference/divergences.md` and an entry to
     `fuzz/divergences.toml` (see below).
   - **The oracle or the target is wrong.** Fix it there.

To look at the input of the last run, set `IETS_WIRE_SCRIPT`,
`IETS_EVAL_SOURCE` or `IETS_DRV_WRITE` to a file name. To serve a `wire`
script to a client by hand, use for example
`socat UNIX-LISTEN:SOCK,fork SYSTEM:'cat FILE'`.

## Record a known divergence

Add an entry to `fuzz/divergences.toml`, under the table of the target's
family:

```toml
[[parse]]
id = "nul-outside-a-string"
doc = "docs/reference/divergences.md#a-nul-byte-outside-a-string"
input = "\u0000"
iets = "unexpected invalid token"
```

The fuzzer then counts that difference as expected and stops adding it to
the corpus. Keep entries narrow: a broad entry hides bugs. To try another
list, set `IETS_FUZZ_DIVERGENCES=FILE`; otherwise each binary uses the list
it was built with.

## Run a long campaign

```sh
nix-farm '.#packages.x86_64-linux.fuzz-parse-campaign' --fetch-to campaign
nix-farm '.#packages.x86_64-linux.fuzz-nar-campaign' --fetch-to campaign
nix-farm '.#packages.x86_64-linux.fuzz-eval-campaign' --fetch-to campaign
```

A campaign fuzzes on every core of the builder for an hour. Look in
`campaign/artifacts/` for the inputs that differed, and in
`campaign/triage.jsonl` for them replayed. To run another target, for
another time, or from an earlier corpus, use
`(import ./fuzz { ... }).campaign { target; seconds; corpus; }`.

The grown corpora are not in the repository. The parse seeds come from the
pinned nixpkgs on each run, so only the growth needs a home.

## Measure coverage

```sh
nix shell nixpkgs#cargo-fuzz nixpkgs#llvmPackages_21.llvm -c \
  cargo fuzz coverage -s none parse CORPUS
llvm-profdata merge -sparse fuzz/coverage/parse/raw \
  -o fuzz/coverage/parse/coverage.profdata
llvm-cov report \
  target/x86_64-unknown-linux-gnu/coverage/x86_64-unknown-linux-gnu/release/parse \
  -instr-profile=fuzz/coverage/parse/coverage.profdata src/syntax/*.rs
```

`cargo fuzz coverage` looks for `llvm-profdata` in the Rust sysroot, and
the flake's Rust has none there, so you merge in a separate step. The LLVM
major version must match the one `rustc -vV` names. For several targets,
merge their `raw` directories into one profile, and give `llvm-cov` one
binary plus the others with `-object`.

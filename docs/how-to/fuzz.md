# How to fuzz iets against Nix

`fuzz/` is a differential fuzzer. It gives the same input to iets and to
Nix 2.34.8, compares the two answers, and reports each difference. The
first target is the parser. This page shows how to run it and how to read
what it reports.

## What the parse target compares

For each input, both parsers give one of two answers:

- the input parses, to a tree;
- the input is refused, at a line and a column.

`fuzz/oracle/parse-oracle.cc` links libnixexpr 2.34.8 and prints Nix's
tree. `fuzz/src/canon.rs` builds iets's tree in the same form, with the
desugaring of Nix's `parser.y`: `a - b` is a call of `__sub`, `a.b = 1;`
makes a nested set, `{ x } @ y` sorts its formals. Both trees then pass
through `sx::normalise`, which removes what is not structure: the order
Nix stores bindings in, where a lexer splits a string into tokens, and
iets folding `-1` into a literal.

The target compares:

- whether both sides accept or both refuse;
- for a refusal, the line and the column;
- for an acceptance, the two trees.

It does not compare:

- the text of an error message. iets words some errors its own way;
- scope. Nix's parser also refuses an undefined variable. The oracle
  puts a `with` around the input to turn this off. iets finds undefined
  variables in `compile`, which the evaluator target will test;
- warnings, lints and doc comments.

A difference has one of six kinds:

| Kind | Meaning |
|:---|:---|
| `nix-only` | Nix parses the input, iets refuses it |
| `iets-only` | iets parses the input, Nix refuses it |
| `position` | both refuse it, at different places |
| `tree` | both parse it, to different trees |
| `iets-panic` | the iets parser panicked |
| `nix-crash` | the oracle process died on the input |

## Run the check

```sh
nix build .#checks.x86_64-linux.fuzz-parse -L
```

The check gives every `.nix` file of the pinned nixpkgs to both parsers,
then runs libFuzzer for 60 seconds from an eighth of those files and
`fuzz/regressions/parse/`. It fails on a difference that
`fuzz/divergences.toml` does not list, and prints the input in base64.

## Run the fuzzer by hand

The toolchain is the stable Rust of the flake. `cargo fuzz` runs on it
with `--sanitizer none`; AddressSanitizer would need nightly, and the
parser has no `unsafe` code for it to check.

```sh
nix build .#parse-oracle -o oracle
export IETS_PARSE_ORACLE=$PWD/oracle/bin/parse-oracle
nix shell nixpkgs#cargo-fuzz -c cargo fuzz build -s none -O parse
mkdir -p corpus && cp tests/lang/*.nix corpus/
fuzz/target/x86_64-unknown-linux-gnu/release/parse \
  -dict=fuzz/parse.dict -max_len=4096 corpus fuzz/regressions/parse
```

Add `-fork=N -ignore_crashes=1` to use N cores and keep going after a
difference. Each difference is then a file `crash-<sha1>`.

`nix build .#fuzz-parse` gives the same binary as `bin/fuzz-parse`.

## Replay inputs

`parse-diff` gives files to both parsers without libFuzzer: a crash
artifact, a corpus, or a tree of `.nix` files.

```sh
nix build .#parse-diff -o parse-diff
parse-diff/bin/parse-diff crash-0123abcd
parse-diff/bin/parse-diff --nix-only ~/src/nixpkgs
parse-diff/bin/parse-diff --json corpus/ > triage.jsonl
```

The exit code is 0 when no difference is unexpected, 1 when one is, and
2 when the run itself failed (no oracle, a bad divergence list). With
`--json`, each unexpected difference is one line
`{"file", "kind", "at", "nix", "iets"}`, and the last line is
`{"summary": {"inputs", "same", "expected", "bugs", "seconds"}}`.
`expected` counts the hits of each known divergence.

## Read a failure

A failure looks like this:

```
parse divergence (position)
  nix:  error at 1:1: syntax error, unexpected end of path, expecting '${'
  iets: error at 1:4: syntax error, unexpected end of path, expecting '${'
```

For a `tree`, `at` names the first subtree that differs, as a path of
node tags and child indexes, for example `attrs[3]/bind[2]/call[1]/`.

1. Make the input small:
   `fuzz-parse -minimize_crash=1 -runs=10000 crash-0123abcd`.
2. Confirm it with the real Nix. Write the input to a file and run
   `nix-instantiate --parse FILE`. Do not use `-E` or standard input:
   there Nix finds lines only in the text before the first NUL byte
   (`Pos::getSource`, position.cc), so a position after a NUL is wrong.
3. Find the rule in the Nix source (`src/libexpr/parser.y`, `lexer.l`,
   `parser-state.hh`) and decide:
   - iets is wrong: fix it, add a test in `src/syntax/tests.rs`, and put
     the small input in `fuzz/regressions/parse/`, named by its sha1;
   - iets differs on purpose: add a section to
     `docs/reference/divergences.md` and an entry to
     `fuzz/divergences.toml`;
   - the oracle or `canon.rs` is wrong: fix it there.

## Known divergences

`fuzz/divergences.toml` lists the differences that are not bugs. A
difference that matches an entry is counted as expected and is not
added to the corpus, so the fuzzer does not find it again and again.

```toml
[[parse]]
id = "nul-outside-a-string"
doc = "docs/reference/divergences.md#a-nul-byte-outside-a-string"
input = "\u0000"
iets = "unexpected invalid token"
```

| Field | Matches when |
|:---|:---|
| `id` | (required) the name the reports count hits under |
| `doc` | (required) the section that explains the divergence |
| `kind` | the difference has this kind |
| `input` | the input contains these bytes |
| `nix` | Nix's side of the report contains this text |
| `iets` | iets's side of the report contains this text |
| `names_differ` | `true`: the first `'...'` of the two messages differ |

An entry matches when every field it gives matches. Keep entries narrow:
a broad entry hides bugs. `IETS_FUZZ_DIVERGENCES=FILE` reads another
list; otherwise the binaries carry the list they were built with.

## Run a long campaign

```sh
nix-farm '.#packages.x86_64-linux.fuzz-parse-campaign' --fetch-to campaign
```

The campaign minimises the nixpkgs seeds, fuzzes on every core of the
builder for an hour, and minimises the result again. Its output holds
`corpus/` (the grown corpus), `artifacts/` (every input that differed),
`triage.jsonl` (the artifacts replayed by `parse-diff --json`) and the
libFuzzer logs. `(import ./fuzz { ... }).campaign { seconds; corpus; }`
continues from an earlier corpus.

The grown corpus is not in the repository. The seeds come from the
pinned nixpkgs again on each run, so only the growth needs a home.

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

`cargo fuzz coverage` looks for `llvm-profdata` in the Rust sysroot,
where the flake's Rust has none, so the merge is a step of its own. The
LLVM major version must be the one `rustc -vV` names.

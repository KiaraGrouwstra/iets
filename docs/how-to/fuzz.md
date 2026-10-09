# How to fuzz iets against Nix

`fuzz/` is a differential fuzzer. It gives the same input to iets and to
Nix 2.34.8, compares the two answers, and reports each difference. This
page shows how to run it and how to read what it reports.

| Target | The input | Compared |
|:---|:---|:---|
| `parse` | Nix source | accept or refuse, the position of a refusal, the desugared tree |
| `nar-read` | a NAR | accept or refuse, bytes consumed, the tree written out again |
| `nar-gen` | bytes that choose a NAR token by token | as `nar-read` |
| `nar-tree` | bytes that choose a file tree on disk | the NAR of the tree, then as `nar-read` |
| `wire` | bytes that choose a daemon's side of a conversation | the answer of iets's daemon client and of Nix's to one operation |
| `eval` | bytes that choose an expression of a known type | the deeply forced value as `nix-instantiate --eval --strict` prints it, or an error on both sides |

Two oracle processes stay running while a target runs, so a comparison
costs a pipe round trip and not a process start.

The oracles, the regression cases and the grown corpora live in
[nix-conformance](https://github.com/numtide/nix-conformance), a flake
input: a test suite any Nix implementation can run. `parse-oracle` links
libnixexpr 2.34.8, `nar-oracle` links libnixutil 2.34.8 and `eval-oracle`
links libnixexpr 2.34.8 in pure mode with no store.

## The parse target

For each input, both parsers give one of two answers:

- the input parses, to a tree;
- the input is refused, at a line and a column.

`parse-oracle` prints Nix's tree. `fuzz/src/canon.rs` builds iets's tree
in the same form, with the desugaring of Nix's `parser.y`: `a - b` is a
call of `__sub`, `a.b = 1;` makes a nested set, `{ x } @ y` sorts its
formals. Both trees then pass through `sx::normalise`, which removes what
is not structure: the order Nix stores bindings in, where a lexer splits
a string into tokens, and iets folding `-1` into a literal.

It does not compare:

- the text of an error message. iets words some errors its own way;
- scope. Nix's parser also refuses an undefined variable. The oracle
  puts a `with` around the input to turn this off. iets finds undefined
  variables in `compile`, which the evaluator target will test;
- warnings, lints and doc comments.

Both sides keep one symbol table across inputs, for speed. Symbol order
decides which of several clashing names an error reports, and a file is
parsed in a fresh process. So an unknown difference is checked again
with a fresh oracle and a fresh table, and only that second answer
counts.

| Kind | Meaning |
|:---|:---|
| `nix-only` | Nix parses the input, iets refuses it |
| `iets-only` | iets parses the input, Nix refuses it |
| `position` | both refuse it, at different places |
| `tree` | both parse it, to different trees |
| `iets-panic` | the iets parser panicked |
| `nix-crash` | the oracle process died on the input |

## The NAR targets

`nar-read` gives the input to Nix's `parseDump`, to `restore_path` and to
`parse_length`. When the input is a NAR, Nix writes its memory tree out
again, and iets dumps the directory `restore_path` made. The two must be
the same bytes. Nix also dumps that directory, and iets's `nar_hash` and
`nar_size` of it must describe those bytes.

Random bytes are rarely a NAR, so `nar-gen` builds one from the input:
mostly well formed, with names and symlink targets of any bytes, entries
in any order, a wrong tag now and then, a cut at any point, and now and
then a chain of directories across the depth limit of 64.

`nar-tree` makes a tree on disk: files with any mode the owner can read,
files past the writer's 64 KiB buffer, symlinks, FIFOs, names in any bytes
a name may hold, and chains across the depth limit. Both sides dump it.

| Kind | Meaning |
|:---|:---|
| `nix-only` | Nix reads the NAR, `restore_path` refuses it |
| `iets-only` | `restore_path` reads the NAR, Nix refuses it |
| `parse-length` | `parse_length` and Nix disagree on accept or refuse |
| `consumed` | both read the NAR, after a different number of bytes |
| `tree` | both read the NAR, and the trees written out again differ |
| `dump` | the two dumps of one path differ, or only one fails |
| `hash` | `nar_hash` or `nar_size` does not describe the dump |
| `iets-panic` | iets panicked |
| `nix-crash` | the oracle process died on the input |

## The wire target

`wire` tests iets's client of the Nix daemon protocol. The input chooses
a script: the daemon's side of the handshake at protocol 1.35 to 1.40,
stderr frames (log lines, activities, results, errors) after the
handshake, after `SetOptions` and after the operation, then the reply of
one operation: `connect`, `path-info`, `valid-paths`, `substitute`,
`missing`, `output-map`, `substitutable`, `build`, `indirect-root`, `nar`
or `add-multiple`. Store paths, hashes, signatures, content addresses,
build results, realisations and NARs in the reply are wrong now and then,
and the script can stop at any byte.

A thread serves the script on a Unix socket. iets's `Connection` and
Nix 2.34.8's `RemoteStore` (`wire-oracle`) each connect and run the
operation, and their answers must be the same line (SPEC.md of
nix-conformance). `IETS_WIRE_SCRIPT=FILE` writes the script of the last
input, to serve it to a client by hand, for example with
`socat UNIX-LISTEN:SOCK,fork SYSTEM:'cat FILE'`.

The script never holds the replies that crash Nix's own client
(nix-bugs/daemon-client/ of nix-conformance), and never a protocol below
1.35, which iets refuses on purpose. For `add-multiple`, the daemon sends
its answer only after it has read the whole upload, as a real daemon
does: Nix's client aborts on an answer that comes first.

| Kind | Meaning |
|:---|:---|
| `nix-only` | Nix's client accepts the reply, iets's refuses it |
| `iets-only` | iets's client accepts the reply, Nix's refuses it |
| `answer` | both accept it, with different answers |
| `iets-panic` | iets panicked |
| `nix-crash` | the oracle process died on the reply |

## The eval target

`eval` tests the evaluator. The input chooses an expression of a known
type (`fuzz/src/eval.rs`):

- numbers with their edges: the largest and smallest integers, overflow,
  division by zero, floats that print in exponent form;
- strings with escapes, `''` strings and interpolation;
- lists, sets with `rec`, `inherit`, nested and dynamic names, `//`;
- lambdas with patterns, defaults and `@`; `let`, `with`, `if`,
  `assert`;
- `seq`, `deepSeq`, `tryEval`, and errors that are never forced;
- a set of pure builtins: arithmetic, comparison, list, set and string
  builtins, `toJSON`, `fromJSON`, `toXML`, `hashString`, `match`, `split`,
  `genericClosure`, `unsafeGetAttrPos` and others.

The expressions are simply typed and no binding refers to itself, so each
evaluation ends. Now and then a value of the wrong type takes the place of
a correct one, to compare the type errors.

iets evaluates the expression in the fuzzer's process, in pure mode.
`eval-oracle` evaluates it with Nix 2.34.8 in pure mode with no store.
Both force the value deeply. The two must print the same value, or both
must fail; error messages are not compared. `IETS_EVAL_SOURCE=FILE`
writes the expression of the last input.

`==` on two sets compares their attributes in each evaluator's symbol
order and stops at the first difference. Which of a different and a
failing attribute it reaches first is therefore not the same on both
sides. Each expression names the generator's field names first, in one
order, so that both sides reach the same attribute first. A difference
is checked again with a fresh oracle and, if it holds, a fresh evaluator,
as a fresh `nix-instantiate` and a fresh `iets eval` would see it.

| Kind | Meaning |
|:---|:---|
| `nix-only` | Nix gives a value, iets an error |
| `iets-only` | iets gives a value, Nix an error |
| `value` | both give a value, and the values differ |
| `iets-panic` | iets panicked |
| `nix-crash` | the oracle process died on the expression |

## Run the checks

```sh
nix build .#checks.x86_64-linux.fuzz-parse -L
nix build .#checks.x86_64-linux.fuzz-nar -L
nix build .#checks.x86_64-linux.fuzz-wire -L
nix build .#checks.x86_64-linux.fuzz-eval -L
nix build .#checks.x86_64-linux.conformance -L
```

`fuzz-parse` gives every `.nix` file of the pinned nixpkgs to both
parsers, then runs libFuzzer for 60 seconds from an eighth of those files
and the parse cases and corpus of nix-conformance. `fuzz-nar` runs each
NAR target for 20 seconds, `fuzz-wire` runs `wire` for 30 and
`fuzz-eval` runs `eval` for 60 seconds. A check fails on a
difference that `fuzz/divergences.toml` does
not list, and prints the input in base64. `conformance` runs every case
of nix-conformance through `conformance-adapter`, iets behind the suite's
adapter protocol; `fuzz/conformance-known` lists the cases iets fails on
purpose.

## Run a fuzzer by hand

The toolchain is the stable Rust of the flake. `cargo fuzz` runs on it
with `--sanitizer none`; AddressSanitizer would need nightly.

```sh
nix build .#fuzz-oracles -o oracles
nix build .#fuzzers -o fuzzers
export IETS_PARSE_ORACLE=$PWD/oracles/bin/parse-oracle
export IETS_NAR_ORACLE=$PWD/oracles/bin/nar-oracle
mkdir -p corpus && cp tests/lang/*.nix corpus/
fuzzers/bin/fuzz-parse -dict=fuzz/parse.dict -max_len=4096 corpus
fuzzers/bin/fuzz-nar-gen -max_len=4096 nar-corpus
```

`fuzz/default.nix` holds the flags of each target. Add `-fork=N
-ignore_crashes=1` to use N cores and keep going after a difference.
Each difference is then a file `crash-<sha1>`. While you change iets,
`cargo fuzz build -s none -O TARGET` in `fuzz/` builds one target.

## Replay inputs

`fuzz-diff` gives files to a target without libFuzzer: a crash artifact,
a corpus, or a tree of `.nix` files.

```sh
nix build .#fuzz-diff -o fuzz-diff
fuzz-diff/bin/fuzz-diff parse crash-0123abcd
fuzz-diff/bin/fuzz-diff --nix-only parse ~/src/nixpkgs
fuzz-diff/bin/fuzz-diff --json nar-gen corpus/ > triage.jsonl
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

For a parse `tree`, `at` names the first subtree that differs, as a path
of node tags and child indexes, for example `attrs[3]/bind[2]/call[1]/`.
A NAR is shown as its length and its first 64 bytes in hexadecimal.

1. Make the input small:
   `fuzz-parse -minimize_crash=1 -runs=10000 crash-0123abcd`.
2. Confirm it with the real Nix. For the parser, write the input to a
   file and run `nix-instantiate --parse FILE`. Do not use `-E` or
   standard input: there Nix finds lines only in the text before the
   first NUL byte (`Pos::getSource`, position.cc). For a NAR, use
   `nix-store --restore DIR < FILE` and `nix-store --dump DIR`.
3. Find the rule in the Nix source (`src/libexpr/parser.y`, `lexer.l`,
   `parser-state.hh`, `src/libutil/archive.cc`) and decide:
   - iets is wrong: fix it, add a test next to the code, and put the
     small input in `parse/` or `nar/` of nix-conformance, named by its
     sha1, with the answer `./expect` writes;
   - iets differs on purpose: add a section to
     `docs/reference/divergences.md` and an entry to
     `fuzz/divergences.toml`;
   - the oracle or the target code is wrong: fix it there.

## Known divergences

`fuzz/divergences.toml` lists the differences that are not bugs, in one
array of tables per target family: `[[parse]]`, `[[nar]]` and `[[eval]]`. A
difference that matches an entry is counted as expected and is not added
to the corpus, so the fuzzer does not find it again and again.

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
| `input` | the input contains these bytes (for `eval`, the generated expression) |
| `nix` | Nix's side of the report contains this text |
| `iets` | iets's side of the report contains this text |
| `names_differ` | `true`: the first `'...'` of the two messages differ |
| `float_digits` | `true`: the two sides differ only in the digits of numbers, and each such pair reads back to the same double |

An entry matches when every field it gives matches. Keep entries narrow:
a broad entry hides bugs. `IETS_FUZZ_DIVERGENCES=FILE` reads another
list; otherwise the binaries carry the list they were built with.

## Run a long campaign

```sh
nix-farm '.#packages.x86_64-linux.fuzz-parse-campaign' --fetch-to campaign
nix-farm '.#packages.x86_64-linux.fuzz-nar-campaign' --fetch-to campaign
nix-farm '.#packages.x86_64-linux.fuzz-eval-campaign' --fetch-to campaign
```

A campaign minimises its seeds, fuzzes on every core of the builder for
an hour, and minimises the result again. Its output holds `corpus/` (the
grown corpus), `artifacts/` (every input that differed), `triage.jsonl`
(the artifacts replayed by `fuzz-diff --json`) and the libFuzzer logs.
`(import ./fuzz { ... }).campaign { target; seconds; corpus; }` runs
another target, for another time, or from an earlier corpus.

The grown corpora are not in the repository. The parse seeds come from
the pinned nixpkgs again on each run, so only the growth needs a home.

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
LLVM major version must be the one `rustc -vV` names. For several
targets, merge their `raw` directories into one profile and give
`llvm-cov` one binary and the others with `-object`.

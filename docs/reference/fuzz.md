# Fuzz targets

What each fuzz target compares, the kinds of difference it reports, and
the format of the list of known divergences. To run them, see
[how-to/fuzz.md](../how-to/fuzz.md).

`fuzz/` is a differential fuzzer. It gives the same input to iets and to
Nix 2.34.8, compares the two answers, and reports each difference.

| Target | The input | Compared |
|:---|:---|:---|
| `parse` | Nix source | accept or refuse, the position of a refusal, the desugared tree |
| `nar-read` | a NAR | accept or refuse, bytes consumed, the tree written out again |
| `nar-gen` | bytes that choose a NAR token by token | as `nar-read` |
| `nar-tree` | bytes that choose a file tree on disk | the NAR of the tree, then as `nar-read` |
| `wire` | bytes that choose a daemon's side of a conversation | the answer of iets's daemon client and of Nix's to one operation |
| `eval` | bytes that choose an expression of a known type | the deeply forced value as `nix-instantiate --eval --strict` prints it, or an error on both sides |
| `drv-read` | a `.drv` file | accept or refuse, the ATerm written again, the drvPath |
| `drv-gen` | bytes that choose a `.drv` part by part | as `drv-read` |

## Oracles

Nix's side of each comparison is an oracle process. Two stay running while
a target runs, so a comparison costs a pipe round trip, not a process
start.

The oracles, the regression cases and the grown corpora live in
[nix-conformance](https://github.com/numtide/nix-conformance), a flake
input. It is a test suite any Nix implementation can run.

| Oracle | Links | Mode |
|:---|:---|:---|
| `parse-oracle` | libnixexpr 2.34.8 | |
| `nar-oracle` | libnixutil 2.34.8 | |
| `eval-oracle` | libnixexpr 2.34.8 | pure, no store |
| `wire-oracle` | Nix 2.34.8's `RemoteStore` | |
| `drv-oracle` | libnixstore 2.34.8 | |

## parse

For each input, both parsers either accept it, giving a tree, or refuse it
at a line and a column.

iets's tree is built in Nix's form, with the desugaring of Nix's
`parser.y`: `a - b` is a call of `__sub`, `a.b = 1;` makes a nested set,
`{ x } @ y` sorts its formals. Both trees are then normalised to drop what
is not structure: the order Nix stores bindings in, where a lexer splits a
string into tokens, and iets folding `-1` into a literal.

Not compared:

- the text of an error message. iets words some errors its own way;
- scope. Nix's parser also refuses an undefined variable. The oracle puts a
  `with` around the input to turn this off. iets finds undefined variables
  after parsing;
- warnings, lints and doc comments.

Both sides keep one symbol table across inputs, for speed. Symbol order
decides which of several clashing names an error reports, and a file is
normally parsed in a fresh process. So an unknown difference is checked
again with a fresh oracle and a fresh table, and only that second answer
counts.

| Kind | Meaning |
|:---|:---|
| `nix-only` | Nix parses the input, iets refuses it |
| `iets-only` | iets parses the input, Nix refuses it |
| `position` | both refuse it, at different places |
| `tree` | both parse it, to different trees |
| `iets-panic` | the iets parser panicked |
| `nix-crash` | the oracle process died on the input |

For a `tree`, `at` names the first subtree that differs, as a path of node
tags and child indexes, for example `attrs[3]/bind[2]/call[1]/`.

## nar-read, nar-gen, nar-tree

`nar-read` gives the input to Nix's `parseDump`, to `restore_path` and to
`parse_length`. When the input is a NAR, Nix writes its memory tree out
again, and iets dumps the directory `restore_path` made. The two must be
the same bytes. Nix also dumps that directory, and iets's `nar_hash` and
`nar_size` of it must describe those bytes.

Random bytes are rarely a NAR, so `nar-gen` builds one from the input:
mostly well formed, with names and symlink targets of any bytes, entries in
any order, a wrong tag now and then, a cut at any point, and now and then a
chain of directories across the depth limit of 64.

`nar-tree` makes a tree on disk: files with any mode the owner can read,
files past the writer's 64 KiB buffer, symlinks, FIFOs, names in any bytes
a name may hold, and chains across the depth limit. Both sides dump it.

A NAR is shown as its length and its first 64 bytes in hexadecimal.

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

## wire

`wire` tests iets's client of the Nix daemon protocol. The input chooses a
script: the daemon's side of the handshake at protocol 1.35 to 1.40, stderr
frames (log lines, activities, results, errors) after the handshake, after
`SetOptions` and after the operation, then the reply of one operation:
`connect`, `path-info`, `valid-paths`, `substitute`, `missing`,
`output-map`, `substitutable`, `build`, `indirect-root`, `nar` or
`add-multiple`. Store paths, hashes, signatures, content addresses, build
results, realisations and NARs in the reply are wrong now and then, and the
script can stop at any byte.

A thread serves the script on a Unix socket. iets's client and Nix
2.34.8's `RemoteStore` each connect and run the operation, and their
answers must be the same line (SPEC.md of nix-conformance).

The script never holds the replies that crash Nix's own client
(nix-bugs/daemon-client/ of nix-conformance), and never a protocol below
1.35, which iets refuses on purpose. For `add-multiple`, the daemon sends
its answer only after it has read the whole upload, as a real daemon does:
Nix's client aborts on an answer that comes first.

| Kind | Meaning |
|:---|:---|
| `nix-only` | Nix's client accepts the reply, iets's refuses it |
| `iets-only` | iets's client accepts the reply, Nix's refuses it |
| `answer` | both accept it, with different answers |
| `iets-panic` | iets panicked |
| `nix-crash` | the oracle process died on the reply |

## eval

`eval` tests the evaluator. The input chooses an expression of a known
type:

- numbers with their edges: the largest and smallest integers, overflow,
  division by zero, floats that print in exponent form;
- strings with escapes, `''` strings and interpolation;
- lists, sets with `rec`, `inherit`, nested and dynamic names, `//`;
- lambdas with patterns, defaults and `@`; `let`, `with`, `if`, `assert`;
- `seq`, `deepSeq`, `tryEval`, and errors that are never forced;
- a set of pure builtins: arithmetic, comparison, list, set and string
  builtins, `toJSON`, `fromJSON`, `toXML`, `hashString`, `match`, `split`,
  `genericClosure`, `unsafeGetAttrPos` and others.

The expressions are simply typed and no binding refers to itself, so each
evaluation ends. Now and then a value of the wrong type takes the place of
a correct one, to compare the type errors.

iets evaluates the expression in the fuzzer's process, in pure mode.
`eval-oracle` evaluates it with Nix 2.34.8 in pure mode with no store. Both
force the value deeply. The two must print the same value, or both must
fail. Error messages are not compared.

`==` on two sets compares their attributes in each evaluator's symbol
order and stops at the first difference. So which of a different and a
failing attribute it reaches first is not the same on both sides. Each
expression names the generator's field names first, in one order, so that
both sides reach the same attribute first. A difference is checked again
with a fresh oracle and, if it holds, a fresh evaluator, as a fresh
`nix-instantiate` and a fresh `iets eval` would see it.

| Kind | Meaning |
|:---|:---|
| `nix-only` | Nix gives a value, iets an error |
| `iets-only` | iets gives a value, Nix an error |
| `value` | both give a value, and the values differ |
| `iets-panic` | iets panicked |
| `nix-crash` | the oracle process died on the expression |

## drv-read, drv-gen

`drv-read` gives a `.drv` file to iets's parser and to Nix's
`parseDerivation`, both with the name `x`. The two must agree on accept or
refuse. When both accept, each side writes the ATerm again, and the bytes
and the drvPath must be the same. The seeds are the real files in `drv/` of
nix-conformance; `fuzz/drv.dict` holds the ATerm tokens.

`drv-gen` makes the `.drv` from the input: outputs of each kind (input
addressed, fixed, floating, impure, deferred), input derivations (with
child outputs in the `DrvWithVersion` form), sources, `system`, `builder`,
arguments and `env`. Paths, algorithms, hashes, orders and escapes are
wrong now and then, and the file can be cut short or have one byte
changed.

| Kind | Meaning |
|:---|:---|
| `nix-only` | Nix reads the file, iets refuses it |
| `iets-only` | iets reads the file, Nix refuses it |
| `aterm-FIELD` | both read it and write different ATerms. Read back, the two derivations first differ in FIELD: `outputs`, `inputs`, `sources`, `system`, `builder`, `args`, `env` or `json` (`__json`). `at` is the first byte that differs |
| `aterm` | as `aterm-FIELD`, but an ATerm does not read back, or the fields are the same |
| `path` | the ATerms are the same, the drvPaths are not |
| `iets-panic` | iets panicked |
| `nix-crash` | the oracle process died on the input |

## Environment variables

| Variable | Effect |
|:---|:---|
| `IETS_PARSE_ORACLE`, `IETS_NAR_ORACLE` | the oracle binary a fuzzer runs by hand |
| `IETS_WIRE_SCRIPT=FILE` | `wire` writes the script of the last input to `FILE` |
| `IETS_EVAL_SOURCE=FILE` | `eval` writes the expression of the last input to `FILE` |
| `IETS_DRV_WRITE=FILE` | the `drv-*` targets write the `.drv` of the last input to `FILE` |
| `IETS_FUZZ_DIVERGENCES=FILE` | read this list of known divergences instead of the one built in |

## `fuzz-diff`

`fuzz-diff [--json] [--nix-only] TARGET PATH...` gives files to a target
without libFuzzer: a crash artifact, a corpus, or a tree of `.nix` files.

| Exit code | Meaning |
|:---|:---|
| 0 | no difference is unexpected |
| 1 | at least one difference is unexpected |
| 2 | the run itself failed (no oracle, a bad divergence list) |

With `--json`, each unexpected difference is one line
`{"file", "kind", "at", "nix", "iets"}`, and the last line is
`{"summary": {"inputs", "same", "expected", "bugs", "seconds"}}`.
`expected` counts the hits of each known divergence.

## `fuzz/divergences.toml`

The differences that are not bugs. It has one array of tables per target
family: `[[parse]]`, `[[nar]]` for the `nar-*` targets, `[[wire]]`,
`[[eval]]`, and `[[drv]]` for the `drv-*` targets. A difference that
matches an entry is counted as expected and is not added to the corpus.

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

An entry matches when every field it gives matches.

## Checks

| Check | What it runs |
|:---|:---|
| `fuzz-parse` | every `.nix` file of the pinned nixpkgs through both parsers, then libFuzzer for 60 s from an eighth of those files and the parse cases and corpus of nix-conformance |
| `fuzz-nar` | each NAR target for 20 s |
| `fuzz-wire` | `wire` for 30 s |
| `fuzz-eval` | `eval` for 60 s |
| `fuzz-drv` | each derivation target for 20 s |
| `conformance` | every case of nix-conformance through `conformance-adapter`; `fuzz/conformance-known` lists the cases iets fails on purpose |

A fuzz check fails on a difference that `fuzz/divergences.toml` does not
list, and prints the input in base64.

## Campaigns

A campaign (`fuzz-parse-campaign`, `fuzz-nar-campaign`,
`fuzz-eval-campaign`) minimises its seeds, fuzzes on every core of the
builder for an hour, and minimises the result again. Its output holds:

| Path | Content |
|:---|:---|
| `corpus/` | the grown corpus |
| `artifacts/` | every input that differed |
| `triage.jsonl` | the artifacts replayed by `fuzz-diff --json` |
| the libFuzzer logs | |

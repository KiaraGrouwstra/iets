# Known divergences from Nix 2.34

Places where ietsp knowingly differs from `nix-instantiate`. Each entry
says what differs, why, and how to observe it.

`scripts/parity.nu` runs the differential corpora under `tests/parity/`
(272 regex pattern/input pairs through both `builtins.match` and
`builtins.split`, 46 patterns both sides must reject, 77 TOML documents
and 46 rejected ones) against the real `nix-instantiate` and requires
byte-equal `--strict --json` output. It needs a nix binary, so it is not
a flake check; run it by hand in the dev shell. `NIX=` and `IETSP=`
override either side.

## Regular expressions (`builtins.match`, `builtins.split`)

Nix compiles patterns with libstdc++ `std::regex` in `extended` (POSIX ERE)
mode. ietsp has its own engine (`src/builtins/text/regex.rs`), written from
the behaviour Nix shows and the POSIX spec. Its header lists the observed
rules. No divergence is known: the matches, the capture groups, the
accept/reject set and both error messages agree, including the
`memory limit exceeded by regular expression` limit (`a{99993}` compiles,
`a{99994}` does not).

## `builtins.fromTOML`

Nix uses toml11 with `toml::spec::v(1, 0, 0)`; ietsp uses the `toml`
crate, which implements TOML 1.1 and has no switch for the older spec. A
scanning pass (`reject_1_1` in `src/builtins/text/fromtoml.rs`) refuses
the 1.1-only syntax first: a newline or a trailing comma inside an inline
table, and the `\e` and `\xHH` escapes. Omitted seconds are caught on the
parsed value, which the crate reports as `second: None`. Only the error
messages differ. Datetime rendering under `parse-toml-timestamps` follows
Nix (`T` separator, 3/6/9 fractional digits).

## `builtins.fromJSON`

Errors carry a message shaped like nlohmann's (`parse error at line L,
column C: ...`) but the `last read` detail is not reproduced.

## `builtins.genericClosure`

The key set is a sorted vector with binary search instead of a
`std::map`, so which pair of elements triggers a `cannot compare` error
(and the error's trace lines) can differ. The result list and its order
match.

## `builtins.break`

Nix returns a copy of the unforced argument thunk; inside a list,
`nix-instantiate --eval --strict` then prints that element as `<CODE>`
(`[ (builtins.break { a = 2; }) ]` gives `[ <CODE> ]`). ietsp forces the
argument and prints the value. Selecting from the result behaves the same
in both.

## Unforced values in lazy positions

In a list element, an attribute value, a `let` binding or a function
argument, Nix makes a thunk of every expression except a literal and an
already evaluated variable (`Expr::maybeThunk`). ietsp keeps a value there
instead of a thunk when it costs nothing to have it: a lambda, a folded
`builtins.x`, an attribute of an attrset that is already forced, and `+`
or `-` on ints that are already forced (`Evaluator::maybe_thunk`,
`Compiler::fold_builtins_select`). The results are the same; only a
printer that stops at thunks can see it. `nix-instantiate --eval` without
`--strict` prints `[ builtins.nixPath (1 + 1) (x: x) ]` as
`[ <CODE> <CODE> <CODE> ]`, ietsp as `[ [ ] 2 <LAMBDA> ]`. These are
speed shortcuts, kept on purpose; `--strict`, `--json` and `nix eval`
print the same.

`==` can see them too. Nix's `eqValues` forces both sides, then takes two
elements from one slot as equal without looking inside, and a function
equals only its own slot; the two operands of `==` are fresh copies, so
that shortcut never applies to them. ietsp keeps its faster rule, equal
forced words are equal, at every level, and makes one exception: a
function is equal only when the words before forcing are the same, so
`[ (id id) ] == [ (id id) ]` and `with foo; [ bar ] == [ bar ]` are false,
as in Nix. What stays different: `let x = { a = throw "a"; }; in x == x`
is true where Nix throws, and a function reached through a shortcut above
still equals itself (`[ builtins.add ] == [ builtins.add ]`, `{ inherit
(builtins) import; } == { inherit (builtins) import; }`, `builtins.elem
builtins.map [ builtins.map ]`). Forcing a cell writes its value over the
thunk, so two thunks that give the same function and are both forced in
place look like one slot. Nix's exact rules cost about 1% of the
instructions of a NixOS evaluation, for no difference in nixpkgs.

## Experimental-feature builtins

Nix leaves `fetchTree` out of `builtins` unless `fetch-tree` or `flakes`
is on, `fetchClosure` unless `fetch-closure` is, and `outputOf` unless
`dynamic-derivations` is (`experimental-features` in nix.conf or
`--extra-experimental-features`). ietsp always has all three, so
`builtins ? fetchTree` is true under ietsp where nix, with no feature
on, says false; code that branches on it (flake-compat) takes the
`fetchTree` branch. `builtins.fetchClosure` and `builtins.outputOf` are
unsupported either way, so classic mode falls back when they are called.

## Classic mode

`nix-instantiate`, `nix-build` and `nix-eval-jobs` under Nix's own names
diverge in a few more places (store `auto`, `-Q`, `--show-trace`,
`--no-out-link`, `getDerivations` recursion, `--add-root` clobbering).
They are listed in `docs/classic.md`.

## Errors, exit codes and `--log-format internal-json`

`docs/cli.md` has the codes and the record format. Where it differs from
Nix 2.34:

- **Build exit codes.** `ietsp build` exits 100, 101, 102 or 104 on a
  failed build (`failingExitStatus`) through a daemon, where Nix exits 1.
  Nix computes these codes only when it builds in its own store. The
  classic names exit 1, as Nix does as a daemon client.
- **Usage errors of native commands** exit 2. Nix exits 1; under a
  classic name ietsp does too.
- **`msg`** holds ietsp's plain text: no ANSI colours, no source
  excerpt. `raw_msg`, `line`, `column` and `file` are the fields to
  compare; they differ from Nix where the text differs. An error that
  Nix reports without a position and with the position in its trace,
  such as `throw`, has the position here.
- **`code` and `hint`** are extra keys; Nix has neither.
- **A failed build's record** has the position of the derivation; Nix's
  has none.
- **eval-jobs** puts out a record for each failed job; nix-eval-jobs
  prints those errors as plain text even under `internal-json`.
- **Activity records** (`start`, `stop`, `result`, docs/cli.md) have
  Nix's shape, and nix-output-monitor reads them. The daemon's
  activities during a build are the same as in Nix: realise, builds,
  copy paths, one build activity per derivation, and the same result
  types. `scripts/classic-e2e.nu` compares that sequence with Nix 2.34.8
  for `nix-build` and `nix build`. What differs:
  - Ids are new ids of the ietsp process; Nix passes the daemon's ids on.
  - Nix sends `QueryMissing` before a build. So Nix has a `querying info
    about missing paths` activity and the substituter queries first,
    then `these N derivations will be built:`. ietsp does not send it:
    the queries come inside the build op, and no plan text is printed.
    `ietsp build --dry-run` prints the plan.
  - Nix writes the `.drv` files during evaluation, with no activity.
    ietsp uploads them before the build, under `copying N paths` and
    `copying path` activities, as `nix copy` does. Nix's copy activities
    also report the bytes of each path; ietsp's report the paths only.
  - Under `internal-json` the build renderer's lines (start, phase, log
    block, verdict, heartbeat) are not put out; the records carry them.
  - With a `grpc://` builder ietsp makes the records itself. There is
    one build activity per derivation from the start of its scheduling,
    and a derivation the cache already has also gets one. Its uploads
    have no copy activities.
  - The `--timings` and `IETSP_STATS` lines stay plain text.
- **`ietsp build --dry-run --json`** prints the plan (`willBuild`,
  `willSubstitute`, `unknown`, `downloadSize`, `narSize`). `nix build
  --dry-run --json` prints the installables' derived paths. The text on
  stderr is the same as Nix's.
- **`json-log-path`** applies to every command. Nix applies it to the
  `nix` command only: the classic names return before `applyJSONLogger`
  (`src/nix/main.cc`).

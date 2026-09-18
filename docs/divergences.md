# Known divergences from Nix 2.34

Places where ietsp knowingly differs from `nix-instantiate`. Each entry
says what differs, why, and how to observe it.

`scripts/parity.nu` runs the differential corpora under `tests/parity/`
(184 regex pattern/input pairs through both `builtins.match` and
`builtins.split`, 24 patterns both sides must reject, 77 TOML documents
and 46 rejected ones) against the real `nix-instantiate` and requires
byte-equal `--strict --json` output. It needs a nix binary, so it is not
a flake check; run it by hand in the dev shell. `NIX=` and `IETSP=`
override either side.

## Regular expressions (`builtins.match`, `builtins.split`)

Nix compiles patterns with libstdc++ `std::regex` in `extended` (POSIX ERE)
mode. ietsp builds the same NFA (`src/builtins/text/ere.rs`) and walks it
with the same depth-first executor (`src/builtins/text/posix.rs`), so the
matches, the capture groups and the accept/reject set agree. Only the
error message differs:

- **Error messages.** ietsp always reports `invalid regular expression
  '<re>'`; Nix says `memory limit exceeded by regular expression` when the
  NFA passes libstdc++'s 100000-state limit, which is where ietsp stops
  too (`a{40000}` compiles, `a{99999}` does not).

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

## Classic mode

`nix-instantiate`, `nix-build` and `nix-eval-jobs` under Nix's own names
diverge in a few more places (store `auto`, `-Q`, `--show-trace`,
`--no-out-link`, `getDerivations` recursion, `--add-root` clobbering).
They are listed in `docs/classic.md`.

# Known divergences from Nix 2.34

Places where ietsp knowingly differs from `nix-instantiate`. Each entry
says what differs, why, and how to observe it.

## Regular expressions (`builtins.match`, `builtins.split`)

Nix compiles patterns with libstdc++ `std::regex` in `extended` (POSIX ERE)
mode. ietsp translates the ERE into the `regex` crate's syntax
(`src/builtins/text/ere.rs`) and runs it as a byte-oriented automaton.

- **Alternation is leftmost-first, not leftmost-longest.** POSIX picks the
  longest match at the leftmost position and the longest subexpression
  matches; the `regex` crate prefers the earliest alternative. Whole-string
  `match` is unaffected in the overall match (the pattern is anchored with
  `\A(?:...)\z`), but capture groups can differ when an earlier alternative
  also allows a full match. `split` can differ in match length: Nix gives
  `builtins.split "x*|ab" "ab"` the match `ab`; ietsp matches the empty
  `x*` first.
- **Empty iterations of a starred group.** `builtins.match "(a*)*" "aa"`
  is `[ "" ]` in Nix (the last iteration of the group matched empty) and
  `[ "aa" ]` in ietsp (the automaton does not run an empty iteration).
- **Accepted-only-by-ietsp patterns.** The translator rejects what
  libstdc++ rejects (`\w`, `\d`, `\n`, `\/`, `a{,2}`, `*a`, backreferences,
  unknown `[:class:]`), so error behaviour matches for those. Patterns
  that the `regex` crate refuses after translation (very large repetition
  counts, deeply nested groups beyond its nest limit) also report
  `invalid regular expression`.
- **Error messages.** ietsp always reports `invalid regular expression
  '<re>'`; Nix distinguishes `memory limit exceeded by regular expression`.

Everything else (bracket expressions with a literal backslash, `.`
matching newline and single bytes, ASCII-only classes, stacked
quantifiers such as `a+?` meaning `(a+)?`, `std::regex_iterator` empty
match stepping) is reproduced.

## `builtins.fromTOML`

Nix uses toml11 with `toml::spec::v(1, 0, 0)`; ietsp uses the `toml` crate,
which implements TOML 1.1. Documents that are valid only under 1.1
(newlines inside inline tables, trailing commas in inline tables, `\e`
and `\xHH` string escapes, seconds omitted from times) parse in ietsp and
fail in Nix. Error messages differ. Datetime rendering under
`parse-toml-timestamps` follows Nix (`T` separator, 3/6/9 fractional
digits).

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

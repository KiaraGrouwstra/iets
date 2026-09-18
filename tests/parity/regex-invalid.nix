# EREs that libstdc++ rejects, so both nix-instantiate and ietsp must fail.
# builtins.tryEval cannot catch "invalid regular expression", so scripts/parity.nu
# runs one process per entry and compares the exit status.
[
  "\\n"
  "\\w"
  "\\d"
  "\\/"
  "\\b"
  "a{,2}"
  "*a"
  "+a"
  "?a"
  "{2}"
  "(a"
  "a)"
  "(a|"
  "[z-a]"
  "[[:foo:]]"
  "[a-c"
  "[[:alpha:]-z]"
  "(?:a)"
  "a{2,1}"
  "a{99999}"
  "(1)\\1"
  "a\\"
  "[["
  "a{"
]

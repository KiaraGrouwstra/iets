with builtins;
[
  (split "foobar" "foobar")
  (split "fo+" "f")
  (split "(fo{1,2})" "fooo")
  (split "(o+)" "oooofoooo")
  (split "(b)" "foobarbaz")
  (split "a*" "baaab")
  (split "(o*)" "f")
  (split "x*" "")
  (split "" "ab")
  (split "b*" "abbbc")
  (split "(a)|(c)" "abc")
  (split "[[:space:]]+|([',.!?])" "Nix Rocks!\nThat's why I use it.\n")
  (split "([[:upper:]]+)" "  FOO   ")
  (split "\\." "a.b.c")
  (split "-" "-a-")
]

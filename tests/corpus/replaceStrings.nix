with builtins;
[
  (replaceStrings [ "o" ] [ "a" ] "foobar")
  (replaceStrings [ "o" ] [ "" ] "foobar")
  (replaceStrings [ "oo" "a" ] [ "a" "oo" ] "foobar")
  (replaceStrings [ "" ] [ "X" ] "abc")
  (replaceStrings [ "" ] [ "X" ] "")
  (replaceStrings [ "ab" "a" ] [ "1" "2" ] "aab")
  (replaceStrings [ "a" "ab" ] [ "1" "2" ] "aab")
  (replaceStrings [ ] [ ] "ab")
  (replaceStrings [ "oo" "XX" ] [ "u" (throw "unreachable") ] "foobar")
  (replaceStrings [ "\n" "\t" ] [ "\\n" "\\t" ] "a\nb\tc")
]

with builtins;
[
  (fromJSON ''[9223372036854775807, -9223372036854775808, -9223372036854775809, 18446744073709551616, 1e2, 1.0, {"a":1,"a":2}, " 1 ", "😀", -0, -0.0, "é", "\/", 5e-324, 1e-400]'')
  (fromJSON ''{"Video": {"Title": "The Penguin Chronicles", "Data": [3.14159, 23493, null, true, false, -10], "Escapes": "\"\\\/\b\f\n\r\t"}}'')
  (fromJSON " \n [ ] ")
  (fromJSON "{}")
  (fromJSON ''"quote \" reverse solidus \\ solidus \/ backspace \b formfeed \f newline \n carriage return \r horizontal tab \t   é š → 😀"'')
  (fromJSON ''{"b": 1, "a": 2, "10": 3, "": 4}'')
]

let f = ../fixtures/paths/hello.txt; in
[
  (builtins.readFile f)
  (builtins.readFile (toString f))
  (builtins.readFile ../fixtures/paths/dir/link)
  (builtins.readFile { __toString = _: toString f; })
  (builtins.readFile { outPath = toString f; })
  (builtins.hasContext (builtins.readFile f))
  (builtins.readFile (../fixtures/paths/dir + "/sub/../a.txt"))
]

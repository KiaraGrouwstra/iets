let d = ../fixtures/paths/dir; in
[
  (builtins.typeOf (builtins.toPath d))
  (builtins.toPath d)
  (builtins.toPath (toString d + "/sub/../a.txt"))
  (builtins.toPath "/a//b/./c/../d/")
  (builtins.toPath { __toString = _: d; })
  (builtins.toPath { outPath = toString d; })
  (builtins.hasContext (builtins.toPath "${d}"))
]

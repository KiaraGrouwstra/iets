[
  (builtins.readDir ../fixtures/paths/dir)
  (builtins.readDir ../fixtures/paths/dir/dirlink)
  (builtins.readDir (toString ../fixtures/paths/dir + "/sub"))
  (builtins.readDir ../fixtures/paths/dir/sub/..)
]

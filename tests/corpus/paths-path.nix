let d = ../fixtures/paths/dir; in
[
  "${d}"
  "${../fixtures/paths/hello.txt}"
  "${d}/sub"
  (builtins.getContext "${d}")
  (builtins.path { path = d; })
  (builtins.path { path = d; name = "renamed"; })
  (builtins.path { path = d; recursive = true; })
  (builtins.path { path = ../fixtures/paths/hello.txt; recursive = false; })
  (builtins.path { path = ../fixtures/paths/hello.txt; recursive = false; sha256 = "a948904f2f0f479b8f8197694b30184b0d2ed1c1cd2a1ec0fb85d299a192a447"; })
  (builtins.path { path = toString d; name = "via-string"; })
  (builtins.path { path = d; filter = p: t: t != "symlink"; })
  (builtins.path { path = d; name = "only-files"; filter = p: t: t == "regular"; })
  (builtins.filterSource (p: t: t != "symlink") d)
  (builtins.filterSource (p: t: baseNameOf p != "sub") d)
  (builtins.getContext (builtins.filterSource (p: t: true) d))
  (builtins.path { path = d; filter = p: t: builtins.trace "${p} ${t}" true; sha256 = "sha256-HXp+LDFzK7lpHJ3qkE6gccf8eYQYlxPyfWN/waLXMiE="; name = "hashed"; })
  (builtins.path { path = ../fixtures/paths/dir/link; name = "link"; })
  (builtins.path { path = ../fixtures/paths/dir/dirlink; name = "dirlink"; })
]

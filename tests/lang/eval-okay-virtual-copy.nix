let
  d = "${./dir2}";
in
[
  (import (builtins.findFile [ { prefix = "v"; path = d; } ] "v/a.nix"))
  (builtins.storePath "${d}/b.nix")
  (builtins.removeAttrs (builtins.fetchTree { type = "path"; path = d; }) [ "outPath" ])
  (import "${builtins.fetchTree { type = "path"; path = d; }}/a.nix")
]

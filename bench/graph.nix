# A layered DAG of cheap derivations for scheduling benchmarks.
#
#   nix-instantiate bench/graph.nix --arg width 8 --arg depth 6
#
# Layer 0 holds `width` leaves. Each node of layer l depends on two nodes of
# layer l-1 (its own column and the next one), so every layer is a full
# antichain and the DAG is `depth` layers deep. A root depends on the whole
# top layer. `cost` is the number of shell loop iterations per build: about
# 40000 takes 100 ms in bash on a 2024 x86 core. `salt` changes every drv
# hash so a warm store can be made cold without deleting anything.
{ width ? 8, depth ? 6, cost ? 40000, salt ? "" }:
let
  system = builtins.currentSystem;
  node = name: inputs: derivation {
    inherit name system;
    builder = "/bin/sh";
    args = [ "-c" ''
      i=0; while [ $i -lt ${toString cost} ]; do i=$((i+1)); done
      { echo ${name} ${salt}; for f in $inputs; do while read -r l; do echo "$l"; done < "$f"; done; } > $out
    '' ];
    inputs = inputs;
    inherit salt;
  };
  layer = l:
    if l == 0 then builtins.genList (c: node "g${toString c}-0" []) width
    else
      let prev = layer (l - 1);
      in builtins.genList (c: node "g${toString c}-${toString l}"
        (if width == 1 then [ (builtins.elemAt prev 0) ]
         else [ (builtins.elemAt prev c) (builtins.elemAt prev (if c + 1 == width then 0 else c + 1)) ])) width;
in
node "root" (layer (depth - 1))

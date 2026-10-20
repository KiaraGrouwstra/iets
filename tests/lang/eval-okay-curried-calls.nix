let
  f = a: b: c: [ a b c ];
  g = { x, y ? x + 1 }: { z ? y }: w: [ x y z w ];
  h = a@{ n, ... }: b: a // { inherit b; };
  shadow = x: x: x;
  partial = f 1;
  over = a: { __functor = self: b: a + b; };
  mk = a: b: c: d: a + b + c + d;
in
[
  (f 1 2 3)
  (partial 2 3)
  (map (f 1 2) [ 3 4 ])
  (g { x = 1; } { } 2)
  (g { x = 1; y = 5; } { z = 0; } 2)
  (h { n = 1; m = 2; } 3)
  (shadow 1 2)
  ((over 1) 2)
  (over 1 2)
  (mk 1 2 3 4)
  (builtins.foldl' (f: x: f x) mk [ 1 2 3 4 ])
  (let k = a: b: builtins.seq a b; in k 1 2)
]

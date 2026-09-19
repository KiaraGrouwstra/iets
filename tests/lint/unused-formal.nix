let f = { a, b }: a; g = { c, d, ... }@args: c + args.d; h = { _e }: 1; in f { a = 1; b = 2; } + g { c = 1; d = 2; } + h { _e = 3; }

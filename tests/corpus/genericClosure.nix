with builtins;
[
  (genericClosure { startSet = [ { key = 5; } ]; operator = item: [ { key = if (item.key / 2) * 2 == item.key then item.key / 2 else 3 * item.key + 1; } ]; })
  (genericClosure { startSet = [ { key = 1; } { key = 1.0; } { key = 2; } ]; operator = x: [ ]; })
  (genericClosure { startSet = [ { key = [ 1 ]; } { key = [ 1 2 ]; } { key = [ 1 ]; } ]; operator = x: [ ]; })
  (genericClosure { startSet = [ { key = /a; } { key = /b; } ]; operator = x: [ ]; })
  (genericClosure { startSet = [ ]; })
  (genericClosure { startSet = [ { key = "a"; n = 0; } ]; operator = { key, n }: if n < 3 then [ { key = key + "a"; n = n + 1; } { key = key; n = 99; } ] else [ ]; })
  (genericClosure { startSet = [ { key = 80; } ]; operator = { key, foo ? false }: if key < 0 then [ ] else [ { key = key - 9; } { key = key - 13; foo = true; } ]; })
]

let b = { a = 1; }; a = 2; in [ { a = a; } { a = b.a; } (rec { c = b.a; a = c; }) ]

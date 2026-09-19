let a = rec { x = 1; y = 2; }; b = rec { x = 1; y = x; }; in a.x + b.y

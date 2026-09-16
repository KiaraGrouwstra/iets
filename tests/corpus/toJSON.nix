with builtins;
[
  (toJSON { a = 123; b = -456; c = "foo\n\"bar\"/\t"; e = true; f = null; g = [ 1 2 3 ]; })
  (toJSON [ 1.0e15 1.0e14 1.0e16 0.0001 0.00001 123456789012345678.0 1.5e-7 0.1 100.0 1.0e300 3.0e21 12345.678 2.5e16 1234567890123456.0 (0.0 - 1.0) ])
  (toJSON { __toString = self: "hi"; })
  (toJSON { outPath = "/foo"; })
  (toJSON { "1" = 1; "b c" = { }; "é" = "ü"; })
  (toJSON "")
  (toJSON [ ])
  (toJSON { })
]

with builtins;
[
  (toXML { a = "s"; })
  (toXML [ ("a" + "b") 10 (rec { x = "x"; y = x; }) ])
  (toXML (x: x))
  (toXML ({ a, b ? 1, ... }@args: a))
  (toXML ({ a }: a))
  (toXML [ map (map (x: x)) 1.5 /foo/bar null "a<b>&\"c\n" true false ])
  (toXML [ { type = "derivation"; drvPath = "/nix/store/x.drv"; outPath = "/nix/store/x"; name = "x"; } { type = "derivation"; drvPath = "/nix/store/x.drv"; outPath = "/nix/store/x"; } { type = "derivation"; } ])
  (toXML { "a b" = 1; "1" = [ ]; })
]

# A mkDerivation-like wrapper outside the lint root: it binds the
# derivation's attributes itself, as nixpkgs does.
attrs: derivation (attrs // { name = "wrapped"; system = "s"; builder = "b"; list = attrs.list; })

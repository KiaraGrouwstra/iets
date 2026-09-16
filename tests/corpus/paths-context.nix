let
  drv = "/nix/store/73as87hfqgllqymcyhn1bi8iman2wwqx-ia.drv";
  f = builtins.toFile "f" "x";
  s = builtins.appendContext "text" {
    ${drv} = { path = true; allOutputs = true; outputs = [ "out" "dev" ]; };
    ${builtins.unsafeDiscardStringContext f} = { path = true; };
  };
  eta = str: str == builtins.appendContext (builtins.unsafeDiscardStringContext str) (builtins.getContext str);
in
[
  s
  (builtins.getContext s)
  (builtins.getContext (builtins.appendContext "" {}))
  (builtins.getContext (builtins.appendContext "" { ${drv} = { outputs = [ "out" ]; }; }))
  (builtins.getContext (builtins.appendContext "" { ${drv} = { path = false; allOutputs = false; outputs = []; }; }))
  (builtins.getContext (builtins.appendContext f { ${drv} = { allOutputs = true; }; }))
  (builtins.getContext (builtins.unsafeDiscardOutputDependency s))
  (builtins.getContext (builtins.addDrvOutputDependencies (builtins.appendContext "" { ${drv} = { path = true; }; })))
  (builtins.getContext (builtins.addDrvOutputDependencies (builtins.appendContext "" { ${drv} = { allOutputs = true; }; })))
  (builtins.hasContext s)
  (builtins.hasContext (builtins.unsafeDiscardStringContext s))
  (eta s) (eta f) (eta "plain")
  (builtins.getContext (builtins.substring 0 0 s))
  (builtins.getContext "${s}${f}")
  (builtins.getContext (builtins.concatStringsSep "," [ s f ]))
  (builtins.getContext (toString [ s f ]))
]

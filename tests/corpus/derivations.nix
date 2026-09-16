# derivationStrict cases; .exp captured from nix-instantiate --eval --strict.
let
  base = { system = "x86_64-linux"; builder = "/bin/sh"; };
  d = builtins.derivation (base // { name = "x"; args = [ "-c" "echo" ]; });
  multi = builtins.derivation (base // {
    name = "m"; outputs = [ "out" "dev" "bin" ]; args = [ "-c" "x\"y\\z\n" ]; FOO = "b\tar";
  });
  hex = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
  sha1 = "a9993e364706816aba3e25717850c26c9cd0d89d";
  sri = "sha256-ungWv48Bz+pBQUDeXa4iI7ADYaOWF3qctBD/YfIAFa0=";
  fx = builtins.derivation (base // { name = "fx"; outputHash = hex; outputHashAlgo = "sha256"; });
  fr = builtins.derivation (base // { name = "fr"; outputHash = hex; outputHashAlgo = "sha256"; outputHashMode = "recursive"; });
  fs1 = builtins.derivation (base // { name = "fs1"; outputHash = sha1; outputHashAlgo = "sha1"; });
  fs1r = builtins.derivation (base // { name = "fs1r"; outputHash = sha1; outputHashAlgo = "sha1"; outputHashMode = "recursive"; });
  fsri = builtins.derivation (base // { name = "sri"; outputHash = sri; });
  fsrir = builtins.derivation (base // { name = "srir"; outputHash = sri; outputHashMode = "recursive"; });
  sa = builtins.derivation (base // {
    name = "sa"; __structuredAttrs = true; outputs = [ "out" "dev" ];
    list = [ 1 2.5 "s" true null { a = 1; } ]; nested = { b = ./derivations.nix; c = d; };
    f = 1.0e-7; g = 123456789.125;
  });
  nulls = builtins.derivation (base // { name = "nulls"; __ignoreNulls = true; foo = null; bar = "x"; });
  dep = builtins.derivation (base // { name = "dep"; f = fx; src = builtins.toFile "a" "x"; });
  dep2 = builtins.derivation (base // { name = "dep2"; d = multi.dev; e = "${dep}"; });
  ph = builtins.derivation (base // { name = "ph"; p = builtins.placeholder "out"; q = builtins.placeholder "dev"; });
  src = builtins.derivation (base // { name = "src"; s = ./derivations.nix; t = "${./derivations.nix}"; });
  paths = drv: { inherit (drv) drvPath outPath; };
in {
  d = paths d;
  multi = paths multi // { dev = multi.dev.outPath; bin = multi.bin.outPath; };
  fx = paths fx; fr = paths fr; fs1 = paths fs1; fs1r = paths fs1r; fsri = paths fsri; fsrir = paths fsrir;
  sa = paths sa // { dev = sa.dev.outPath; };
  nulls = paths nulls; dep = paths dep; dep2 = paths dep2; ph = paths ph; src = paths src;
  ctx = builtins.getContext "${d.drvPath}${d.out}${dep2}";
  ctx2 = builtins.getContext (builtins.addDrvOutputDependencies d.drvPath);
  disc = builtins.getContext (builtins.unsafeDiscardOutputDependency d.drvPath);
  drvAttrs = builtins.attrNames (builtins.derivationStrict (base // { name = "n"; outputs = [ "out" "lib" ]; }));
}

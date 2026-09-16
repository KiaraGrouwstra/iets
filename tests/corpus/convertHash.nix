with builtins;
let
  h16 = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
in
[
  (convertHash { hash = h16; hashAlgo = "sha256"; toHashFormat = "sri"; })
  (convertHash { hash = h16; hashAlgo = "sha256"; toHashFormat = "nix32"; })
  (convertHash { hash = h16; hashAlgo = "sha256"; toHashFormat = "base64"; })
  (convertHash { hash = h16; hashAlgo = "sha256"; toHashFormat = "base16"; })
  (convertHash { hash = "sha256:${h16}"; toHashFormat = "sri"; })
  (convertHash { hash = "sha256-47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU="; toHashFormat = "base16"; })
  (convertHash { hash = "0mdqa9w1p6cmli6976v4wi0sw9r4p5prkj7lzfd1877wk11c9c73"; hashAlgo = "sha256"; toHashFormat = "base16"; })
  (convertHash { hash = "d41d8cd98f00b204e9800998ecf8427e"; hashAlgo = "md5"; toHashFormat = "sri"; })
]

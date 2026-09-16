with builtins;
[
  (map (algo: hashFile algo ./data.txt) [ "md5" "sha1" "sha256" "sha512" ])
  (hashFile "sha256" (toString ./data.txt))
]

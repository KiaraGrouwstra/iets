with builtins;
map (algo: map (hashString algo) [ "" "text 1" "héllo" ]) [ "md5" "sha1" "sha256" "sha512" ]

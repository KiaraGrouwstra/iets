# Nix writes the system into the .drv without escapes: the `"` ends it
# early, and Nix cannot read the file it wrote.
derivation {
  name = "sys";
  system = "a\"b";
  builder = "/bin/sh";
}

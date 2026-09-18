{
  description = "classic mode flake fixture: no inputs, a locked flake.lock";
  outputs = { self }: {
    x = "hello";
    n = 1.5;
    l = [ 1 2 { a = null; } ];
    s = { a = 1; b = "two"; nested = { c = [ ]; }; };
    packages.x86_64-linux.pkg = derivation {
      name = "classic-e2e-flake";
      system = "x86_64-linux";
      builder = "/bin/sh";
      args = [ "-c" "echo flake > $out" ];
    };
    packages.x86_64-linux.default = self.packages.x86_64-linux.pkg;
  };
}

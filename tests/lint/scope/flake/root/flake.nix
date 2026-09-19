{
  inputs.dep.url = "path:@DEP@";
  outputs = { self, dep }: {
    value = toString (dep.value + (with { y = 2; }; y));
    file = "${./flake.nix}";
    dir = "${./sub}";
    inputFile = import (dep + "/file.nix");
    store = "${/. + "@STORE@"}";
  };
}

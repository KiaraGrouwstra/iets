{
  inputs.dep.url = "path:@DEP@";
  outputs = { self, dep }: {
    value = dep.value + http://root.example;
    file = "${./flake.nix}";
    dir = "${./sub}";
    inputFile = import (dep + "/file.nix");
    store = "${/. + "@STORE@"}";
  };
}

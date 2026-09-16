let d = ../fixtures/paths/dir; in
{
  a = builtins.readFileType (d + "/a.txt");
  sub = builtins.readFileType (d + "/sub");
  link = builtins.readFileType (d + "/link");
  dirlink = builtins.readFileType (d + "/dirlink");
  broken = builtins.readFileType (d + "/broken");
  viaString = builtins.readFileType (toString d + "/exe.sh");
}

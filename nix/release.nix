# The files of a GitHub release: one static musl binary per platform, as the
# README sells it, and their checksums. riscv64 is left out: CI only
# cross-builds it, and nothing runs it.
{ runCommand, version, static }:
runCommand "iets-release-${version}" { } ''
  [ "$(${static.x86_64-linux}/bin/iets version)" = "iets ${version}" ]
  mkdir $out
  ${builtins.concatStringsSep "\n" (map (system: ''
    cp ${static.${system}}/bin/iets $out/iets-${version}-${system}
  '') (builtins.attrNames static))}
  cd $out
  sha256sum iets-* > SHA256SUMS
''

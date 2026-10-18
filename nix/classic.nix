# `classic`: the real Nix with iets under its command names. Each name handles
# what iets implements and execs the real Nix for the rest.
# `share/iets/real-nix-bin` names that Nix, because the links shadow it
# (docs/reference/classic.md).
{ lib, runCommand, lndir, iets, nix }:
runCommand "iets-classic-${nix.version}" {
  # NixOS reads pname and version of nix.package.
  passthru = { inherit (nix) pname version; };
  meta.mainProgram = "nix";
} ''
  mkdir -p $out/bin $out/share/iets
  for o in ${toString (map (o: nix.${o}) (lib.intersectLists [ "out" "man" "doc" ] nix.outputs))}; do
    ${lndir}/bin/lndir -silent $o $out
  done
  # Nix's bin holds relative links to `nix`, which would reach iets here.
  for f in ${nix}/bin/*; do
    ln -sf $f $out/bin/
  done
  echo -n "${nix}/bin" > $out/share/iets/real-nix-bin
  for n in nix-instantiate nix-build nix-eval-jobs nix-shell nix; do
    ln -sf ${iets}/bin/iets $out/bin/$n
  done
''

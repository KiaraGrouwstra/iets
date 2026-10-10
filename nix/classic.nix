# `classic`: the same binary under Nix's own command names. Each one
# handles the subset iets implements and execs the real nix for the
# rest; `share/iets/real-nix-bin` names that nix, so the fallback works
# even when the farm shadows it out of PATH (docs/reference/classic.md).
{ runCommand, iets, nix }:
runCommand "iets-classic" { } ''
  mkdir -p $out/bin
  mkdir -p $out/share/iets
  echo -n "${nix}/bin" > $out/share/iets/real-nix-bin
  for n in nix-instantiate nix-build nix-eval-jobs nix-shell nix; do
    ln -s ${iets}/bin/iets $out/bin/$n
  done
''

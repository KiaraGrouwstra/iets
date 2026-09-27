self:
{ config, lib, pkgs, ... }:
let
  packages = self.packages.${pkgs.stdenv.hostPlatform.system};
in
{
  options.ietsp.classic.enable = lib.mkEnableOption
    "the ietsp classic command names ahead of nix in the system path";

  config = {
    environment.systemPackages = [ packages.default ]
      # buildEnv resolves a collision with nix's own bin/nix-instantiate by
      # meta.priority, lower wins, so hiPrio (-10) is what puts the farm's
      # symlinks in system-path instead. The farm links the one ietsp this
      # flake ships, never a rebuild against the host's pkgs, and is built for this host's
      # nix: the loser of the collision leaves the system path, and the
      # fallback finds it through the farm's own marker, with no environment.
      ++ lib.optional config.ietsp.classic.enable
        (lib.hiPrio (self.lib.mkClassic pkgs packages.default config.nix.package));

    programs.direnv = {
      enable = true;
      # `use flake` is ietsp's (./direnvrc); nix-direnv is not loaded.
      nix-direnv.enable = false;
      direnvrcExtra = builtins.readFile ./direnvrc;
    };
  };
}

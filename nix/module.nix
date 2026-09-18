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
      # symlinks in system-path instead.
      ++ lib.optional config.ietsp.classic.enable (lib.hiPrio packages.classic);

    # The loser of a buildEnv collision is not in system-path at all, so the
    # fallback would find no other `nix` on PATH.
    environment.variables = lib.mkIf config.ietsp.classic.enable {
      IETSP_NIX_BIN_DIR = "${config.nix.package}/bin";
    };

    programs.direnv = {
      enable = true;
      # `use flake` is ietsp's (./direnvrc); nix-direnv is not loaded.
      nix-direnv.enable = false;
      direnvrcExtra = builtins.readFile ./direnvrc;
    };
  };
}

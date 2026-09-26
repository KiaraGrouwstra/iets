self:
{ config, lib, pkgs, ... }:
let
  packages = self.packages.${pkgs.stdenv.hostPlatform.system};
  settings = config.ietsp.settings;
in
{
  options.ietsp.classic.enable = lib.mkEnableOption
    "the ietsp classic command names ahead of nix in the system path";

  options.ietsp.settings = lib.mkOption {
    type = with lib.types; attrsOf (oneOf [ bool int str (listOf str) ]);
    default = { };
    example = { builder = "ssh-ng://bld1"; workers = 8; };
    description = ''
      ietsp's own settings, rendered to /etc/ietsp/ietsp.toml. They are read
      only by ietsp, so `builder` can point the fleet's builds somewhere the
      nix behind the classic fallback never sees. The Nix type is the TOML
      type: `workers = 8`, not `"8"`. Keys and defaults:
      docs/reference/settings.md.
    '';
  };

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

    # a JSON bool, int, string or list of strings is the same TOML value
    environment.etc."ietsp/ietsp.toml" = lib.mkIf (settings != { }) {
      text = lib.concatStrings
        (lib.mapAttrsToList (k: v: "${k} = ${builtins.toJSON v}\n") settings);
    };

    programs.direnv = {
      enable = true;
      # `use flake` is ietsp's (./direnvrc); nix-direnv is not loaded.
      nix-direnv.enable = false;
      direnvrcExtra = builtins.readFile ./direnvrc;
    };
  };
}

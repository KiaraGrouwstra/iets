self:
{ config, lib, pkgs, ... }:
let
  packages = self.packages.${pkgs.stdenv.hostPlatform.system};
  settings = config.ietsp.settings;
  render = v:
    if lib.isBool v then lib.boolToString v
    else if lib.isList v then lib.concatMapStringsSep " " toString v
    else toString v;
in
{
  options.ietsp.classic.enable = lib.mkEnableOption
    "the ietsp classic command names ahead of nix in the system path";

  options.ietsp.settings = lib.mkOption {
    type = with lib.types; attrsOf (oneOf [ bool int str (listOf str) ]);
    default = { };
    example = { builder = "ssh-ng://bld1"; workers = 8; };
    description = ''
      ietsp's own settings, rendered to /etc/ietsp/ietsp.conf. They are read
      only by ietsp, so `builder` can point the fleet's builds somewhere the
      nix behind the classic fallback never sees. Keys and defaults:
      docs/config.md.
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

    environment.etc."ietsp/ietsp.conf" = lib.mkIf (settings != { }) {
      text = lib.concatStrings
        (lib.mapAttrsToList (k: v: "${k} = ${render v}\n") settings);
    };

    programs.direnv = {
      enable = true;
      # `use flake` is ietsp's (./direnvrc); nix-direnv is not loaded.
      nix-direnv.enable = false;
      direnvrcExtra = builtins.readFile ./direnvrc;
    };
  };
}

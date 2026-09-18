self:
{ pkgs, ... }:
{
  environment.systemPackages = [
    self.packages.${pkgs.stdenv.hostPlatform.system}.default
  ];

  programs.direnv = {
    enable = true;
    # `use flake` is ietsp's (./direnvrc); nix-direnv is not loaded.
    nix-direnv.enable = false;
    direnvrcExtra = builtins.readFile ./direnvrc;
  };
}

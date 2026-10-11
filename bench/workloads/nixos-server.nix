# A NixOS server: several services and a handful of command-line packages,
# and no desktop. It forces far more of the module system than the minimal one.
# Resolve nixpkgs through NIX_PATH so this workload can be run directly.
let
  nixpkgs = <nixpkgs>;
in
  ((import (nixpkgs + "/nixos") {
    system = "x86_64-linux";
    configuration = { pkgs, ... }: {
      boot.loader.grub.enable = false;
      fileSystems."/" = { device = "none"; fsType = "tmpfs"; };
      networking.hostName = "server";
      networking.networkmanager.enable = true;
      system.stateVersion = "25.11";
      nixpkgs.config = {
        allowUnfree = true;
        allowBroken = true;
        allowUnsupportedSystem = true;
      };

      services.openssh.enable = true;
      services.nginx.enable = true;
      services.postgresql.enable = true;
      services.printing.enable = true;
      virtualisation.docker.enable = true;

      users.users.bench = {
        isNormalUser = true;
        extraGroups = [ "wheel" "docker" ];
      };

      environment.systemPackages = with pkgs; [
        git vim ripgrep jq curl wget htop tmux fd bat
      ];
    };
  }).config.system.build.toplevel).drvPath

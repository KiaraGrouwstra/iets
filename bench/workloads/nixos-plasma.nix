# A NixOS desktop: Plasma 6 on SDDM with sound, printing and Firefox, as
# the graphical installer writes it. The desktop pulls in Qt, KDE and most
# of the graphics stack, so it forces far more of nixpkgs than nixos-server.
# Resolve nixpkgs through NIX_PATH so this workload can be run directly.
let
  nixpkgs = <nixpkgs>;
in
  ((import (nixpkgs + "/nixos") {
    system = "x86_64-linux";
    configuration = { pkgs, ... }: {
      boot.loader.grub.enable = false;
      fileSystems."/" = { device = "none"; fsType = "tmpfs"; };
      networking.hostName = "plasma";
      networking.networkmanager.enable = true;
      system.stateVersion = "25.11";
      nixpkgs.config = {
        allowUnfree = true;
        allowBroken = true;
        allowUnsupportedSystem = true;
      };

      time.timeZone = "Europe/Zurich";
      i18n.defaultLocale = "en_US.UTF-8";

      services.xserver.enable = true;
      services.xserver.xkb.layout = "us";
      services.displayManager.sddm.enable = true;
      services.desktopManager.plasma6.enable = true;

      services.printing.enable = true;
      services.pulseaudio.enable = false;
      security.rtkit.enable = true;
      services.pipewire = {
        enable = true;
        alsa.enable = true;
        alsa.support32Bit = true;
        pulse.enable = true;
      };

      users.users.bench = {
        isNormalUser = true;
        extraGroups = [ "networkmanager" "wheel" ];
        packages = with pkgs; [ kdePackages.kate ];
      };

      programs.firefox.enable = true;

      environment.systemPackages = with pkgs; [ git vim wget ];
    };
  }).config.system.build.toplevel).drvPath

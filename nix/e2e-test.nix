# scripts/<name>.nu in a NixOS test container: a real nix-daemon, a writable
# store, the real nix on PATH and no network.
{ self, nixpkgs, pkgs, ietsp, name, packages ? [ ], env ? "" }:
let
  system = pkgs.stdenv.hostPlatform.system;
  # nixpkgs/shell.nix imports this tree through a sha256-pinned fetchTarball;
  # the same store path, so the container's nix finds it and fetches nothing.
  pin = (pkgs.lib.importJSON "${nixpkgs}/ci/pinned.json").pins.nixpkgs;
  pinned = pkgs.fetchzip { inherit (pin) url hash; };
  # Every store path a case realises. Nothing is substituted, so what is not
  # here gets built, and only the scripts' own /bin/sh derivations can be.
  offline = [
    "${nixpkgs}"
    "${self}"
    pinned
    ietsp
    pkgs.hello
    pkgs.hello.inputDerivation
    pkgs.bashInteractive
    pkgs.coreutils
    pkgs.nix-output-monitor
    self.devShells.${system}.default.inputDerivation
  ];
  # The container gets the build's store as one read-only bind mount per
  # path, which no overlay can see through. So the writable store is a fresh
  # directory with the closure bound into it, path by path, and a database
  # that knows exactly those paths. The registration is opened first: the new
  # store hides it.
  writableStore = pkgs.writeShellScript "writable-store" ''
    set -eu
    exec 3< "$1/registration"
    mkdir -p /nix/.store
    while read -r p; do
      t=/nix/.store/''${p##*/}
      if [ -d "$p" ]; then mkdir "$t"; else touch "$t"; fi
      mount --bind "$p" "$t"
    done < "$1/store-paths"
    mount --rbind /nix/.store /nix/store
    NIX_REMOTE=local nix-store --load-db <&3
  '';
in
pkgs.testers.runNixOSTest {
  name = "ietsp-${name}";
  containers.machine = { lib, ... }: {
    nix.package = pkgs.nix;
    nix.settings = {
      experimental-features = [ "nix-command" "flakes" ];
      substituters = lib.mkForce [ ];
      trusted-users = [ "alice" ];
    };
    nix.nixPath = lib.mkForce [ ];
    nix.channel.enable = false;
    # the daemon must not open the database before the store is in place
    systemd.sockets.nix-daemon.wantedBy = lib.mkForce [ ];
    users.users.alice.isNormalUser = true;
    environment.systemPackages = [ pkgs.nushell pkgs.gnutar pkgs.gzip pkgs.diffutils ] ++ packages;
  };
  testScript = { containers, ... }:
    let
      registration = pkgs.closureInfo {
        rootPaths = offline ++ [ containers.machine.system.build.toplevel writableStore ];
      };
    in
    ''
      machine.wait_for_unit("multi-user.target")
      machine.succeed("${writableStore} ${registration}")
      machine.succeed("systemctl start nix-daemon.socket")
      machine.succeed("cp -r ${self} /home/alice/ietsp && chmod -R u+w /home/alice/ietsp && chown -R alice /home/alice/ietsp")
      machine.succeed("runuser -l alice -c 'cd ietsp && IETSP=${ietsp}/bin/ietsp NIXPKGS=${nixpkgs} ${env} nu scripts/${name}.nu' >&2")
    '';
}

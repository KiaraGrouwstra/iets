{
  description = "eval-jobs flake fixture";
  outputs = { self }: {
    hydraJobs = import ../ci.nix { };
    legacyPackages.x86_64-linux = import ../aggregates.nix { };
  };
}

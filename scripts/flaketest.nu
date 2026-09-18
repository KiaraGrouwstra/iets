#!/usr/bin/env nu
# End-to-end flake evaluation: `ietsp eval --raw FLAKE#attr` must print what
# `nix eval --raw --option eval-cache false FLAKE#attr` prints, on this
# repo's flake, on a scratch flake with locked inputs of every offline type
# (`path:`, `git+file://`, a `file://` tarball), a `follows`, a devShell and a
# NixOS system, and on a scratch git flake with a `path:` input, a relative
# `path:` input (no `narHash` in the lock), a `follows`, a clean tree, a
# dirty tree, and a `git worktree` at a path nix has never copied (ietsp
# must not fetch the root tree). The only fetch is this repo's own nixpkgs
# input; the scratch flakes reuse that store tree through a `path:` input.
use lib.nu [chomp show verdict]

def --wrapped nixf [...args: string] { ^nix --extra-experimental-features "nix-command flakes" ...$args }
def --wrapped git-in [repo: string, ...args: string] {
  ^git -c user.name=ietsp -c user.email=ietsp@example.com -c init.defaultBranch=main -C $repo ...$args
}

# check NAME REF ENV: both evaluators must agree and succeed. Prints the verdict, returns it.
def check [ietsp: string, name: string, ref: string, extra?: record]: nothing -> bool {
  let n = (nixf eval --raw --option eval-cache "false" $ref | complete)
  let i = (with-env ($extra | default {}) { ^$ietsp eval --raw --no-cache $ref | complete })
  let exp = ($n.stdout | chomp)
  let got = ($i.stdout | chomp)
  if $exp != "" and $exp == $got {
    print $"ok   ($name)"
    true
  } else {
    print $"FAIL ($name)"
    print $"  nix:   ($exp)"
    $n.stderr | lines | where not ($it | str contains "is dirty") | last 3 | show
    print $"  ietsp: ($got)"
    $i.stderr | lines | last 3 | show
    false
  }
}

# check-fresh NAME REF: like check, on a tree nix has never copied to the
# store. ietsp runs first, with the cache off, and must not fetch anything;
# nix then gives the expected value (and copies the tree).
def check-fresh [ietsp: string, name: string, ref: string]: nothing -> bool {
  let i = (with-env {IETSP_CACHE: "0"} { ^$ietsp eval --raw --no-cache $ref | complete })
  let fetched = ($i.stderr | lines | where ($it | str contains "fetching"))
  let n = (nixf eval --raw --option eval-cache "false" $ref | complete)
  let exp = ($n.stdout | chomp)
  let got = ($i.stdout | chomp)
  if $exp != "" and $exp == $got and ($fetched | is-empty) {
    print $"ok   ($name)"
    true
  } else {
    print $"FAIL ($name)"
    print $"  nix:   ($exp)"
    print $"  ietsp: ($got)"
    $i.stderr | lines | last 3 | show
    false
  }
}

# the `cache:` lines of a --timings run
def cache-lines [ietsp: string, ref: string]: nothing -> list<string> {
  ^$ietsp eval --raw --timings $ref | complete | get stderr | lines | where $it =~ '^cache:'
}

def run-tests [ietsp: string, work: string]: nothing -> int {
  $env.IETSP_CACHE_DIR = $"($work)/cache"
  mut res = []

  # ---- this repo ---------------------------------------------------------------
  $res ++= [(check $ietsp "repo packages.default" ".#packages.x86_64-linux.default.drvPath")]

  # ---- a flake with locked inputs of every type, a devShell and a NixOS system --
  # Offline: nixpkgs is the store tree this repo's lock names (`path:`), `lib`
  # is a scratch git repo (`git+file://`), `util` a directory (`path:`) whose
  # own `lib` input follows ours, `tb` a tarball behind a `file://` URL. `nix
  # flake lock` then fetches nothing from the network.
  let nixpkgs = (nixf eval --raw --impure --expr $"\(builtins.getFlake \"($env.PWD)\"\).inputs.nixpkgs.outPath" | complete | get stdout | chomp)
  let big = $"($work)/big"
  mkdir $"($work)/lib" $"($work)/util" $"($work)/tbsrc" $big
  '{ outputs = { self }: { greeting = "hello-" + self.shortRev; }; }' + "\n" | save $"($work)/lib/flake.nix"
  git-in $"($work)/lib" init -q
  git-in $"($work)/lib" add -A
  git-in $"($work)/lib" commit -q -m lib
  r#'{
  inputs.lib.url = "git+file://@WORK@/lib";
  outputs = { self, lib }: { twice = lib.greeting + lib.greeting; inherit lib; };
}
'# | str replace -a "@WORK@" $work | save $"($work)/util/flake.nix"
  '{ outputs = { self }: { value = "tarball-" + self.narHash; }; }' + "\n" | save $"($work)/tbsrc/flake.nix"
  ^tar -C $work -czf $"($work)/tb.tar.gz" tbsrc
  r#'{
  inputs.nixpkgs.url = "path:@NIXPKGS@";
  inputs.lib.url = "git+file://@WORK@/lib";
  inputs.util.url = "path:@WORK@/util";
  inputs.util.inputs.lib.follows = "lib";
  inputs.tb.url = "file://@WORK@/tb.tar.gz";
  outputs = { self, nixpkgs, lib, util, tb }:
    let
      system = "x86_64-linux";
      pkgs = nixpkgs.legacyPackages.${system};
    in {
      devShells.${system}.default = pkgs.mkShell {
        packages = [ pkgs.hello pkgs.jq ];
        GREETING = util.twice + " " + tb.value + " " + lib.greeting;
      };
      nixosConfigurations.scratch = nixpkgs.lib.nixosSystem {
        inherit system;
        modules = [ ./configuration.nix ];
      };
    };
}
'# | str replace -a "@WORK@" $work | str replace -a "@NIXPKGS@" $nixpkgs | save $"($big)/flake.nix"
  r#'{ modulesPath, ... }: {
  imports = [ (modulesPath + "/profiles/minimal.nix") ];
  boot.loader.grub.enable = false;
  fileSystems."/" = { device = "none"; fsType = "tmpfs"; };
  networking.hostName = "scratch";
  system.stateVersion = "25.11";
}
'# | save $"($big)/configuration.nix"
  let lock = (do { cd $big; nixf flake lock } | complete)
  if $lock.exit_code != 0 { print $"FAIL nix flake lock: ($lock.stderr)"; $res ++= [false] }
  $res ++= [(check $ietsp "big: devShell" $"($big)#devShells.x86_64-linux.default.drvPath")]
  $res ++= [(check $ietsp "big: devShell, IETSP_WORKERS=8" $"($big)#devShells.x86_64-linux.default.drvPath" {IETSP_WORKERS: "8"})]
  $res ++= [(check $ietsp "big: nixosConfigurations.scratch toplevel" $"($big)#nixosConfigurations.scratch.config.system.build.toplevel.drvPath")]

  # ---- scratch flake ---------------------------------------------------------
  let repo = $"($work)/repo"
  mkdir $"($work)/sub" $"($work)/dep" $repo
  '{ outputs = { self }: { value = "sub-" + self.narHash; }; }' + "\n" | save $"($work)/sub/flake.nix"
  r#'{
  inputs.sub.url = "path:@WORK@/sub";
  outputs = { self, sub }: { value = "dep-" + sub.value; inherit sub; };
}
'# | str replace -a "@WORK@" $work | save $"($work)/dep/flake.nix"
  # `rel` is a relative `path:` input inside the repo; `nix flake lock` writes
  # it without a `narHash`, and call-flake.nix resolves it under the root tree.
  r#'{
  inputs.sub.url = "path:@WORK@/sub";
  inputs.dep.url = "path:@WORK@/dep";
  inputs.dep.inputs.sub.follows = "sub";
  inputs.rel.url = "path:./rel";
  outputs = { self, dep, sub, rel }: {
    packages.x86_64-linux.default = derivation {
      name = "scratch";
      system = "x86_64-linux";
      builder = "/bin/sh";
      args = [ "-c" "echo ${self.narHash} ${self.rev or self.dirtyRev or "norev"} ${self.shortRev or self.dirtyShortRev or "norev"} ${toString self.lastModified} ${self.lastModifiedDate} ${toString (self.revCount or 0)} ${dep.value} ${dep.sub.value} ${dep.outPath} ${sub.outPath} ${rel.value} ${rel.outPath} ${self}/file ${builtins.readFile (self + "/file")} ${toString (builtins.attrNames (builtins.readDir self))} ${toString (builtins.pathExists (self + "/untracked"))} ${import ./nested/lib.nix} ${./nested} > $out" ];
    };
  };
}
'# | str replace -a "@WORK@" $work | save $"($repo)/flake.nix"
  "one\n" | save $"($repo)/file"
  mkdir $"($repo)/nested" $"($repo)/rel"
  "two\n" | save $"($repo)/nested/tracked"
  '"lib"' + "\n" | save $"($repo)/nested/lib.nix"
  '{ outputs = { self }: { value = "rel-" + builtins.readFile (self + "/data"); }; }' + "\n" | save $"($repo)/rel/flake.nix"
  "reldata" | save $"($repo)/rel/data"
  git-in $repo init -q
  git-in $repo add -A
  git-in $repo commit -q -m init
  do { cd $repo; nixf flake lock } | complete | ignore
  git-in $repo add flake.lock
  git-in $repo commit -q -m lock
  let scratch = $"($repo)#packages.x86_64-linux.default"

  # nix writes a relative path node as `{ path = "./rel"; type = "path"; }`, no narHash;
  # lock::check must accept it and the evaluation must use the tree under the root.
  let rel_locked = (open --raw $"($repo)/flake.lock" | from json | get nodes.rel.locked)
  $res ++= [(verdict ($rel_locked == {path: "./rel", type: "path"}) "lock: relative path input has no narHash, as nix writes it" $"lock: nodes.rel.locked is ($rel_locked | to json -r)")]

  # a worktree at a path nix has never copied: the root tree is a virtual copy, never fetched
  let fresh = $"($work)/fresh"
  git-in $repo worktree add -q $fresh HEAD
  "untracked\n" | save $"($fresh)/untracked"
  $res ++= [(check-fresh $ietsp "worktree: clean tree nix never saw, nothing fetched" $"($fresh)#packages.x86_64-linux.default.drvPath")]
  "changed\n" | save -f $"($fresh)/file"
  $res ++= [(check-fresh $ietsp "worktree: dirty tree nix never saw, nothing fetched" $"($fresh)#packages.x86_64-linux.default.drvPath")]
  git-in $repo worktree remove --force $fresh

  # this repo's own flake from a fresh worktree
  let mine = $"($work)/ietsp-fresh"
  git-in $env.PWD worktree add -q $mine HEAD
  $res ++= [(check-fresh $ietsp "worktree: this repo's packages.default, nothing fetched" $"($mine)#packages.x86_64-linux.default.drvPath")]
  git-in $env.PWD worktree remove --force $mine

  $res ++= [(check $ietsp "scratch: clean tree" $"($scratch).drvPath")]
  $res ++= [(check $ietsp "scratch: --raw on a derivation prints its outPath" $scratch)]
  "untracked\n" | save $"($repo)/untracked"
  $res ++= [(check $ietsp "scratch: untracked file is ignored" $"($scratch).drvPath")]
  "three\n" | save -f $"($repo)/file"
  $res ++= [(check $ietsp "scratch: dirty tree (modified tracked file)" $"($scratch).drvPath")]
  rm $"($repo)/nested/tracked"
  $res ++= [(check $ietsp "scratch: dirty tree (deleted tracked file)" $"($scratch).drvPath")]
  "four\n" | save $"($repo)/added"
  git-in $repo add added
  $res ++= [(check $ietsp "scratch: dirty tree (staged new file)" $"($scratch).drvPath")]
  git-in $repo commit -q -am next
  $res ++= [(check $ietsp "scratch: clean again after commit" $"($scratch).drvPath")]
  $res ++= [(check $ietsp "scratch: subdirectory flake ref" $"($repo)/.#packages.x86_64-linux.default.drvPath")]
  $res ++= [(check $ietsp "scratch: path: ref copies the whole tree" $"path:($repo)#packages.x86_64-linux.default.drvPath")]

  let exp = (nixf eval --json --option eval-cache "false" $"($scratch).drvPath" | complete | get stdout | chomp)
  let got = (^$ietsp eval --json --no-cache $"($scratch).drvPath" | complete | get stdout | chomp)
  $res ++= [(verdict ($exp != "" and $exp == $got) "scratch: --json" $"scratch: --json \(($exp) vs ($got)\)")]

  # tracing cache: hit after a miss, miss after the tree changes
  ^$ietsp eval --raw $"($scratch).drvPath" | complete | ignore
  let st = (cache-lines $ietsp $"($scratch).drvPath" | str join "\n")
  $res ++= [(verdict ($st | str contains "cache: hit") "cache: second run hits" $"cache: second run hits \(($st)\)")]
  "five\n" | save -f $"($repo)/file"
  let st = (cache-lines $ietsp $"($scratch).drvPath" | get -o 0 | default "")
  $res ++= [(verdict ($st | str contains "miss (") "cache: editing a tracked file misses" $"cache: editing a tracked file misses \(($st)\)")]
  git-in $repo commit -q -am five
  let st = (cache-lines $ietsp $"($scratch).drvPath" | get -o 0 | default "")
  $res ++= [(verdict ($st =~ 'miss \(.*git') "cache: a commit misses on a git command" $"cache: a commit misses on a git command \(($st)\)")]

  rm $"($repo)/flake.lock"
  let err = (^$ietsp eval --raw --no-cache $"($scratch).drvPath" | complete | get stderr)
  $res ++= [(verdict ($err | str contains "flake.lock missing or incomplete, run nix flake lock") "missing flake.lock is an error" $"missing flake.lock is an error \(($err)\)")]

  # a locked remote input whose tree is not in the store, with no nix to fetch it:
  # the two messages name the input, exit is non-zero, stdout stays empty
  let ghost = $"($work)/ghost"
  mkdir $ghost $"($work)/nobin"
  let hash = $"sha256-(('ghost-input-never-in-the-store!!' | encode base64))"
  '{ inputs.ghost.url = "github:NixOS/ghost"; outputs = { self, ghost }: { v = ghost.outPath; }; }' + "\n" | save $"($ghost)/flake.nix"
  {
    nodes: {
      ghost: { locked: { type: "github", owner: "NixOS", repo: "ghost", rev: "0000000000000000000000000000000000000000", narHash: $hash }, original: { type: "github", owner: "NixOS", repo: "ghost" } }
      root: { inputs: { ghost: "ghost" } }
    }
    root: "root"
    version: 7
  } | to json | save $"($ghost)/flake.lock"
  let g = (with-env {IETSP_NIX_BIN_DIR: $"($work)/nobin", PATH: $"($work)/nobin"} { ^$ietsp eval --raw --no-cache $"($ghost)#v" | complete })
  let named = ($g.stderr | lines | where ($it | str contains "input 'ghost'") | length)
  $res ++= [(verdict ($g.exit_code != 0 and $g.stdout == "" and $named == 2 and ($g.stderr | str contains "nix is not on PATH")) "unfetchable input: both messages name it, no nix anywhere is plain" $"unfetchable input \(exit ($g.exit_code)\): ($g.stderr | lines | first 2 | str join ' | ')")]

  let fail = ($res | where not $it | length)
  print $"pass=($res | where $it | length) fail=($fail)"
  if $fail == 0 { 0 } else { 1 }
}

def main [] {
  cd ($env.FILE_PWD | path join "..")
  let ietsp = ($env.IETSP? | default ($env.PWD | path join "target/release/ietsp"))
  let work = (mktemp -d --tmpdir ietsp-flaketest.XXXXXX)
  let code = (try { run-tests $ietsp $work } catch {|e| print -e $e.rendered; 1 })
  rm -rf $work
  exit $code
}

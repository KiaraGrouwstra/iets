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
#
# It also covers the input store (docs/reference/flakes.md): a fetched input is kept
# there and not registered anywhere, `ietsp` alone evaluates such a flake with
# no nix, no daemon and a fresh cache, a second run does it with
# `IETSP_OFFLINE=1`, and `build --no-build` sends the input's NAR out of the
# same spool.
use lib.nu [ROOT chomp ietsp-bin isolate run-in show verdict]

def --wrapped nixf [...args: string] { ^nix --extra-experimental-features "nix-command flakes" ...$args }
def --wrapped git-in [repo: string, ...args: string] {
  ^git -c user.name=ietsp -c user.email=ietsp@example.com -c init.defaultBranch=main -C $repo ...$args
}

# the `--log-format internal-json` records in STDERR
def records [stderr: string]: nothing -> list<record> {
  $stderr | lines | where ($it | str starts-with "@nix ") | each { str substring 5.. | from json }
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

# fetch-only NAME REF EXPECTED NOBIN: ietsp alone, with NOBIN (an empty
# directory) as the whole PATH and as IETSP_NIX_BIN_DIR, so no nix exists to
# fall back on. It must say it is fetching the input and print EXPECTED.
def fetch-only [ietsp: string, name: string, ref: string, expected: string, nobin: string]: nothing -> bool {
  let i = (with-env {IETSP_NIX_BIN_DIR: $nobin, PATH: $nobin} { ^$ietsp eval --raw --no-cache $ref | complete })
  let got = ($i.stdout | chomp)
  if $expected != "" and $got == $expected and ($i.stderr | str contains "fetching it") {
    print $"ok   ($name)"
    true
  } else {
    print $"FAIL ($name)"
    print $"  want:  ($expected)"
    print $"  ietsp: ($got)"
    $i.stderr | lines | last 3 | show
    false
  }
}

# standalone NAME REF EXPECTED NOBIN CACHE: ietsp with no nix on PATH, no
# IETSP_NIX_BIN_DIR, an unreachable store (nix.conf's `store`, which
# `Connection::open` reads), and a cache root of its own. The first run must
# fetch into the input store and print EXPECTED; the second must print it
# again with IETSP_OFFLINE=1, which makes any download an error.
def standalone [ietsp: string, name: string, ref: string, expected: string, nobin: string, cache: string]: nothing -> bool {
  let vars = {
    IETSP_NIX_BIN_DIR: $nobin,
    PATH: $nobin,
    NIX_CONFIG: "store = unix:///nonexistent/ietsp-no-daemon",
    XDG_CACHE_HOME: $cache,
    IETSP_CACHE_DIR: $"($cache)/ietsp",
  }
  rm -rf $cache
  let one = (with-env $vars { ^$ietsp eval --raw --no-cache $ref | complete })
  let two = (with-env ($vars | merge {IETSP_OFFLINE: "1"}) { ^$ietsp eval --raw --no-cache $ref | complete })
  let entries = (try { ls $"($cache)/ietsp/inputs" | where type == dir | length } catch { 0 })
  let ok = (
    $expected != "" and ($one.stdout | chomp) == $expected and ($two.stdout | chomp) == $expected
      and ($one.stderr | str contains "fetching it") and not ($two.stderr | str contains "fetching it")
      and $entries > 0
  )
  if $ok {
    print $"ok   ($name)"
    true
  } else {
    print $"FAIL ($name)"
    print $"  want:      ($expected)"
    print $"  first:     ($one.stdout | chomp) \(exit ($one.exit_code), ($entries) input store entries\)"
    $one.stderr | lines | last 3 | show
    print $"  second:    ($two.stdout | chomp) \(exit ($two.exit_code)\)"
    $two.stderr | lines | last 3 | show
    false
  }
}

# the store path of a locked input of the flake at DIR
def input-path [dir: string, input: string]: nothing -> string {
  nixf eval --raw --impure --expr $"\(builtins.getFlake \"($dir)\"\).inputs.($input).outPath" | complete | get stdout | chomp
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
  '{ outputs = { self }: { value = "tarball-" + self.narHash; }; }' + $"\n# ($work)\n" | save $"($work)/tbsrc/flake.nix"
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
  # reading an untracked file is Nix's error, and the hint is the `git add`
  let ut = $"($work)/untracked"
  mkdir $ut
  '{ outputs = _: { v = builtins.readFile ./extra; }; }' | save $"($ut)/flake.nix"
  '{"nodes":{"root":{}},"root":"root","version":7}' | save $"($ut)/flake.lock"
  "x\n" | save $"($ut)/extra"
  git-in $ut init -q
  git-in $ut add flake.nix flake.lock
  let ut_line = $"Path 'extra' in the repository \"($ut)\" is not tracked by Git."
  let ut_n = (nixf eval --raw $"($ut)#v" | complete)
  let ut_i = (^$ietsp eval --raw --no-cache --log-format internal-json $"($ut)#v" | complete)
  let ut_rec = (records $ut_i.stderr | where level? == 0 | get -o 0 | default {})
  let ut_ok = ($ut_n.exit_code == 1 and ($ut_n.stderr | str contains $ut_line) and $ut_i.exit_code == 1
    and ($ut_rec.raw_msg? | default "" | str starts-with $ut_line)
    and ($ut_rec.hint? | default "") == $"run 'git -C \"($ut)\" add \"extra\"'")
  $res ++= [(verdict $ut_ok "git: reading an untracked file is Nix's error, the hint is the git add" $"untracked read \(exit ($ut_n.exit_code)/($ut_i.exit_code)\): ($ut_rec | to json -r)")]
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

  # ---- fetching an input, with no nix to fall back on -------------------------
  # `nobin` is empty, so IETSP_NIX_BIN_DIR and PATH find no nix at all: every
  # case below is ietsp's own substitution or its own fetch.
  let nobin = $"($work)/nobin"
  mkdir $nobin

  # a `file://` tarball, offline: nix locks it, the store copy goes away, and
  # ietsp downloads, unpacks (one leading component stripped), hashes and
  # registers the tree again.
  let tbf = $"($work)/tbflake"
  mkdir $tbf $"($work)/tb2src/sub"
  $"hello ($work)\n" | save $"($work)/tb2src/a"
  "#!/bin/sh\n" | save $"($work)/tb2src/run"
  chmod 755 $"($work)/tb2src/run"
  "x\n" | save $"($work)/tb2src/sub/b"
  ^ln -s a $"($work)/tb2src/link"
  ^tar -C $work -czf $"($work)/tb2.tar.gz" tb2src
  r#'{
  inputs.tb.url = "file://@WORK@/tb2.tar.gz";
  inputs.tb.flake = false;
  outputs = { self, tb }: { v = toString (builtins.attrNames (builtins.readDir tb)) + " " + builtins.readFile (tb + "/a"); };
}
'# | str replace -a "@WORK@" $work | save $"($tbf)/flake.nix"
  do { cd $tbf; nixf flake lock } | complete | ignore
  let tb_exp = (nixf eval --raw --option eval-cache "false" $"($tbf)#v" | complete | get stdout | chomp)
  let tb_path = (input-path $tbf "tb")
  nixf store delete $tb_path | complete | ignore
  $res ++= [(fetch-only $ietsp "tarball: file:// input indexed into ietsp's input store" $"($tbf)#v" $tb_exp $nobin)]
  # nothing was registered: the tree lives in the input store, not the store
  let tb_entry = $"($work)/cache/inputs/($tb_path | path basename)"
  let virt_ok = (not ($tb_path | path exists)) and ($"($tb_entry)/spool" | path exists) and ($"($tb_entry)/index" | path exists)
  $res ++= [(verdict $virt_ok "tarball: the fetch wrote the input store, not /nix/store" $"input store entry ($tb_entry)")]

  # `ietsp build --no-build` is the only thing that uploads an input, and it
  # sends the NAR out of that spool. The lock is written by hand from
  # `nix hash path` of the tree the tar holds, so nix never fetches this input
  # and its store path is one the daemon has never seen.
  let upf = $"($work)/upflake"
  mkdir $upf $"($work)/tb3src/sub"
  $"hello3 ($work)\n" | save $"($work)/tb3src/a"
  "y\n" | save $"($work)/tb3src/sub/b"
  ^tar -C $work -czf $"($work)/tb3.tar.gz" tb3src
  # dereferenceSingletonDirectory strips the one top-level entry, so the lock's
  # narHash is the hash of what is inside it.
  let tb3_hash = (nixf hash path --type sha256 --sri $"($work)/tb3src" | complete | get stdout | chomp)
  r#'{
  inputs.tb.url = "file://@WORK@/tb3.tar.gz";
  inputs.tb.flake = false;
  outputs = { self, tb }: {
    packages.x86_64-linux.default = derivation {
      name = "usestb";
      system = "x86_64-linux";
      builder = "/bin/sh";
      args = [ "-c" "cp ${tb}/a $out" ];
    };
  };
}
'# | str replace -a "@WORK@" $work | save $"($upf)/flake.nix"
  {
    nodes: {
      tb: { flake: false, locked: { type: "tarball", url: $"file://($work)/tb3.tar.gz", narHash: $tb3_hash }, original: { type: "tarball", url: $"file://($work)/tb3.tar.gz" } }
      root: { inputs: { tb: "tb" } }
    }
    root: "root"
    version: 7
  } | to json | save $"($upf)/flake.lock"
  let ub = (^$ietsp build --no-build --timings --ietsp-option builder daemon $"($upf)#packages.x86_64-linux.default" | complete)
  let tb3_path = (^$ietsp eval --raw --no-cache -E $"\(builtins.fetchTree { type = \"tarball\"; url = \"file://($work)/tb3.tar.gz\"; narHash = \"($tb3_hash)\"; }).outPath" | complete | get stdout | str trim)
  let tb3_valid = ((nixf path-info $tb3_path | complete).exit_code == 0)
  let resolve = ($ub.stderr | lines | where $it =~ '^resolve:' | get -o 0 | default "")
  let add = ($ub.stderr | lines | where $it =~ '^add:' | get -o 0 | default "")
  # The input is the plan's only source, it is not in the store and there is no
  # local tree behind it, so a resolved source that was not "read from
  # /nix/store" came out of the spool, and the daemon took its NAR.
  let ub_ok = (
    $ub.exit_code == 0
      and ($ub.stderr | str contains "fetching it")
      and ($resolve | str starts-with "resolve: 1 sources")
      and not ($resolve | str contains "/nix/store")
      and ($add | parse -r 'add: (?<n>\d+) paths, (?<b>\d+) bytes' | get -o 0.b | default "0" | into int) > 200
      and $tb3_valid
  )
  $res ++= [(verdict $ub_ok "build --no-build sends the input's NAR out of its spool and the daemon holds it" $"build --no-build \(exit ($ub.exit_code)\): ($resolve) | ($add) | ($tb3_path) valid=($tb3_valid)")]

  # a filtered `builtins.path` of an input member uploads the NAR evaluation
  # hashed: the daemon checks it against the store path.
  let upg = $"($work)/upflake2"
  mkdir $upg $"($work)/tb4src/sub"
  "b\n" | save $"($work)/tb4src/sub/b"
  "c\n" | save $"($work)/tb4src/sub/c"
  ^tar -C $work -czf $"($work)/tb4.tar.gz" tb4src
  let tb4_hash = (nixf hash path --type sha256 --sri $"($work)/tb4src" | complete | get stdout | chomp)
  r#'{
  inputs.tb.url = "file://@WORK@/tb4.tar.gz";
  inputs.tb.flake = false;
  outputs = { self, tb }: {
    packages.x86_64-linux.default = derivation {
      name = "usestbsub";
      system = "x86_64-linux";
      builder = "/bin/sh";
      args = [ "-c" "cat ${builtins.path { path = "${tb}/sub"; name = "tbsub"; filter = p: t: baseNameOf p != "c"; }}/b > $out" ];
    };
  };
}
'# | str replace -a "@WORK@" $work | save $"($upg)/flake.nix"
  {
    nodes: {
      tb: { flake: false, locked: { type: "tarball", url: $"file://($work)/tb4.tar.gz", narHash: $tb4_hash }, original: { type: "tarball", url: $"file://($work)/tb4.tar.gz" } }
      root: { inputs: { tb: "tb" } }
    }
    root: "root"
    version: 7
  } | to json | save $"($upg)/flake.lock"
  let fb = (^$ietsp build --no-build --ietsp-option builder daemon $"($upg)#packages.x86_64-linux.default" | complete)
  let fb_ok = ($fb.exit_code == 0 and ($fb.stderr | str contains "fetching it"))
  $res ++= [(verdict $fb_ok "build --no-build uploads a filtered copy of an input member" $"build --no-build \(exit ($fb.exit_code)\): ($fb.stderr | lines | last 1 | str join '')")]

  # one download per tree: a sibling holds the tree's fetch lock and stores
  # the entry before it lets go, so ietsp waits and then reads that entry,
  # with every download refused.
  let wf = $"($work)/waitflake"
  mkdir $wf $"($work)/tb5src"
  $"hello5 ($work)\n" | save $"($work)/tb5src/a"
  ^tar -C $work -czf $"($work)/tb5.tar.gz" tb5src
  let tb5_hash = (nixf hash path --type sha256 --sri $"($work)/tb5src" | complete | get stdout | chomp)
  '{ inputs.tb = { url = "file://@WORK@/tb5.tar.gz"; flake = false; }; outputs = { self, tb }: { v = builtins.readFile (tb + "/a"); }; }' | str replace -a "@WORK@" $work | save $"($wf)/flake.nix"
  {
    nodes: {
      tb: { flake: false, locked: { type: "tarball", url: $"file://($work)/tb5.tar.gz", narHash: $tb5_hash }, original: { type: "tarball", url: $"file://($work)/tb5.tar.gz" } }
      root: { inputs: { tb: "tb" } }
    }
    root: "root"
    version: 7
  } | to json | save $"($wf)/flake.lock"
  let no_daemon = {IETSP_NIX_BIN_DIR: $nobin, NIX_CONFIG: "store = unix:///nonexistent/ietsp-no-daemon"}
  let first = (with-env ($no_daemon | merge {IETSP_CACHE_DIR: $"($work)/waitsrc"}) { ^$ietsp eval --raw --no-cache $"($wf)#v" | complete })
  let entry5 = (try { ls $"($work)/waitsrc/inputs" | where type == dir | get 0.name } catch { "" })
  let wc = $"($work)/waitcache"
  mkdir $"($wc)/inputs"
  cp $"($work)/waitsrc/inputs/version" $"($wc)/inputs/"
  let lockf = $"($wc)/inputs/.($entry5 | path basename).fetching.tmp"
  let script = $"flock '($lockf)' sh -c 'sleep 1; cp -r ($entry5) ($wc)/inputs/' & sleep 0.3; '($ietsp)' eval --raw --no-cache '($wf)#v'; wait"
  let w = (with-env ($no_daemon | merge {IETSP_CACHE_DIR: $wc, IETSP_OFFLINE: "1"}) { ^bash -c $script | complete })
  let w_ok = ($first.exit_code == 0 and $w.exit_code == 0 and $w.stdout == $"hello5 ($work)\n" and ($w.stderr | str contains "waiting for another process to fetch input 'tb'"))
  $res ++= [(verdict $w_ok "tarball: a second process waits for the first one's download and reads its entry" $"fetch lock \(exit ($first.exit_code)/($w.exit_code)\): ($w.stdout) ($w.stderr | lines | last 2 | str join ' | ')")]

  # `nix flake prefetch-inputs`: both tarball inputs land in the input store,
  # with no nix to fall back on, and an evaluation then downloads nothing.
  let pf = $"($work)/prefetchflake"
  mkdir $pf $"($work)/tb6src"
  $"hello6 ($work)\n" | save $"($work)/tb6src/a"
  ^tar -C $work -czf $"($work)/tb6.tar.gz" tb6src
  let tb6_hash = (nixf hash path --type sha256 --sri $"($work)/tb6src" | complete | get stdout | chomp)
  '{ inputs.a = { url = "file://@WORK@/tb5.tar.gz"; flake = false; }; inputs.b = { url = "file://@WORK@/tb6.tar.gz"; flake = false; }; outputs = { self, a, b }: { v = builtins.readFile (a + "/a") + builtins.readFile (b + "/a"); }; }' | str replace -a "@WORK@" $work | save $"($pf)/flake.nix"
  let tbnode = {|f, h| { flake: false, locked: { type: "tarball", url: $"file://($work)/($f)", narHash: $h }, original: { type: "tarball", url: $"file://($work)/($f)" } } }
  {
    nodes: { a: (do $tbnode "tb5.tar.gz" $tb5_hash), b: (do $tbnode "tb6.tar.gz" $tb6_hash), root: { inputs: { a: "a", b: "b" } } }
    root: "root"
    version: 7
  } | to json | save $"($pf)/flake.lock"
  let pvars = ($no_daemon | merge {IETSP_CACHE_DIR: $"($work)/prefetchcache", PATH: $nobin})
  let p = (with-env $pvars { ^$ietsp nix flake prefetch-inputs $pf | complete })
  let pe = (with-env ($pvars | merge {IETSP_OFFLINE: "1"}) { ^$ietsp eval --raw --no-cache $"($pf)#v" | complete })
  let p_ok = ($p.exit_code == 0 and $p.stdout == "" and ($p.stderr | lines | where ($it | str contains "fetching it") | length) == 2
    and $pe.exit_code == 0 and $pe.stdout == $"hello5 ($work)\nhello6 ($work)\n" and not ($pe.stderr | str contains "fetching it"))
  $res ++= [(verdict $p_ok "prefetch-inputs: fetches every input, then an offline eval downloads nothing" $"prefetch-inputs \(exit ($p.exit_code)/($pe.exit_code)\): ($p.stderr | lines | last 2 | str join ' | ') / ($pe.stderr | lines | last 2 | str join ' | ')")]

  # the lock's narHash is the contract: a tarball whose contents changed is
  # refused, and nothing is registered. The input store entry must go first,
  # or the fetch never happens: the entry is the locked tree, already verified.
  rm -rf $tb_entry
  nixf store delete $tb_path | complete | ignore
  "changed\n" | save -f $"($work)/tb2src/a"
  ^tar -C $work -czf $"($work)/tb2.tar.gz" tb2src
  let mm = (with-env {IETSP_NIX_BIN_DIR: $nobin, PATH: $nobin} { ^$ietsp eval --raw --no-cache $"($tbf)#v" | complete })
  let mm_ok = ($mm.exit_code != 0 and $mm.stdout == "" and ($mm.stderr | str contains "NAR hash mismatch") and not ($tb_path | path exists))
  $res ++= [(verdict $mm_ok "tarball: a changed tarball is refused, not registered" $"tarball mismatch \(exit ($mm.exit_code)\): ($mm.stderr | lines | first 1 | str join '')")]

  # a `github` input over https: the public archive URL, one redirect to
  # codeload, gzip, the same tree nix has.
  let ghf = $"($work)/ghflake"
  mkdir $ghf
  r#'{
  inputs.tiny.url = "github:numtide/flake-utils/471aed544aef9c610ea465cddf4a39c358ac5aa8";
  inputs.tiny.flake = false;
  outputs = { self, tiny }: { v = toString (builtins.attrNames (builtins.readDir tiny)); };
}
'# | save $"($ghf)/flake.nix"
  do { cd $ghf; nixf flake lock } | complete | ignore
  let gh_exp = (nixf eval --raw --option eval-cache "false" $"($ghf)#v" | complete | get stdout | chomp)
  let gh_path = (input-path $ghf "tiny")
  nixf store delete $gh_path | complete | ignore
  $res ++= [(fetch-only $ietsp "github: input fetched over https by ietsp" $"($ghf)#v" $gh_exp $nobin)]

  # the standalone case: no nix, no daemon (nix.conf's `store` points at a
  # socket that is not there), a fresh cache. The first run fetches into the
  # input store and evaluates; the second evaluates again with IETSP_OFFLINE=1,
  # so any download would be an error.
  nixf store delete $gh_path | complete | ignore
  $res ++= [(standalone $ietsp "standalone: no nix, no daemon, fetch then an offline hit" $"($ghf)#v" $gh_exp $nobin $"($work)/alone")]

  # a path cache.nixos.org has and this machine has not: the daemon substitutes
  # it, so there is no download at all. The lock names a `github` input whose
  # coordinates are never used, because the narHash alone says where the tree
  # belongs.
  let src = (nixf eval --raw --impure --expr $"\(import ($nixpkgs) {}).sl.src.outPath" | complete | get stdout | chomp)
  let src_hash = (nixf path-info --store "https://cache.nixos.org" --json $src | complete | get stdout | from json | values | get 0.narHash)
  nixf store delete $src | complete | ignore
  let subf = $"($work)/subflake"
  mkdir $subf
  r#'{
  inputs.sub.url = "github:x/y";
  inputs.sub.flake = false;
  outputs = { self, sub }: { v = sub.outPath; };
}
'# | save $"($subf)/flake.nix"
  {
    nodes: {
      sub: { flake: false, locked: { type: "github", owner: "x", repo: "y", rev: "0000000000000000000000000000000000000000", narHash: $src_hash }, original: { type: "github", owner: "x", repo: "y" } }
      root: { inputs: { sub: "sub" } }
    }
    root: "root"
    version: 7
  } | to json | save $"($subf)/flake.lock"
  let sb = (with-env {IETSP_NIX_BIN_DIR: $nobin, PATH: $nobin} { ^$ietsp eval --raw --no-cache $"($subf)#v" | complete })
  let sb_ok = (($sb.stdout | chomp) == $src and not ($sb.stderr | str contains "cannot fetch"))
  $res ++= [(verdict $sb_ok "substitute: the daemon substitutes the input, ietsp downloads nothing" $"substitute \(exit ($sb.exit_code)\): ($sb.stdout | chomp) ($sb.stderr | lines | last 2 | str join ' | ')")]
  nixf store delete $src | complete | ignore

  # `git` and `hg` are the last shell-out: ietsp has no git fetcher, so a
  # missing git input goes to nix, and without nix it is an error that names it.
  let gitf = $"($work)/gitflake"
  mkdir $gitf
  r#'{
  inputs.lib.url = "git+file://@WORK@/lib";
  outputs = { self, lib }: { v = lib.greeting; };
}
'# | str replace -a "@WORK@" $work | save $"($gitf)/flake.nix"
  do { cd $gitf; nixf flake lock } | complete | ignore
  let git_exp = (nixf eval --raw --option eval-cache "false" $"($gitf)#v" | complete | get stdout | chomp)
  let git_path = (input-path $gitf "lib")
  nixf store delete $git_path | complete | ignore
  let gf = (^$ietsp eval --raw --no-cache $"($gitf)#v" | complete)
  let gf_ok = (($gf.stdout | chomp) == $git_exp and $git_exp != "")
  $res ++= [(verdict $gf_ok "git: a missing git input still goes through nix" $"git input \(exit ($gf.exit_code)\): ($gf.stdout | chomp) vs ($git_exp)")]
  nixf store delete $git_path | complete | ignore
  let gn = (with-env {IETSP_NIX_BIN_DIR: $nobin, PATH: $nobin} { ^$ietsp eval --raw --no-cache $"($gitf)#v" | complete })
  let gn_ok = ($gn.exit_code != 0 and ($gn.stderr | str contains "nix is not on PATH") and ($gn.stderr | str contains "input 'lib'"))
  $res ++= [(verdict $gn_ok "git: without nix the error names the input" $"git input, no nix \(exit ($gn.exit_code)\): ($gn.stderr | lines | last 2 | str join ' | ')")]
  # a git input whose tree is not the one the lock names: nix reports a NAR
  # hash mismatch, and ietsp's record names the usual causes
  let gm = $"($work)/gitmismatch"
  mkdir $gm
  cp $"($gitf)/flake.nix" $gm
  open --raw $"($gitf)/flake.lock" | from json | update nodes.lib.locked.narHash $"sha256-('ietsp-git-mismatch-never-stored!' | encode base64)" | to json | save $"($gm)/flake.lock"
  let gmr = (^$ietsp eval --raw --no-cache --log-format internal-json $"($gm)#v" | complete)
  let gm_rec = (records $gmr.stderr | where code? == "hash-mismatch" | get -o 0 | default {})
  $res ++= [(verdict ($gmr.exit_code == 1 and ($gm_rec.hint? | default "" | str contains "export-subst")) "git: a NAR hash mismatch is hash-mismatch, and the hint names filters, export-subst, submodules" $"git mismatch \(exit ($gmr.exit_code)\): ($gm_rec | to json -r)")]

  rm $"($repo)/flake.lock"
  let err = (^$ietsp eval --raw --no-cache $"($scratch).drvPath" | complete | get stderr)
  $res ++= [(verdict ($err | str contains "flake.lock missing or incomplete, run nix flake lock") "missing flake.lock is an error" $"missing flake.lock is an error \(($err)\)")]

  # a locked remote input that no substituter has and no forge serves: the two
  # messages name the input, exit is non-zero, stdout stays empty
  let ghost = $"($work)/ghost"
  mkdir $ghost
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
  let g = (with-env {IETSP_NIX_BIN_DIR: $nobin, PATH: $nobin} { ^$ietsp eval --raw --no-cache $"($ghost)#v" | complete })
  let named = ($g.stderr | lines | where ($it | str contains "input 'ghost'") | length)
  $res ++= [(verdict ($g.exit_code != 0 and $g.stdout == "" and $named == 2 and ($g.stderr | str contains "cannot fetch input 'ghost'")) "unfetchable input: both messages name it, the failed fetch was ietsp's own" $"unfetchable input \(exit ($g.exit_code)\): ($g.stderr | lines | first 2 | str join ' | ')")]
  # a lock entry without a narHash: the hint names the input and the command
  let unl = $"($work)/unlocked"
  mkdir $unl
  cp $"($ghost)/flake.nix" $unl
  open --raw $"($ghost)/flake.lock" | from json | reject nodes.ghost.locked.narHash | to json | save $"($unl)/flake.lock"
  let u = (^$ietsp eval --raw --no-cache --log-format internal-json $"($unl)#v" | complete)
  let u_rec = (records $u.stderr | where level? == 0 | get -o 0 | default {})
  $res ++= [(verdict ($u.exit_code == 1 and ($u_rec.hint? | default "" | str contains "run 'nix flake update ghost'")) "lock: an entry without narHash has a hint that names the input" $"unlocked entry \(exit ($u.exit_code)\): ($u_rec | to json -r)")]

  let gp = (with-env {IETSP_NIX_BIN_DIR: $nobin, PATH: $nobin} { ^$ietsp nix flake prefetch-inputs $ghost | complete })
  $res ++= [(verdict ($gp.exit_code == 1 and ($gp.stderr | str contains "cannot fetch input 'ghost'")) "prefetch-inputs: an unfetchable input is an error that names it, exit 1" $"prefetch-inputs ghost \(exit ($gp.exit_code)\): ($gp.stderr | lines | last 2 | str join ' | ')")]

  let fail = ($res | where not $it | length)
  print $"pass=($res | where $it | length) fail=($fail)"
  if $fail == 0 { 0 } else { 1 }
}

def main [] {
  let ietsp = (ietsp-bin)
  cd $ROOT
  let work = (isolate flaketest)
  run-in $work { run-tests $ietsp $work }
}

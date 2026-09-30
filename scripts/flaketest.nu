#!/usr/bin/env nu
# End-to-end flake evaluation: `iets eval --raw FLAKE#attr` must print what
# `nix eval --raw --option eval-cache false FLAKE#attr` prints, on this
# repo's flake, on a scratch flake with locked inputs of every offline type
# (`path:`, `git+file://`, a `file://` tarball), a `follows`, a devShell and a
# NixOS system, and on a scratch git flake with a `path:` input, a relative
# `path:` input (no `narHash` in the lock), a `follows`, a clean tree, a
# dirty tree, and a `git worktree` at a path nix has never copied (iets
# must not fetch the root tree). The only fetch is this repo's own nixpkgs
# input; the scratch flakes reuse that store tree through a `path:` input.
#
# It also covers the input store (docs/reference/flakes.md): a fetched input is kept
# there and not registered anywhere, `iets` alone evaluates such a flake with
# no nix, no daemon and a fresh cache, a second run does it with
# `IETS_OFFLINE=1`, and `build --no-build` sends the input's NAR out of the
# same spool.
use lib.nu [ROOT chomp iets-bin isolate online require-real-nix run-in show verdict]

def --wrapped nixf [...args: string] { ^nix --extra-experimental-features "nix-command flakes" ...$args }
def --wrapped git-in [repo: string, ...args: string] {
  ^git -c user.name=iets -c user.email=iets@example.com -c init.defaultBranch=main -C $repo ...$args
}

# the `--log-format internal-json` records in STDERR
def records [stderr: string]: nothing -> list<record> {
  $stderr | lines | where ($it | str starts-with "@nix ") | each { str substring 5.. | from json }
}

# check NAME REF ENV: both evaluators must agree and succeed. Prints the verdict, returns it.
def check [iets: string, name: string, ref: string, extra?: record]: nothing -> bool {
  let n = (nixf eval --raw --option eval-cache "false" $ref | complete)
  let i = (with-env ($extra | default {}) { ^$iets eval --raw --no-tracing-cache $ref | complete })
  let exp = ($n.stdout | chomp)
  let got = ($i.stdout | chomp)
  if $exp != "" and $exp == $got {
    print $"ok   ($name)"
    true
  } else {
    print $"FAIL ($name)"
    print $"  nix:   ($exp)"
    $n.stderr | lines | where not ($it | str contains "is dirty") | last 3 | show
    print $"  iets: ($got)"
    $i.stderr | lines | last 3 | show
    false
  }
}

# check-fresh NAME REF: like check, on a tree nix has never copied to the
# store. iets runs first, with the cache off, and must not fetch anything;
# nix then gives the expected value (and copies the tree).
def check-fresh [iets: string, name: string, ref: string]: nothing -> bool {
  let i = (with-env {IETS_CACHE: "0"} { ^$iets eval --raw --no-cache $ref | complete })
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
    print $"  iets: ($got)"
    $i.stderr | lines | last 3 | show
    false
  }
}

# check-error NAME REF: both must refuse, with the same `error:` line. iets
# runs with `--pure-eval`, which is what `nix eval` on a flake always has, and
# with the provenance rewrite off, so the message names the store path.
def check-error [iets: string, name: string, ref: string]: nothing -> bool {
  let first = {|s: string| $s | lines | each { str trim } | where ($it | str starts-with "error: ") | get -o 0 | default "" }
  let n = (nixf eval --raw --option eval-cache "false" $ref | complete)
  let i = (with-env {IETS_PROVENANCE: "0"} { ^$iets eval --raw --no-tracing-cache --pure-eval $ref | complete })
  let exp = (do $first $n.stderr)
  let got = (do $first $i.stderr)
  if $exp != "" and $exp == $got and $n.exit_code == $i.exit_code {
    print $"ok   ($name)"
    true
  } else {
    print $"FAIL ($name)"
    print $"  nix:   ($exp) \(exit ($n.exit_code)\)"
    print $"  iets: ($got) \(exit ($i.exit_code)\)"
    false
  }
}

# fetch-only NAME REF EXPECTED NOBIN: iets alone, with NOBIN (an empty
# directory) as the whole PATH and as IETS_NIX_BIN_DIR, so no nix exists to
# fall back on. It must say it is fetching the input and print EXPECTED.
def fetch-only [iets: string, name: string, ref: string, expected: string, nobin: string]: nothing -> bool {
  let i = (with-env {IETS_NIX_BIN_DIR: $nobin, PATH: $nobin} { ^$iets eval --raw --no-tracing-cache $ref | complete })
  let got = ($i.stdout | chomp)
  if $expected != "" and $got == $expected and ($i.stderr | str contains "fetching it") {
    print $"ok   ($name)"
    true
  } else {
    print $"FAIL ($name)"
    print $"  want:  ($expected)"
    print $"  iets: ($got)"
    $i.stderr | lines | last 3 | show
    false
  }
}

# standalone NAME REF EXPECTED NOBIN CACHE: iets with no nix on PATH, no
# IETS_NIX_BIN_DIR, an unreachable store (nix.conf's `store`, which
# `Connection::open` reads), and a cache root of its own. The first run must
# fetch into the input store and print EXPECTED; the second must print it
# again with IETS_OFFLINE=1, which makes any download an error.
def standalone [iets: string, name: string, ref: string, expected: string, nobin: string, cache: string]: nothing -> bool {
  let vars = {
    IETS_NIX_BIN_DIR: $nobin,
    PATH: $nobin,
    NIX_CONFIG: "store = unix:///nonexistent/iets-no-daemon",
    XDG_CACHE_HOME: $cache,
    IETS_CACHE_DIR: $"($cache)/iets",
  }
  rm -rf $cache
  let one = (with-env $vars { ^$iets eval --raw --no-tracing-cache $ref | complete })
  let two = (with-env ($vars | merge {IETS_OFFLINE: "1"}) { ^$iets eval --raw --no-tracing-cache $ref | complete })
  let entries = (try { ls $"($cache)/iets/inputs" | where type == dir | length } catch { 0 })
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
def cache-lines [iets: string, ref: string]: nothing -> list<string> {
  ^$iets eval --raw --timings $ref | complete | get stderr | lines | where $it =~ '^cache:'
}

def run-tests [iets: string, work: string]: nothing -> int {
  $env.IETS_CACHE_DIR = $"($work)/cache"
  mut res = []

  # ---- this repo ---------------------------------------------------------------
  $res ++= [(check $iets "repo packages.default" ".#packages.x86_64-linux.default.drvPath")]

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
  $res ++= [(check $iets "big: devShell" $"($big)#devShells.x86_64-linux.default.drvPath")]
  $res ++= [(check $iets "big: devShell, IETS_WORKERS=8" $"($big)#devShells.x86_64-linux.default.drvPath" {IETS_WORKERS: "8"})]
  $res ++= [(check $iets "big: nixosConfigurations.scratch toplevel" $"($big)#nixosConfigurations.scratch.config.system.build.toplevel.drvPath")]
  # the metadata of an input is silent until the input has a level of its own;
  # the site is the binding in the root flake, through nixpkgs's mkDerivation
  let quiet = (^$iets eval --raw --no-tracing-cache $"($big)#devShells.x86_64-linux.default.drvPath" | complete | get stderr)
  "[lint.inputs.lib]\ntree-meta-in-drv = \"warn\"\n" | save $"($big)/iets.toml"
  let loud = (^$iets eval --raw --no-tracing-cache $"($big)#devShells.x86_64-linux.default.drvPath" | complete | get stderr)
  rm $"($big)/iets.toml"
  let ok = (not ($quiet | str contains "tree-meta-in-drv")) and ($loud | str contains "attribute 'GREETING' holds 'inputs.lib.shortRev', a value that changes with every update of the lock file") and ($loud | str contains $"at ($big)/flake.nix:14:9")
  $res ++= [(verdict $ok "big: tree-meta-in-drv reports an input's metadata under [lint.inputs.lib]" $"tree-meta, input: ($quiet) | ($loud)")]

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
    visible = builtins.toJSON {
      context = builtins.getContext "${self.narHash} ${self.lastModifiedDate}";
      has = builtins.hasContext (builtins.substring 0 8 self.lastModifiedDate);
      equal = self.narHash == builtins.unsafeDiscardStringContext self.narHash;
      json = builtins.toJSON { inherit (self) narHash lastModifiedDate; };
      name = { ${self.lastModifiedDate} = builtins.replaceStrings [ "-" ] [ "+" ] self.narHash; };
      file = builtins.toFile "meta" self.narHash;
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
  $res ++= [(check-fresh $iets "worktree: clean tree nix never saw, nothing fetched" $"($fresh)#packages.x86_64-linux.default.drvPath")]
  "changed\n" | save -f $"($fresh)/file"
  $res ++= [(check-fresh $iets "worktree: dirty tree nix never saw, nothing fetched" $"($fresh)#packages.x86_64-linux.default.drvPath")]
  git-in $repo worktree remove --force $fresh

  # this repo's own flake from a fresh worktree
  let mine = $"($work)/iets-fresh"
  git-in $env.PWD worktree add -q $mine HEAD
  $res ++= [(check-fresh $iets "worktree: this repo's packages.default, nothing fetched" $"($mine)#packages.x86_64-linux.default.drvPath")]
  git-in $env.PWD worktree remove --force $mine

  $res ++= [(check $iets "scratch: clean tree" $"($scratch).drvPath")]
  # the marks on the metadata change nothing that Nix sees
  $res ++= [(check $iets "scratch: a string of the tree metadata has no context" $"($repo)#visible")]
  $res ++= [(check $iets "scratch: --raw on a derivation prints its outPath" $scratch)]
  "untracked\n" | save $"($repo)/untracked"
  $res ++= [(check $iets "scratch: untracked file is ignored" $"($scratch).drvPath")]
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
  let ut_i = (^$iets eval --raw --no-tracing-cache --log-format internal-json $"($ut)#v" | complete)
  let ut_rec = (records $ut_i.stderr | where level? == 0 | get -o 0 | default {})
  let ut_ok = ($ut_n.exit_code == 1 and ($ut_n.stderr | str contains $ut_line) and $ut_i.exit_code == 1
    and ($ut_rec.raw_msg? | default "" | str starts-with $ut_line)
    and ($ut_rec.hint? | default "") == $"run 'git -C \"($ut)\" add \"extra\"'")
  $res ++= [(verdict $ut_ok "git: reading an untracked file is Nix's error, the hint is the git add" $"untracked read \(exit ($ut_n.exit_code)/($ut_i.exit_code)\): ($ut_rec | to json -r)")]
  "three\n" | save -f $"($repo)/file"
  $res ++= [(check $iets "scratch: dirty tree (modified tracked file)" $"($scratch).drvPath")]
  let held = (^$iets eval --raw --no-tracing-cache $"($scratch).drvPath" | complete | get stderr)
  $res ++= [(verdict ($held | str contains "holds 'self.dirtyRev', 'self.dirtyShortRev', 'self.lastModifiedDate', 'self.narHash'") "scratch: tree-meta-in-drv names the metadata of a dirty tree" $"tree-meta, dirty: ($held)")]
  rm $"($repo)/nested/tracked"
  $res ++= [(check $iets "scratch: dirty tree (deleted tracked file)" $"($scratch).drvPath")]
  "four\n" | save $"($repo)/added"
  git-in $repo add added
  $res ++= [(check $iets "scratch: dirty tree (staged new file)" $"($scratch).drvPath")]
  git-in $repo commit -q -am next
  $res ++= [(check $iets "scratch: clean again after commit" $"($scratch).drvPath")]
  let held = (^$iets eval --raw --no-tracing-cache $"($scratch).drvPath" | complete | get stderr)
  $res ++= [(verdict ($held | str contains "[tree-meta-in-drv] derivation 'scratch': attribute 'args' holds 'self.lastModifiedDate', 'self.narHash', 'self.rev', 'self.shortRev'") "scratch: tree-meta-in-drv names the metadata of a clean tree" $"tree-meta, clean: ($held)")]
  $res ++= [(check $iets "scratch: subdirectory flake ref" $"($repo)/.#packages.x86_64-linux.default.drvPath")]
  $res ++= [(check $iets "scratch: path: ref copies the whole tree" $"path:($repo)#packages.x86_64-linux.default.drvPath")]

  let exp = (nixf eval --json --option eval-cache "false" $"($scratch).drvPath" | complete | get stdout | chomp)
  let got = (^$iets eval --json --no-tracing-cache $"($scratch).drvPath" | complete | get stdout | chomp)
  $res ++= [(verdict ($exp != "" and $exp == $got) "scratch: --json" $"scratch: --json \(($exp) vs ($got)\)")]

  # tracing cache: hit after a miss, miss after the tree changes
  ^$iets eval --raw $"($scratch).drvPath" | complete | ignore
  let st = (cache-lines $iets $"($scratch).drvPath" | str join "\n")
  $res ++= [(verdict ($st | str contains "cache: hit") "cache: second run hits" $"cache: second run hits \(($st)\)")]
  # the date and the count follow from the revision, so they are no
  # observations: a validation starts no `git log` and no `git rev-list`
  let cmds = (glob $"($work)/cache/trace/*/entry" | each {|e| ^$iets cache dump $e | complete | get stdout | lines } | flatten | where ($it | str starts-with $"cmd ($repo) ") | each {|l| $l | split row " " | get 3 } | uniq | sort)
  $res ++= [(verdict ($cmds == [ls-files rev-parse status]) "cache: the git observations are ls-files, status and rev-parse" $"cache: git observations: ($cmds | str join ' ')")]
  "five\n" | save -f $"($repo)/file"
  let st = (cache-lines $iets $"($scratch).drvPath" | get -o 0 | default "")
  $res ++= [(verdict ($st | str contains "miss (") "cache: editing a tracked file misses" $"cache: editing a tracked file misses \(($st)\)")]
  git-in $repo commit -q -am five
  let st = (cache-lines $iets $"($scratch).drvPath" | get -o 0 | default "")
  $res ++= [(verdict ($st =~ 'miss \(.*git') "cache: a commit misses on a git command" $"cache: a commit misses on a git command \(($st)\)")]

  # ---- revCount: there when Nix has it, counted when it is read ----------------
  let cnt = $"($work)/count"
  mkdir $cnt $"($work)/reader"
  '{ outputs = { self }: { count = t: t.revCount; }; }' + "\n" | save $"($work)/reader/flake.nix"
  r#'{
  inputs.reader.url = "path:@WORK@/reader";
  outputs = { self, reader }:
    let
      s = self;
      f = { self, ... }: self.revCount;
      info = removeAttrs self.sourceInfo [ "outPath" "submodules" ];
      name = "rev" + "Count";
    in {
    meta = builtins.toJSON {
      count = self.revCount or null;
      has = self ? revCount;
      names = builtins.attrNames self.sourceInfo;
      date = self.lastModified;
    };
    names = builtins.toJSON [ (self ? revCount) (builtins.attrNames self) (let n = self.revCount; in 1) ];
    again = toString self.revCount;
    twice = toString (self.revCount + self.sourceInfo.revCount);
    site = {
      select = toString self.revCount;
      default = toString (self.revCount or 0);
      inherited = toString { inherit (self) revCount; }.revCount;
      info = toString self.sourceInfo.revCount;
      bound = toString s.revCount;
      argument = toString (f { inherit self; });
      builtin = toString (builtins.getAttr "revCount" self);
      input = toString (reader.count self);
    };
    forced = {
      json = builtins.toJSON info;
      values = toString (builtins.attrValues info);
      computed = toString self.${name};
      mapped = toString (builtins.mapAttrs (n: v: v) info).revCount;
      scope = with self; toString revCount;
    };
    silent = {
      allowed = toString self.revCount; # iets: allow rev-count
      other = toString { revCount = 2; }.revCount;
      whole = builtins.toJSON self;
    };
    all = builtins.toJSON [ self.site self.forced self.silent ];
  };
}
'# | str replace "@WORK@" $work | save $"($cnt)/flake.nix"
  do { cd $cnt; nixf flake lock } | complete | ignore
  git-in $cnt init -q
  git-in $cnt add -A
  git-in $cnt commit -q -m one
  git-in $cnt commit -q --allow-empty -m two
  $res ++= [(check $iets "revCount: clean tree" $"($cnt)#meta")]
  let shallow = $"($work)/count-shallow"
  git-in $work clone -q --depth 1 $"file://($cnt)" $shallow
  $res ++= [(check $iets "revCount: shallow clone" $"($shallow)#meta")]
  # `git rev-list` fails here: a run that does not read the count starts
  # none, and a run that reads it takes the count that the store kept
  let nolist = $"($work)/nolist"
  mkdir $nolist
  $"#!/bin/sh\ncase \"$1\" in rev-list\) exit 9 ;; esac\nexec (which git | get 0.path) \"$@\"\n" | save $"($nolist)/git"
  chmod 755 $"($nolist)/git"
  let no_list = {PATH: ($env.PATH | prepend $nolist)}
  let lazy = (with-env $no_list { ^$iets eval --raw --no-tracing-cache $"($cnt)#names" | complete })
  let none = (with-env $no_list { ^$iets eval --raw --no-tracing-cache $"($cnt)#again" | complete })
  ^$iets eval --raw $"($cnt)#meta" | complete | ignore
  let kept = (with-env $no_list { ^$iets eval --raw $"($cnt)#again" | complete })
  let ok = ($lazy.exit_code == 0 and ($lazy.stdout | str starts-with "[true,") and $none.exit_code == 1 and ($none.stderr | str contains "`git rev-list --count") and $kept.exit_code == 0 and $kept.stdout == "2")
  $res ++= [(verdict $ok "revCount: lazy, and kept by revision" $"revCount: lazy \(exit ($lazy.exit_code)\) ($lazy.stdout), no store \(exit ($none.exit_code)\) ($none.stderr | lines | last 2 | str join ' | '), kept \(exit ($kept.exit_code)\) ($kept.stdout) ($kept.stderr | lines | last 2 | str join ' | ')")]
  # rev-count: a read of the count is one finding for each tree, with its site,
  # from a cache hit too; the names, `?` and an unused binding read nothing
  let finding = "[rev-count] 'self.revCount' is deprecated: the count of all revisions is expensive to compute"
  let found = {|r| records $r.stderr | where code? == "lint" | where ($it.raw_msg | str starts-with $finding) | length }
  let sites = {|r| records $r.stderr | where code? == "lint" | get -o start }
  let json = [--raw --timings --log-format internal-json --cache-dir $"($work)/count-cache" $"($cnt)#twice"]
  let r1 = (^$iets eval ...$json | complete)
  let r2 = (^$iets eval ...$json | complete)
  let ok = ($r1.stdout == "4" and $r2.stdout == "4" and (do $found $r1) == 1 and (do $found $r2) == 1 and (do $sites $r1 | compact | length) == 1 and (do $sites $r1) == (do $sites $r2) and ($r2.stderr | str contains "cache: hit") and not ($lazy.stderr | str contains "rev-count"))
  $res ++= [(verdict $ok "rev-count: one finding for a tree, on a cache hit too, none for the names" $"rev-count: ($r1.stderr) | ($r2.stderr) | ($lazy.stderr)")]
  let r = (^$iets eval --raw --no-tracing-cache --lint rev-count=error $"($cnt)#again" | complete)
  $res ++= [(verdict ($r.exit_code == 1 and ($r.stderr | str starts-with $"error: ($finding)")) "rev-count: --lint sets the level" $"rev-count: error: rc=($r.exit_code) ($r.stderr)")]
  # a selection of the count is the site, in the code of an input too: the
  # span covers these bytes. A read without a selection has no site
  let covered = {|r|
    let recs = (records $r.stderr | where code? == "lint" | where ($it.raw_msg | str starts-with $finding))
    let rec = ($recs | get -o 0 | default {})
    if $r.exit_code != 0 or ($recs | length) != 1 {
      $"exit ($r.exit_code), ($recs | length) findings"
    } else if $rec.start? == null or $rec.line? == null {
      "no site"
    } else {
      open --raw ($rec.file | str replace --regex ':\d+:\d+$' '') | into binary | bytes at ($rec.start)..<($rec.end) | decode utf-8
    }
  }
  let want = {
    "site.select": "self.revCount"
    "site.default": "self.revCount or 0"
    "site.inherited": "revCount"
    "site.info": "self.sourceInfo.revCount"
    "site.bound": "s.revCount"
    "site.argument": "self.revCount"
    "site.builtin": "builtins.getAttr \"revCount\" self"
    "site.input": "t.revCount"
    "forced.json": "no site"
    "forced.values": "no site"
    "forced.computed": "no site"
    "forced.mapped": "no site"
    "forced.scope": "no site"
    "silent.allowed": "exit 0, 0 findings"
    "silent.other": "exit 0, 0 findings"
    "silent.whole": "exit 0, 0 findings"
  }
  let bad = ($want | transpose name text | each {|w|
    let got = (do $covered (^$iets eval --raw --no-tracing-cache --log-format internal-json $"($cnt)#($w.name)" | complete))
    if $got == $w.text { null } else { $"($w.name): ($got)" }
  } | compact)
  $res ++= [(verdict ($bad | is-empty) "rev-count: a selection is the site, a read without one has none" $"rev-count: sites: ($bad | str join ' | ')")]
  $res ++= [(check $iets "revCount: each spelling gives the value of Nix" $"($cnt)#all")]
  # the count of a locked input comes from the lock; it is silent until the
  # input has a level of its own
  let cin = $"($work)/count-in"
  mkdir $cin
  '{ inputs.lib.url = "git+file://@WORK@/lib"; outputs = { self, lib }: { n = toString lib.revCount; }; }' + "\n" | str replace "@WORK@" $work | save $"($cin)/flake.nix"
  do { cd $cin; nixf flake lock } | complete | ignore
  $res ++= [(check $iets "revCount: locked input" $"($cin)#n")]
  let quiet = (^$iets eval --raw --no-tracing-cache $"($cin)#n" | complete | get stderr)
  "[lint.inputs.lib]\nrev-count = \"warn\"\n" | save $"($cin)/iets.toml"
  let loud = (^$iets eval --raw --no-tracing-cache $"($cin)#n" | complete | get stderr)
  rm $"($cin)/iets.toml"
  let ok = (not ($quiet | str contains "rev-count")) and ($loud | str contains "warning: [rev-count] 'inputs.lib.revCount' is deprecated") and ($loud | str contains $"\n  at ($cin)/flake.nix:1:")
  $res ++= [(verdict $ok "rev-count: an input is silent without [lint.inputs.lib]" $"rev-count, input: ($quiet) | ($loud)")]
  "\n" | save --append $"($cnt)/flake.nix"
  $res ++= [(check $iets "revCount: dirty tree" $"($cnt)#meta")]

  # ---- fetching an input, with no nix to fall back on -------------------------
  # `nobin` is empty, so IETS_NIX_BIN_DIR and PATH find no nix at all: every
  # case below is iets's own substitution or its own fetch.
  let nobin = $"($work)/nobin"
  mkdir $nobin

  # a `file://` tarball, offline: nix locks it, the store copy goes away, and
  # iets downloads, unpacks (one leading component stripped), hashes and
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
  $res ++= [(fetch-only $iets "tarball: file:// input indexed into iets's input store" $"($tbf)#v" $tb_exp $nobin)]
  # nothing was registered: the tree lives in the input store, not the store
  let tb_entry = $"($work)/cache/inputs/($tb_path | path basename)"
  let virt_ok = (not ($tb_path | path exists)) and ($"($tb_entry)/spool" | path exists) and ($"($tb_entry)/index" | path exists)
  $res ++= [(verdict $virt_ok "tarball: the fetch wrote the input store, not /nix/store" $"input store entry ($tb_entry)")]

  # `iets build --no-build` is the only thing that uploads an input, and it
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
  let ub = (^$iets build --no-build --timings --iets-option builder daemon $"($upf)#packages.x86_64-linux.default" | complete)
  let tb3_path = (^$iets eval --raw --no-tracing-cache -E $"\(builtins.fetchTree { type = \"tarball\"; url = \"file://($work)/tb3.tar.gz\"; narHash = \"($tb3_hash)\"; }).outPath" | complete | get stdout | str trim)
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
  let fb = (^$iets build --no-build --iets-option builder daemon $"($upg)#packages.x86_64-linux.default" | complete)
  let fb_ok = ($fb.exit_code == 0 and ($fb.stderr | str contains "fetching it"))
  $res ++= [(verdict $fb_ok "build --no-build uploads a filtered copy of an input member" $"build --no-build \(exit ($fb.exit_code)\): ($fb.stderr | lines | last 1 | str join '')")]

  # one download per tree: a sibling holds the tree's fetch lock and stores
  # the entry before it lets go, so iets waits and then reads that entry,
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
  let no_daemon = {IETS_NIX_BIN_DIR: $nobin, NIX_CONFIG: "store = unix:///nonexistent/iets-no-daemon"}
  let first = (with-env ($no_daemon | merge {IETS_CACHE_DIR: $"($work)/waitsrc"}) { ^$iets eval --raw --no-tracing-cache $"($wf)#v" | complete })
  let entry5 = (try { ls $"($work)/waitsrc/inputs" | where type == dir | get 0.name } catch { "" })
  let wc = $"($work)/waitcache"
  mkdir $"($wc)/inputs"
  cp $"($work)/waitsrc/inputs/version" $"($wc)/inputs/"
  let lockf = $"($wc)/inputs/.($entry5 | path basename).fetching.tmp"
  let script = $"flock '($lockf)' sh -c 'sleep 1; cp -r ($entry5) ($wc)/inputs/' & sleep 0.3; '($iets)' eval --raw --no-tracing-cache '($wf)#v'; wait"
  let w = (with-env ($no_daemon | merge {IETS_CACHE_DIR: $wc, IETS_OFFLINE: "1"}) { ^bash -c $script | complete })
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
  let pvars = ($no_daemon | merge {IETS_CACHE_DIR: $"($work)/prefetchcache", PATH: $nobin})
  let p = (with-env $pvars { ^$iets nix flake prefetch-inputs $pf | complete })
  let pe = (with-env ($pvars | merge {IETS_OFFLINE: "1"}) { ^$iets eval --raw --no-tracing-cache $"($pf)#v" | complete })
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
  let mm = (with-env {IETS_NIX_BIN_DIR: $nobin, PATH: $nobin} { ^$iets eval --raw --no-tracing-cache $"($tbf)#v" | complete })
  let mm_ok = ($mm.exit_code != 0 and $mm.stdout == "" and ($mm.stderr | str contains "NAR hash mismatch") and not ($tb_path | path exists))
  $res ++= [(verdict $mm_ok "tarball: a changed tarball is refused, not registered" $"tarball mismatch \(exit ($mm.exit_code)\): ($mm.stderr | lines | first 1 | str join '')")]

  # ---- the cases that need the network: github.com and cache.nixos.org
  if (online "a github input, the standalone fetch, a substituted input") {
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
    $res ++= [(fetch-only $iets "github: input fetched over https by iets" $"($ghf)#v" $gh_exp $nobin)]

    # the standalone case: no nix, no daemon (nix.conf's `store` points at a
    # socket that is not there), a fresh cache. The first run fetches into the
    # input store and evaluates; the second evaluates again with IETS_OFFLINE=1,
    # so any download would be an error.
    nixf store delete $gh_path | complete | ignore
    $res ++= [(standalone $iets "standalone: no nix, no daemon, fetch then an offline hit" $"($ghf)#v" $gh_exp $nobin $"($work)/alone")]

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
    let sb = (with-env {IETS_NIX_BIN_DIR: $nobin, PATH: $nobin} { ^$iets eval --raw --no-tracing-cache $"($subf)#v" | complete })
    let sb_ok = (($sb.stdout | chomp) == $src and not ($sb.stderr | str contains "cannot fetch"))
    $res ++= [(verdict $sb_ok "substitute: the daemon substitutes the input, iets downloads nothing" $"substitute \(exit ($sb.exit_code)\): ($sb.stdout | chomp) ($sb.stderr | lines | last 2 | str join ' | ')")]
    nixf store delete $src | complete | ignore
  }

  # `git` and `hg` are the last shell-out: iets has no git fetcher, so a
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
  let gf = (^$iets eval --raw --no-tracing-cache $"($gitf)#v" | complete)
  let gf_ok = (($gf.stdout | chomp) == $git_exp and $git_exp != "")
  $res ++= [(verdict $gf_ok "git: a missing git input still goes through nix" $"git input \(exit ($gf.exit_code)\): ($gf.stdout | chomp) vs ($git_exp)")]
  nixf store delete $git_path | complete | ignore
  let gn = (with-env {IETS_NIX_BIN_DIR: $nobin, PATH: $nobin} { ^$iets eval --raw --no-tracing-cache $"($gitf)#v" | complete })
  let gn_ok = ($gn.exit_code != 0 and ($gn.stderr | str contains "nix is not on PATH") and ($gn.stderr | str contains "input 'lib'"))
  $res ++= [(verdict $gn_ok "git: without nix the error names the input" $"git input, no nix \(exit ($gn.exit_code)\): ($gn.stderr | lines | last 2 | str join ' | ')")]
  # a git input whose tree is not the one the lock names: nix reports a NAR
  # hash mismatch, and iets's record names the usual causes
  let gm = $"($work)/gitmismatch"
  mkdir $gm
  cp $"($gitf)/flake.nix" $gm
  open --raw $"($gitf)/flake.lock" | from json | update nodes.lib.locked.narHash $"sha256-('iets-git-mismatch-never-stored!!' | encode base64)" | to json | save $"($gm)/flake.lock"
  let gmr = (^$iets eval --raw --no-tracing-cache --log-format internal-json $"($gm)#v" | complete)
  let gm_rec = (records $gmr.stderr | where code? == "hash-mismatch" | get -o 0 | default {})
  $res ++= [(verdict ($gmr.exit_code == 1 and ($gm_rec.hint? | default "" | str contains "export-subst")) "git: a NAR hash mismatch is hash-mismatch, and the hint names filters, export-subst, submodules" $"git mismatch \(exit ($gmr.exit_code)\): ($gm_rec | to json -r)")]

  rm $"($repo)/flake.lock"
  let err = (^$iets eval --raw --no-tracing-cache $"($scratch).drvPath" | complete | get stderr)
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
  let g = (with-env {IETS_NIX_BIN_DIR: $nobin, PATH: $nobin} { ^$iets eval --raw --no-tracing-cache $"($ghost)#v" | complete })
  let named = ($g.stderr | lines | where ($it | str contains "input 'ghost'") | length)
  $res ++= [(verdict ($g.exit_code != 0 and $g.stdout == "" and $named == 2 and ($g.stderr | str contains "cannot fetch input 'ghost'")) "unfetchable input: both messages name it, the failed fetch was iets's own" $"unfetchable input \(exit ($g.exit_code)\): ($g.stderr | lines | first 2 | str join ' | ')")]
  # a lock entry without a narHash: the hint names the input and the command
  let unl = $"($work)/unlocked"
  mkdir $unl
  cp $"($ghost)/flake.nix" $unl
  open --raw $"($ghost)/flake.lock" | from json | reject nodes.ghost.locked.narHash | to json | save $"($unl)/flake.lock"
  let u = (^$iets eval --raw --no-tracing-cache --log-format internal-json $"($unl)#v" | complete)
  let u_rec = (records $u.stderr | where level? == 0 | get -o 0 | default {})
  $res ++= [(verdict ($u.exit_code == 1 and ($u_rec.hint? | default "" | str contains "run 'nix flake update ghost'")) "lock: an entry without narHash has a hint that names the input" $"unlocked entry \(exit ($u.exit_code)\): ($u_rec | to json -r)")]

  let gp = (with-env {IETS_NIX_BIN_DIR: $nobin, PATH: $nobin} { ^$iets nix flake prefetch-inputs $ghost | complete })
  $res ++= [(verdict ($gp.exit_code == 1 and ($gp.stderr | str contains "cannot fetch input 'ghost'")) "prefetch-inputs: an unfetchable input is an error that names it, exit 1" $"prefetch-inputs ghost \(exit ($gp.exit_code)\): ($gp.stderr | lines | last 2 | str join ' | ')")]


  # ---- symlinks in a working tree, and the tree the flake directory names ----
  # iets reads the root tree from the working tree, so a link must resolve
  # where the store copy puts it: inside the copy, under the tracked file set.
  let sym = $"($work)/sym"
  mkdir $"($sym)/d"
  "outside\n" | save $"($work)/outside.txt"
  r#'{
  outputs = { self }: {
    esc = builtins.readFile (self + "/esc");
    abs = builtins.readFile (self + "/abs");
    unt = builtins.readFile (self + "/unt");
    ok = builtins.readFile (self + "/okdir/t.txt");
    okdir = builtins.toJSON (builtins.readDir (self + "/okdir"));
    okex = builtins.toJSON (builtins.pathExists (self + "/okdir"));
    nar = self.narHash;
  };
}
'# | save $"($sym)/flake.nix"
  '{"nodes":{"root":{}},"root":"root","version":7}' | save $"($sym)/flake.lock"
  "tracked\n" | save $"($sym)/d/t.txt"
  "untracked\n" | save $"($sym)/untracked.txt"
  ^ln -s ../outside.txt $"($sym)/esc"
  ^ln -s /nonexistent/iets-symlink-test $"($sym)/abs"
  ^ln -s untracked.txt $"($sym)/unt"
  ^ln -s d $"($sym)/okdir"
  git-in $sym init -q
  git-in $sym add flake.nix flake.lock d/t.txt esc abs unt okdir
  git-in $sym commit -q -m sym
  $res ++= [(check-error $iets "sym: a link out of the tree is refused" $"($sym)#esc")]
  $res ++= [(check-error $iets "sym: an absolute link is refused" $"($sym)#abs")]
  $res ++= [(check-error $iets "sym: a link to an untracked file is Nix's git error" $"($sym)#unt")]
  $res ++= [(check $iets "sym: readFile through a tracked link to a tracked directory" $"($sym)#ok")]
  $res ++= [(check $iets "sym: readDir of a tracked link to a tracked directory" $"($sym)#okdir")]
  $res ++= [(check $iets "sym: pathExists of a tracked link to a tracked directory" $"($sym)#okex")]
  # a flake directory that is a link to a working tree is that tree, not the
  # link object: git.cc opens the repository, which resolves the path
  ^ln -s sym $"($work)/sym-link"
  $res ++= [(check $iets "sym: a flake directory that is a link is the working tree" $"($work)/sym-link#nar")]

  # ---- a sparse checkout is a dirty tree ------------------------------------
  # `git status` never reports a skip-worktree entry, but libgit2's status,
  # which getWorkdirInfo reads, calls an index entry that is not on disk
  # deleted, and that makes the tree dirty: no rev and no revCount.
  let sparse = $"($work)/sparse"
  mkdir $"($sparse)/d"
  '{ outputs = { self }: { v = builtins.toJSON [ (self.rev or "none") (self.dirtyRev or "none") self.narHash (self.revCount or (-1)) ]; }; }' + "\n" | save $"($sparse)/flake.nix"
  '{"nodes":{"root":{}},"root":"root","version":7}' | save $"($sparse)/flake.lock"
  "x\n" | save $"($sparse)/d/f.txt"
  "y\n" | save $"($sparse)/top.txt"
  git-in $sparse init -q
  git-in $sparse add -A
  git-in $sparse commit -q -m init
  $res ++= [(check $iets "sparse: the whole tree is clean" $"($sparse)#v")]
  git-in $sparse sparse-checkout set --no-cone "/*" "!/d/"
  $res ++= [(check $iets "sparse: an index entry that is not on disk makes it dirty" $"($sparse)#v")]

  # ---- a git working tree still caches ----------------------------------------
  # every traced git query must be in `trace::Query`, or the run is uncacheable
  # and a git flake silently loses the tracing cache.
  let cacheable = $"($work)/cacheable"
  mkdir $cacheable
  '{ outputs = { self }: { v = "c"; }; }' + "\n" | save $"($cacheable)/flake.nix"
  '{"nodes":{"root":{}},"root":"root","version":7}' | save $"($cacheable)/flake.lock"
  git-in $cacheable init -q
  git-in $cacheable add -A
  let cold = (^$iets eval --timings --raw $"($cacheable)#v" | complete)
  let warm = (^$iets eval --timings --raw $"($cacheable)#v" | complete)
  $res ++= [(verdict (($cold.stderr | str contains "cache: wrote") and ($warm.stderr | str contains "cache: hit"))
    "git: a working tree evaluation is cacheable"
    $"cold ($cold.stderr | lines | last 2 | str join ' | '), warm ($warm.stderr | lines | last 2 | str join ' | ')")]

  # ---- a working tree that appears above a path flake -------------------------
  # the detection probes every ancestor, and those probes are observations:
  # a cached result must not stay a path flake after `git init` above it.
  let above = $"($work)/above"
  mkdir $"($above)/fl"
  '{ outputs = { self }: { v = builtins.toJSON [ (self ? rev) (builtins.pathExists (self + "/b")) ]; }; }' + "\n" | save $"($above)/fl/flake.nix"
  '{"nodes":{"root":{}},"root":"root","version":7}' | save $"($above)/fl/flake.lock"
  "b\n" | save $"($above)/fl/b"
  let before = (^$iets eval --raw $"($above)/fl#v" | complete)
  git-in $above init -q
  let after = (^$iets eval --raw $"($above)/fl#v" | complete)
  let real = (nixf eval --raw --option eval-cache "false" $"($above)/fl#v" | complete)
  let ok = ($before.exit_code == 0 and $before.stdout == "[false,true]" and $after.exit_code == $real.exit_code
    and $after.exit_code == 1 and ($after.stderr | str contains "is not tracked by Git"))
  $res ++= [(verdict $ok "above: git init above a cached path flake invalidates the entry" $"above: before ($before.stdout), after \(exit ($after.exit_code)\) ($after.stderr | lines | last 2 | str join ' | '), nix exit ($real.exit_code)")]
  let fail = ($res | where not $it | length)
  print $"pass=($res | where $it | length) fail=($fail)"
  if $fail == 0 { 0 } else { 1 }
}

def main [] {
  require-real-nix nix
  let iets = (iets-bin)
  cd $ROOT
  let work = (isolate flaketest)
  run-in $work { run-tests $iets $work }
}

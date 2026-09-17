#!/usr/bin/env nu
# End-to-end flake evaluation: `ietsp eval --raw FLAKE#attr` must print what
# `nix eval --raw --option eval-cache false FLAKE#attr` prints, on real
# flakes and on a scratch git flake with a `path:` input, a `follows`, a
# clean tree and a dirty tree.
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

# the `cache:` lines of a --timings run
def cache-lines [ietsp: string, ref: string]: nothing -> list<string> {
  ^$ietsp eval --raw --timings $ref | complete | get stderr | lines | where $it =~ '^cache:'
}

def run-tests [ietsp: string, work: string]: nothing -> int {
  $env.IETSP_CACHE_DIR = $"($work)/cache"
  mut res = []

  let home = "/home/zimbatm/home"
  let host = (nixf eval --json $"($home)#nixosConfigurations" --apply builtins.attrNames err> /dev/null | from json | first)
  $res ++= [(check $ietsp "home devShell" $"($home)#devShells.x86_64-linux.default.drvPath")]
  $res ++= [(check $ietsp "home devShell, IETSP_WORKERS=8" $"($home)#devShells.x86_64-linux.default.drvPath" {IETSP_WORKERS: "8"})]
  $res ++= [(check $ietsp $"home nixosConfigurations.($host) toplevel" $"($home)#nixosConfigurations.($host).config.system.build.toplevel.drvPath")]
  $res ++= [(check $ietsp "hairesis muster" "/home/zimbatm/hairesis#packages.x86_64-linux.muster.drvPath")]
  $res ++= [(check $ietsp "iets-prime default" "/home/zimbatm/iets-prime#packages.x86_64-linux.default.drvPath")]

  # ---- scratch flake ---------------------------------------------------------
  let repo = $"($work)/repo"
  mkdir $"($work)/sub" $"($work)/dep" $repo
  '{ outputs = { self }: { value = "sub-" + self.narHash; }; }' + "\n" | save $"($work)/sub/flake.nix"
  r#'{
  inputs.sub.url = "path:@WORK@/sub";
  outputs = { self, sub }: { value = "dep-" + sub.value; inherit sub; };
}
'# | str replace -a "@WORK@" $work | save $"($work)/dep/flake.nix"
  r#'{
  inputs.sub.url = "path:@WORK@/sub";
  inputs.dep.url = "path:@WORK@/dep";
  inputs.dep.inputs.sub.follows = "sub";
  outputs = { self, dep, sub }: {
    packages.x86_64-linux.default = derivation {
      name = "scratch";
      system = "x86_64-linux";
      builder = "/bin/sh";
      args = [ "-c" "echo ${self.narHash} ${self.rev or self.dirtyRev or "norev"} ${self.shortRev or self.dirtyShortRev or "norev"} ${toString self.lastModified} ${self.lastModifiedDate} ${toString (self.revCount or 0)} ${dep.value} ${dep.sub.value} ${dep.outPath} ${sub.outPath} ${self}/file ${builtins.readFile (self + "/file")} > $out" ];
    };
  };
}
'# | str replace -a "@WORK@" $work | save $"($repo)/flake.nix"
  "one\n" | save $"($repo)/file"
  mkdir $"($repo)/nested"
  "two\n" | save $"($repo)/nested/tracked"
  git-in $repo init -q
  git-in $repo add -A
  git-in $repo commit -q -m init
  do { cd $repo; nixf flake lock } | complete | ignore
  git-in $repo add flake.lock
  git-in $repo commit -q -m lock
  let scratch = $"($repo)#packages.x86_64-linux.default"

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

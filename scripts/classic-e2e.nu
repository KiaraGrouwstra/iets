#!/usr/bin/env nu
# Classic mode against the real nix: for every case, run the real
# nix-instantiate/nix-build and ours and compare stdout byte for byte and the
# exit code. Needs a live nix-daemon and `nix` on PATH (the dev shell has
# nix 2.34.8); it writes to the store, so it is not a flake check.
#
# usage: nu scripts/classic-e2e.nu
#        BIN=target/debug nu scripts/classic-e2e.nu
use lib.nu [chomp show verdict]

const NIXPKGS = "/nix/store/ah3pxz4xa52hd6vhbx17vcsci0b8bmbr-source"

def real-bin-dir []: nothing -> string {
  (^readlink -f (^bash -c "command -v nix-instantiate" | chomp) | chomp | path dirname)
}

# The symlink farm: our binary under the classic names.
def make-farm [bin: string, dir: string]: nothing -> string {
  mkdir $"($dir)/bin"
  for n in [nix-instantiate nix-build nix-eval-jobs nix-shell nix] {
    ^ln -sf ($bin | path expand) $"($dir)/bin/($n)"
  }
  $"($dir)/bin"
}

def --wrapped run-both [farm: string, name: string, --cwd: string = ".", ...args: string] {
  let ours = (^$"($farm)/($name)" ...$args | complete)
  let real = (^$name ...$args | complete)
  {ours: $ours, real: $real}
}

# Compares stdout bytes and exit code; prints ok/FAIL and returns the verdict.
def cmp-case [label: string, r: record]: nothing -> bool {
  let same_out = ($r.ours.stdout | into binary) == ($r.real.stdout | into binary)
  let same_code = $r.ours.exit_code == $r.real.exit_code
  if $same_out and $same_code {
    verdict true $label ""
  } else {
    let d = $"($label): exit ours=($r.ours.exit_code) real=($r.real.exit_code)"
    print $"FAIL ($d)"
    print $"  ours stdout: ($r.ours.stdout | str substring 0..400)"
    print $"  real stdout: ($r.real.stdout | str substring 0..400)"
    print $"  ours stderr: ($r.ours.stderr | str substring 0..400)"
    false
  }
}

# `env` output is only comparable without the variables that legitimately
# differ: the per-run temporary directory, bash's own bookkeeping, the prompt.
const ENV_SKIP = '^(NIX_BUILD_TOP|TMPDIR|TMP|TEMP|TEMPDIR|NIX_ATTRS_SH_FILE|NIX_ATTRS_JSON_FILE|__[A-Za-z_]*|SHLVL|_|PWD|OLDPWD|PS1)='

def env-filter [text: string]: nothing -> string {
  $text | lines | where not ($it =~ $ENV_SKIP) | sort | str join "\n"
}

# Compares a filtered `env` dump and the exit code.
def cmp-env [label: string, r: record]: nothing -> bool {
  let a = (env-filter $r.real.stdout)
  let b = (env-filter $r.ours.stdout)
  if $a == $b and $r.ours.exit_code == $r.real.exit_code {
    verdict true $"($label) \(($a | lines | length) variables\)" ""
  } else {
    print $"FAIL ($label): exit ours=($r.ours.exit_code) real=($r.real.exit_code)"
    print $"  only real: ($a | lines | where $it not-in ($b | lines) | first 4 | str join ' | ')"
    print $"  only ours: ($b | lines | where $it not-in ($a | lines) | first 4 | str join ' | ')"
    print $"  ours stderr: ($r.ours.stderr | str substring 0..300)"
    false
  }
}

# The reason a case falls back, without paying for the real run. `cmd` is the
# classic name and its arguments.
def fallback-reason [farm: string, cmd: list<string>]: nothing -> string {
  let name = ($cmd | first)
  let args = ($cmd | skip 1)
  (with-env {IETSP_NO_FALLBACK: "1"} { ^$"($farm)/($name)" ...$args | complete }).stderr
}

def main [] {
  let bin = ($env | get -o BIN | default "target/release")
  let ietsp = $"($bin)/ietsp"
  if not ($ietsp | path exists) { print -e $"no ($ietsp); cargo build --release first"; exit 1 }
  let real = (real-bin-dir)
  print $"real nix: ($real)"
  $env.IETSP_NIX_BIN_DIR = $real
  let tmp = (mktemp -d -t classic-e2e-XXXXXX)
  let farm = (make-farm $ietsp $tmp)
  mut ok = []

  # --- nix-instantiate: drv paths
  $ok = ($ok | append (cmp-case "instantiate <nixpkgs> -A hello"
    (run-both $farm nix-instantiate -I $"nixpkgs=($NIXPKGS)" "<nixpkgs>" -A hello)))
  $ok = ($ok | append (cmp-case "instantiate <nixpkgs> -A glibc (multi-output)"
    (run-both $farm nix-instantiate -I $"nixpkgs=($NIXPKGS)" "<nixpkgs>" -A glibc)))
  $ok = ($ok | append (cmp-case "instantiate <nixpkgs> -A glibc.dev (!output suffix)"
    (run-both $farm nix-instantiate -I $"nixpkgs=($NIXPKGS)" "<nixpkgs>" -A glibc.dev)))
  $ok = ($ok | append (cmp-case "instantiate two -A in one run"
    (run-both $farm nix-instantiate -I $"nixpkgs=($NIXPKGS)" "<nixpkgs>" -A hello -A glibc.dev)))

  # --- nix-instantiate: --eval
  $ok = ($ok | append (cmp-case "instantiate --eval --strict --json"
    (run-both $farm nix-instantiate --eval --strict --json -E "{ a = 1; }")))
  $ok = ($ok | append (cmp-case "instantiate --eval"
    (run-both $farm nix-instantiate --eval -E "{ a = 1; b = [ 1 2 ]; }")))
  $ok = ($ok | append (cmp-case "instantiate --eval --raw"
    (run-both $farm nix-instantiate --eval --raw -E "\"x\" + \"y\"")))
  $ok = ($ok | append (cmp-case "instantiate --eval --argstr"
    (run-both $farm nix-instantiate --eval --strict --json --argstr who world -E "{ who }: { inherit who; }")))

  # --- a tiny custom derivation, like scripts/daemon-e2e.nu case 1
  let d = $"($tmp)/case"
  mkdir $"($d)/src"
  $"hello ($nu.pid)\n" | save $"($d)/src/a"
  r#'derivation {
  name = "classic-e2e";
  system = "x86_64-linux";
  builder = "/bin/sh";
  args = [ "-c" "cat ${@DIR@/src}/a > $out" ];
}
'# | str replace -a "@DIR@" $d | save $"($d)/default.nix"
  r#'derivation {
  name = "classic-e2e-multi";
  system = "x86_64-linux";
  outputs = [ "out" "dev" ];
  builder = "/bin/sh";
  args = [ "-c" "echo a > $out; echo b > $dev" ];
}
'# | save $"($d)/multi.nix"

  $ok = ($ok | append (cmp-case "instantiate a custom drv"
    (run-both $farm nix-instantiate $"($d)/default.nix")))
  $ok = ($ok | append (cmp-case "instantiate --readonly-mode"
    (run-both $farm nix-instantiate --readonly-mode $"($d)/default.nix")))

  # --- --add-root: the printed path is the link, and the daemon knows the root
  mut root_ok = true
  for who in [real ours] {
    let cmd = if $who == "real" { "nix-instantiate" } else { $"($farm)/nix-instantiate" }
    let link = $"($tmp)/gcroot-($who)"
    let out = (^$cmd --add-root $link $"($d)/default.nix" | complete)
    let printed = ($out.stdout | chomp)
    if $printed != $link { print $"FAIL --add-root ($who): printed '($printed)' != '($link)'"; $root_ok = false }
    let target = (^readlink $link | chomp)
    let roots = (^nix-store --query --roots $target | complete | get stdout)
    if not ($roots | str contains $link) { print $"FAIL --add-root ($who): ($link) is not a gc root"; $root_ok = false }
  }
  $ok = ($ok | append (verdict $root_ok "--add-root links and registers a gc root" "see above"))

  # --- nix-build: default `result`, -o, multi-output naming
  for case in [
    {label: "build a custom drv, default result", args: [$"($d)/default.nix"], links: [result]}
    {label: "build -o link", args: [-o link $"($d)/default.nix"], links: [link]}
    {label: "build --no-out-link", args: [--no-out-link $"($d)/default.nix"], links: []}
    {label: "build two outputs of one drv", args: [-E $"let m = import ($d)/multi.nix; in [ m.dev m.out ]"], links: [result result-dev]}
    {label: "build two drvs", args: [-E ("[ (import " + $d + "/default.nix) (import " + $d + "/multi.nix) ]")], links: [result result-2]}
  ] {
    mut got = {}
    for who in [real ours] {
      let wd = $"($tmp)/build-($who)"
      mkdir $wd
      cd $wd
      let cmd = if $who == "real" { "nix-build" } else { $"($farm)/nix-build" }
      let r = (^$cmd ...$case.args | complete)
      let links = ($case.links | each {|l| if ($l | path exists) { ^readlink $l | chomp } else { $"MISSING ($l)" } })
      $got = ($got | insert $who {out: ($r.stdout), code: $r.exit_code, links: $links, err: $r.stderr})
    }
    let same = (($got.ours.out | into binary) == ($got.real.out | into binary)) and ($got.ours.code == $got.real.code) and ($got.ours.links == $got.real.links)
    if $same {
      print $"ok   ($case.label)"
    } else {
      print $"FAIL ($case.label)"
      print $"  ours: code=($got.ours.code) links=($got.ours.links)"
      print $"  real: code=($got.real.code) links=($got.real.links)"
      print $"  ours stdout: ($got.ours.out)"
      print $"  real stdout: ($got.real.out)"
      print $"  ours stderr: ($got.ours.err | str substring 0..400)"
    }
    $ok = ($ok | append $same)
  }

  # --- fallbacks
  $ok = ($ok | append (cmp-case "--parse execs the real nix-instantiate"
    (run-both $farm nix-instantiate --parse -E 1)))
  let fb = (^$"($farm)/nix-instantiate" --parse -E 1 | complete)
  $ok = ($ok | append (verdict ($fb.stderr | str contains "ietsp: falling back to nix-instantiate:")
    "the fallback prints one line on stderr" $"stderr was '($fb.stderr)'"))

  let nofb = (with-env {IETSP_NO_FALLBACK: "1"} { ^$"($farm)/nix-instantiate" --parse -E 1 | complete })
  $ok = ($ok | append (verdict ($nofb.exit_code == 1) "IETSP_NO_FALLBACK=1 exits 1" $"exit ($nofb.exit_code)"))

  mkdir $"($tmp)/tb"
  "hi\n" | save $"($tmp)/tb/f"
  ^tar -czf $"($tmp)/x.tar.gz" -C $tmp tb
  $ok = ($ok | append (cmp-case "fetchTarball falls back and prints the real result"
    (run-both $farm nix-instantiate --eval -E $"builtins.fetchTarball \"file://($tmp)/x.tar.gz\"")))

  let repl = (^$"($farm)/nix" repl --version | complete)
  $ok = ($ok | append (verdict ($repl.stderr | str contains "ietsp: falling back to nix:")
    "the 'nix' name falls back" $"stderr was '($repl.stderr)'"))

  # --- the `nix` name: a locked flake with no inputs, copied out of the
  # repository so both sides see a `path:` flake instead of a git one.
  let fdir = $"($tmp)/flake"
  mkdir $fdir
  cp $"($env.FILE_PWD)/../tests/fixtures/flake/flake.nix" $fdir
  cp $"($env.FILE_PWD)/../tests/fixtures/flake/flake.lock" $fdir
  let fref = $"path:($fdir)"

  $ok = ($ok | append (cmp-case "nix eval --json .#x"
    (run-both $farm nix eval --json $"($fref)#x")))
  $ok = ($ok | append (cmp-case "nix eval --raw .#x"
    (run-both $farm nix eval --raw $"($fref)#x")))
  $ok = ($ok | append (cmp-case "nix eval .#s (attrset, list, float)"
    (run-both $farm nix eval $"($fref)#s")))
  $ok = ($ok | append (cmp-case "nix eval .#l"
    (run-both $farm nix eval $"($fref)#l")))

  # the farm first on PATH: copying a fresh tree must spawn the real nix, not us
  let fdir2 = $"($tmp)/flake-fresh"
  mkdir $fdir2
  cp $"($fdir)/flake.nix" $fdir2
  cp $"($fdir)/flake.lock" $fdir2
  $"# ($tmp)\n" | save --append $"($fdir2)/flake.nix"
  let ours = (with-env { PATH: $"($farm):($env.PATH)" } { ^$"($farm)/nix" eval --json $"path:($fdir2)#x" | complete })
  let real = (^nix eval --json $"path:($fdir2)#x" | complete)
  $ok = ($ok | append (cmp-case "nix eval, farm first on PATH, fresh tree" {ours: $ours, real: $real}))
  $ok = ($ok | append (cmp-case "nix eval .#pkg (a derivation)"
    (run-both $farm nix eval $"($fref)#pkg")))
  $ok = ($ok | append (cmp-case "nix eval . (packages.<system>.default)"
    (run-both $farm nix eval $fref)))
  $ok = ($ok | append (cmp-case "nix eval --expr"
    (run-both $farm nix eval --expr "{ a = 1; b = [ 1 2 ]; c = \"x\"; d = 123456789.5; }")))
  $ok = ($ok | append (cmp-case "nix eval --expr, a nested error"
    (run-both $farm nix eval --expr "{ a = throw \"boom\"; b = 1; }")))
  $ok = ($ok | append (cmp-case "nix eval --expr, repeated and empty values"
    (run-both $farm nix eval --expr "let a = { x = 1; }; in [ a a {} {} [] ]")))
  $ok = ($ok | append (cmp-case "nix eval --expr, functions"
    (run-both $farm nix eval --expr "{ f = x: x; p = builtins.add; q = builtins.add 1; }")))
  $ok = ($ok | append (cmp-case "nix eval -f FILE ATTR"
    (run-both $farm nix eval -f $"($d)/multi.nix" outputs)))
  $ok = ($ok | append (cmp-case "nix eval --json -f FILE ATTR"
    (run-both $farm nix eval --json -f $"($d)/multi.nix" outputs)))

  # --- nix build: the out-link prefix, the printed paths and the link target
  mut got = {}
  for who in [real ours] {
    let wd = $"($tmp)/nixbuild-($who)"
    mkdir $wd
    cd $wd
    let cmd = if $who == "real" { "nix" } else { $"($farm)/nix" }
    let r = (^$cmd build $"($fref)#pkg" --print-out-paths -o result-x | complete)
    let link = (if ("result-x" | path exists) { ^readlink result-x | chomp } else { "MISSING" })
    $got = ($got | insert $who {out: $r.stdout, code: $r.exit_code, link: $link, err: $r.stderr})
  }
  let same = (($got.ours.out | into binary) == ($got.real.out | into binary)) and ($got.ours.code == $got.real.code) and ($got.ours.link == $got.real.link)
  if not $same {
    print $"  ours: code=($got.ours.code) link=($got.ours.link) out=($got.ours.out)"
    print $"  real: code=($got.real.code) link=($got.real.link) out=($got.real.out)"
    print $"  ours stderr: ($got.ours.err | str substring 0..400)"
  }
  $ok = ($ok | append (verdict $same "nix build .#pkg --print-out-paths -o result-x" "see above"))

  # `nix print-dev-env` is `ietsp print-dev-env`, so only the dispatch is new.
  let pde_nix = (^$"($farm)/nix" print-dev-env $"($fref)#pkg" | complete)
  let pde_own = (^(^readlink -f $"($farm)/nix" | chomp) print-dev-env $"($fref)#pkg" | complete)
  $ok = ($ok | append (verdict (($pde_nix.stdout == $pde_own.stdout) and ($pde_nix.exit_code == $pde_own.exit_code))
    "nix print-dev-env is ietsp print-dev-env" $"exit ($pde_nix.exit_code) vs ($pde_own.exit_code)"))

  # --- nix-shell: the derivation's inputs, then bash on nix-build.cc's rc file.
  # `<nixpkgs>` resolves to nixpkgs/shell.nix, which fetches a pinned tree, so
  # the real code path needs default.nix by name.
  cd $tmp
  let pkgs_nix = $"($NIXPKGS)/default.nix"
  $ok = ($ok | append (cmp-case "nix-shell nixpkgs -A hello --run 'echo $name'"
    (run-both $farm nix-shell -I $"nixpkgs=($NIXPKGS)" $pkgs_nix -A hello --run 'echo $name')))
  $ok = ($ok | append (cmp-env "nix-shell nixpkgs -A hello --run env"
    (run-both $farm nix-shell -I $"nixpkgs=($NIXPKGS)" $pkgs_nix -A hello --run env)))
  $ok = ($ok | append (cmp-env "nix-shell --pure --run env"
    (run-both $farm nix-shell -I $"nixpkgs=($NIXPKGS)" $pkgs_nix -A hello --pure --run env)))
  $ok = ($ok | append (cmp-case "nix-shell --pure --keep FOO"
    (with-env {FOO: "bar"} { run-both $farm nix-shell -I $"nixpkgs=($NIXPKGS)" $pkgs_nix -A hello --pure --keep FOO --run 'echo "[$FOO]"' })))
  # --command is interactive unless stdin is not a tty: an empty pipe settles it.
  let cmd_ours = ("" | ^$"($farm)/nix-shell" -I $"nixpkgs=($NIXPKGS)" $pkgs_nix -A hello --command 'echo $name' | complete)
  let cmd_real = ("" | ^nix-shell -I $"nixpkgs=($NIXPKGS)" $pkgs_nix -A hello --command 'echo $name' | complete)
  $ok = ($ok | append (cmp-case "nix-shell --command, stdin not a tty" {ours: $cmd_ours, real: $cmd_real}))

  # a plain derivation: shell.nix wins over default.nix, and passAsFile
  let sh = $"($tmp)/shellnix"
  mkdir $sh
  r#'derivation {
  name = "classic-e2e-shell";
  system = "x86_64-linux";
  builder = "/bin/sh";
  args = [ "-c" "echo hi > $out" ];
  MARK = "from-@WHICH@";
  passAsFile = "BIG";
  BIG = "big-value";
}
'# | str replace -a "@WHICH@" "shell-nix" | save $"($sh)/shell.nix"
  open --raw $"($sh)/shell.nix" | str replace -a "shell-nix" "default-nix" | save $"($sh)/default.nix"
  cd $sh
  $ok = ($ok | append (cmp-case "nix-shell with no argument prefers shell.nix"
    (run-both $farm nix-shell --run 'echo $MARK; cat $BIGPath')))
  $ok = ($ok | append (cmp-case "nix-shell --run 'exit 3' keeps the exit code"
    (run-both $farm nix-shell --run "exit 3")))
  cd $tmp
  $ok = ($ok | append (cmp-case "nix-shell with no shell.nix or default.nix"
    (run-both $farm nix-shell --run "true")))

  # --- nix develop: the print-dev-env environment, then bash. This repository's
  # own flake is the only mkShell-shaped shell around (the fixture has no nixpkgs).
  let repo = ($env.FILE_PWD | path join ".." | path expand)
  $ok = ($ok | append (cmp-env "nix develop -c env"
    (run-both $farm nix develop $repo -c env)))
  $ok = ($ok | append (cmp-case "nix develop -c sh -c 'echo $name'"
    (run-both $farm nix develop $repo -c sh -c 'echo $name')))
  $ok = ($ok | append (cmp-env "nix develop --ignore-environment --keep HOME"
    (run-both $farm nix develop $repo -i -k HOME -c env)))
  $ok = ($ok | append (cmp-case "nix develop -u HOME"
    (run-both $farm nix develop $repo -u HOME -c sh -c 'echo "[$HOME]"')))

  # --- fallbacks, priced with IETSP_NO_FALLBACK so the real run never happens
  for case in [
    {label: "nix-shell -p falls back", want: "'-p' is not implemented"
     args: [nix-shell -p hello --run "true"]}
    {label: "nix-shell <nixpkgs> falls back on the fetch in nixpkgs/shell.nix", want: "builtins.fetchTarball"
     args: [nix-shell -I $"nixpkgs=($NIXPKGS)" "<nixpkgs>" -A hello --run "true"]}
    {label: "nix develop --phase falls back", want: "'--phase' is not implemented"
     args: [nix develop --phase build]}
    {label: "nix develop --profile falls back", want: "'--profile' is not implemented"
     args: [nix develop --profile p]}
    {label: "nix develop --redirect falls back", want: "'--redirect' is not implemented"
     args: [nix develop --redirect a b]}
  ] {
    let err = (fallback-reason $farm $case.args)
    $ok = ($ok | append (verdict ($err | str contains $case.want) $case.label $"stderr was '($err)'"))
  }
  let shebang = $"($tmp)/script.sh"
  "#! /usr/bin/env nix-shell\n#! nix-shell -p hello -i bash\necho hi\n" | save -f $shebang
  let err = (fallback-reason $farm [nix-shell $shebang])
  $ok = ($ok | append (verdict ($err | str contains "a '#!' script") "nix-shell falls back on a #! script" $"stderr was '($err)'"))

  let meta = (^$"($farm)/nix" flake metadata $fref | complete)
  $ok = ($ok | append (verdict ($meta.stderr | str contains "falling back to nix: subcommand 'flake'")
    "nix flake metadata falls back" $"stderr was '($meta.stderr)'"))
  let apply = (^$"($farm)/nix" eval --apply "x: x" --expr 1 | complete)
  $ok = ($ok | append (verdict ($apply.stderr | str contains "'--apply' is not implemented")
    "nix eval --apply falls back" $"stderr was '($apply.stderr)'"))

  cd /
  rm -rf $tmp
  let failed = ($ok | where $it == false | length)
  print $"($ok | length) cases, ($failed) failed"
  if $failed > 0 { exit 1 }
}

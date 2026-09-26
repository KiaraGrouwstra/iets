#!/usr/bin/env nu
# Classic mode against the real nix: for every case, run the real
# nix-instantiate/nix-build and ours and compare stdout byte for byte and the
# exit code. Needs a live nix-daemon and the real `nix` on PATH, and it writes
# to the store: `nix flake check` runs it in a container (nix/e2e-test.nix),
# and by hand it runs in the dev shell, which has nix 2.34.8.
#
# usage: nu scripts/classic-e2e.nu
#        BIN=target/debug nu scripts/classic-e2e.nu
use lib.nu [chomp current-system isolate ietsp-bin make-farm nixpkgs real-bin-dir require-real-nix scratch-daemon show verdict]

def --wrapped run-both [farm: string, name: string, --cwd: string = ".", ...args: string] {
  let ours = (^$"($farm)/($name)" ...$args | complete)
  let real = (^$name ...$args | complete)
  {ours: $ours, real: $real}
}

# `cmd` with `DIR/\xff.nix` as its last argument, through bash: a nushell
# string cannot hold the byte.
def --wrapped with-ff [dir: string, ...cmd: string] {
  ^bash -c r#'d=$1; shift; exec "$@" "$d"/$'\xff'.nix'# _ $dir ...$cmd | complete
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
    print $"  real stderr: ($r.real.stderr | str substring 0..400)"
    false
  }
}

# The first level-0 `--log-format internal-json` error record on stderr, with
# the fields both sides fill alike: Nix colours raw_msg, ietsp does not.
def err-record [stderr: string]: nothing -> record {
  let recs = ($stderr | lines | where ($it | str starts-with "@nix ") | each { str substring 5.. | from json })
  let r = ($recs | where {|r| $r.level? == 0 and ($r.raw_msg? != null) } | get -o 0 | default {})
  {raw_msg: ($r.raw_msg? | default "" | ansi strip), line: $r.line?, column: $r.column?, file: $r.file?, code: $r.code?}
}

# Compares the error records and the exit codes; ours must carry CODE.
def cmp-record [label: string, code: string, r: record]: nothing -> bool {
  let a = (err-record $r.real.stderr | reject code)
  let b = (err-record $r.ours.stderr)
  if $a == ($b | reject code) and $b.code == $code and $r.ours.exit_code == $r.real.exit_code and $a.raw_msg != "" {
    verdict true $label ""
  } else {
    print $"FAIL ($label): exit ours=($r.ours.exit_code) real=($r.real.exit_code)"
    print $"  real: ($a | to json -r)"
    print $"  ours: ($b | to json -r)"
    false
  }
}

# The structure of a build's activity records: the starts and stops of the
# daemon worker's realise, builds and copy-paths activities and of each
# build, in order, and which result types each activity type got. Ids,
# timings, texts, the substituter queries and ietsp's own upload ("copying N
# paths", which has a text) are left out.
def act-shape [stderr: string]: nothing -> record {
  let recs = ($stderr | lines | where ($it | str starts-with "@nix ") | each { str substring 5.. | from json })
  let kept = ($recs | where {|r| $r.action == "start" and $r.type in [102 103 104 105] and ($r.type == 105 or $r.text == "") })
  let ty = ($kept | reduce -f {} {|a, m| $m | insert ($a.id | into string) $a.type })
  let ids = ($ty | columns)
  let mine = ($recs | where {|r| ($r.id? | default "" | into string) in $ids })
  {
    order: ($mine | where action in [start stop] | each {|r| $"($r.action) ($ty | get ($r.id | into string))" })
    results: ($mine | where action == result | each {|r| $"($ty | get ($r.id | into string))/($r.type)" } | uniq | sort)
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
  require-real-nix nix nix-instantiate nix-build nix-shell
  let ietsp = (ietsp-bin)
  let tmp = (isolate classic-e2e)
  let np = (nixpkgs)
  let real = (real-bin-dir)
  print $"real nix: ($real)"
  $env.IETSP_NIX_BIN_DIR = $real
  let farm = (make-farm $ietsp $tmp)
  mut ok = []

  # --- nix-instantiate: drv paths
  $ok = ($ok | append (cmp-case "instantiate <nixpkgs> -A hello"
    (run-both $farm nix-instantiate -I $"nixpkgs=($np)" "<nixpkgs>" -A hello)))
  $ok = ($ok | append (cmp-case "instantiate <nixpkgs> -A glibc (multi-output)"
    (run-both $farm nix-instantiate -I $"nixpkgs=($np)" "<nixpkgs>" -A glibc)))
  $ok = ($ok | append (cmp-case "instantiate <nixpkgs> -A glibc.dev (!output suffix)"
    (run-both $farm nix-instantiate -I $"nixpkgs=($np)" "<nixpkgs>" -A glibc.dev)))
  $ok = ($ok | append (cmp-case "instantiate two -A in one run"
    (run-both $farm nix-instantiate -I $"nixpkgs=($np)" "<nixpkgs>" -A hello -A glibc.dev)))

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

  # --- the `builder` setting: the build runs on a scratch chroot daemon, and
  # the outputs come back so `result` names a path the local store has. The
  # builder uses shell builtins only: a chroot store has no coreutils.
  r#'derivation {
  name = "classic-e2e-builder";
  system = "x86_64-linux";
  builder = "/bin/sh";
  args = [ "-c" "while read -r l; do echo \"$l\"; done < ${@DIR@/src}/a > $out" ];
}
'# | str replace -a "@DIR@" $d | save $"($d)/builder.nix"
  let sd = $"($tmp)/scratch"
  mkdir $sd
  let sock = (scratch-daemon $sd)
  let wd = $"($tmp)/build-builder"
  mkdir $wd
  cd $wd
  let r = (with-env {IETSP_CONFIG: $"builder = unix://($sock)"} {
    ^$"($farm)/nix-build" $"($d)/builder.nix" | complete
  })
  let printed = ($r.stdout | chomp)
  let link = (if ("result" | path exists) { ^readlink result | chomp } else { "MISSING" })
  let built_there = ($"($sd)($printed)" | path exists)
  let here = ((^nix-store --check-validity $printed | complete).exit_code == 0)
  let rooted = ((^nix-store --query --roots $printed | complete).stdout | str contains $"($wd)/result")
  let builder_ok = ($r.exit_code == 0) and ($link == $printed) and $built_there and $here and $rooted
  if not $builder_ok {
    print $"  exit=($r.exit_code) printed=($printed) link=($link)"
    print $"  on the scratch daemon=($built_there) valid here=($here) gc root=($rooted)"
    print $"  stderr: ($r.stderr | str substring 0..600)"
  }
  $ok = ($ok | append (verdict $builder_ok "builder = unix://SCRATCH builds there and registers here" "see above"))
  cd $tmp

  # --- fallbacks
  $ok = ($ok | append (cmp-case "--parse execs the real nix-instantiate"
    (run-both $farm nix-instantiate --parse -E 1)))
  # `--ietsp-option` is ours: the fallback must exec nix without it.
  $ok = ($ok | append (cmp-case "--ietsp-option never reaches the fallback nix" {
    ours: (^$"($farm)/nix-instantiate" --ietsp-option workers 2 --parse -E 1 | complete),
    real: (^nix-instantiate --parse -E 1 | complete)
  }))
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

  let ifd = $"import \(derivation { name = \"x\"; system = builtins.currentSystem; builder = \"/bin/sh\"; args = [\"-c\" \"echo 42 > $out\"]; })"
  $ok = ($ok | append (cmp-case "import from derivation falls back"
    (run-both $farm nix-instantiate --eval -E $ifd)))
  let r = (^$ietsp eval -E $ifd | complete)
  $ok = ($ok | append (verdict ($r.exit_code == 1 and ($r.stderr | str contains "import from derivation is not supported"))
    "ietsp eval refuses import from derivation" $"exit ($r.exit_code), stderr '($r.stderr)'"))

  # --- a file name that is not UTF-8
  let ff = $"($tmp)/ff"
  mkdir $ff
  ^bash -c r#'echo "{ a = 1; }" > "$1"/$'\xff'.nix'# _ $ff
  let real_ff = (with-ff $ff nix-instantiate --eval)
  $ok = ($ok | append (cmp-case "instantiate --eval a non-UTF-8 file name"
    {ours: (with-ff $ff $"($farm)/nix-instantiate" --eval), real: $real_ff}))
  $ok = ($ok | append (cmp-case "ietsp eval a non-UTF-8 file name"
    {ours: (with-ff $ff $ietsp eval), real: $real_ff}))
  $ok = ($ok | append (cmp-case "a non-UTF-8 file name reaches the fallback intact"
    {ours: (with-ff $ff $"($farm)/nix-instantiate" --parse), real: (with-ff $ff nix-instantiate --parse)}))

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

  let pf = (run-both $farm nix flake prefetch-inputs $fref)
  $ok = ($ok | append ((cmp-case "nix flake prefetch-inputs, no inputs" $pf) and (verdict (not ($pf.ours.stderr | str contains "falling back"))
    "nix flake prefetch-inputs is ietsp's own" $"stderr '($pf.ours.stderr)'")))

  # the farm first on PATH: copying a fresh tree must spawn the real nix, not us
  let fdir2 = $"($tmp)/flake-fresh"
  mkdir $fdir2
  cp $"($fdir)/flake.nix" $fdir2
  cp $"($fdir)/flake.lock" $fdir2
  $"# ($tmp)\n" | save --append $"($fdir2)/flake.nix"
  let ours = (with-env { PATH: $"($farm):($env.PATH)" } { ^$"($farm)/nix" eval --json $"path:($fdir2)#x" | complete })
  let real = (^nix eval --json $"path:($fdir2)#x" | complete)
  $ok = ($ok | append (cmp-case "nix eval, farm first on PATH, fresh tree" {ours: $ours, real: $real}))

  # the farm shadows the real nix out of PATH and no IETSP_NIX_BIN_DIR is set:
  # the fallback must find nix through the farm's own marker
  let shadow = $"($tmp)/shadow"
  mkdir $shadow
  ^ln -sf (which bash | get 0.path) $"($shadow)/bash"
  let ours = (with-env { PATH: $"($farm):($shadow)", IETSP_NIX_BIN_DIR: null } { ^$"($farm)/nix-instantiate" --parse -E "1 + 1" | complete })
  let real = (^nix-instantiate --parse -E "1 + 1" | complete)
  $ok = ($ok | append (cmp-case "fallback through the farm marker, real nix off PATH" {ours: $ours, real: $real}))
  $ok = ($ok | append (cmp-case "nix eval .#pkg (a derivation)"
    (run-both $farm nix eval $"($fref)#pkg")))
  $ok = ($ok | append (cmp-case "nix eval . (packages.<system>.default)"
    (run-both $farm nix eval $fref)))
  $ok = ($ok | append (cmp-case "nix eval --expr"
    (run-both $farm nix eval --expr "{ a = 1; b = [ 1 2 ]; c = \"x\"; d = 123456789.5; }")))
  # --- pure-eval (docs/reference/settings.md): `nix` is pure unless told otherwise,
  # `--file` is impure, and the legacy names are impure.
  let env_expr = 'builtins.getEnv "HOME"'
  $env_expr | save -f $"($tmp)/env.nix"
  let pure_probe = '[ (builtins ? currentSystem) (builtins.getEnv "HOME") builtins.nixPath ]'
  for case in [
    [label args];
    ["nix eval --expr is pure" [nix eval --expr $env_expr]]
    ["nix eval --impure --expr" [nix eval --impure --expr $env_expr]]
    ["nix --option pure-eval false eval --expr" [nix --option pure-eval false eval --expr $env_expr]]
    ["nix eval -f is impure" [nix eval -f $"($tmp)/env.nix"]]
    ["nix eval .#x is pure" [nix eval $"($fref)#x"]]
    ["nix-instantiate is impure" [nix-instantiate --eval -E $env_expr]]
    ["nix-instantiate --pure-eval" [nix-instantiate --pure-eval --eval --strict -E $pure_probe]]
    ["nix-instantiate --option pure-eval true" [nix-instantiate --option pure-eval true --eval --strict -E $pure_probe]]
    ["nix-instantiate --pure-eval refuses a local file" [nix-instantiate --pure-eval --eval -E "builtins.readFile /etc/hostname"]]
  ] {
    $ok = ($ok | append (cmp-case $case.label (run-both $farm ($case.args | first) ...($case.args | skip 1))))
  }
  $ok = ($ok | append (cmp-case "nix.conf system reaches builtins.currentSystem"
    (with-env {NIX_CONFIG: "system = aarch64-linux"} { run-both $farm nix-instantiate --eval -E "builtins.currentSystem" })))
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

  # --- a file whose top level is a function: the attribute path auto-calls it
  # at every step, in the native names too (docs/reference/plumbing.md)
  let fun = $"($d)/fun.nix"
  '{ a ? 1, b ? 2 }: { sum = a + b; name = "n-${toString a}"; }' | save -f $fun
  let ell = $"($d)/ellipsis.nix"
  '{ a ? 1, ... }@args: { sum = a; name = builtins.toJSON (builtins.attrNames args); }' | save -f $ell
  for case in [
    {label: "defaults", args: [], attr: sum}
    {label: "--arg", args: [--arg a 5], attr: sum}
    {label: "--argstr", args: [--argstr a x], attr: name}
  ] {
    $ok = ($ok | append (cmp-case $"auto-call a function file, ($case.label): nix-instantiate"
      (run-both $farm nix-instantiate --eval ...$case.args $fun -A $case.attr)))
    $ok = ($ok | append (cmp-case $"auto-call a function file, ($case.label): ietsp eval"
      {ours: (^$ietsp eval ...$case.args $fun -A $case.attr | complete),
       real: (^nix eval ...$case.args -f $fun $case.attr | complete)}))
    # formals with `...` are handed every argument, named or not
    $ok = ($ok | append (cmp-case $"auto-call a '...' function file, ($case.label): nix-instantiate"
      (run-both $farm nix-instantiate --eval ...$case.args --arg extra 9 $ell -A $case.attr)))
    $ok = ($ok | append (cmp-case $"auto-call a '...' function file, ($case.label): ietsp eval"
      {ours: (^$ietsp eval ...$case.args --arg extra 9 $ell -A $case.attr | complete),
       real: (^nix eval ...$case.args --arg extra 9 -f $ell $case.attr | complete)}))
  }
  # a required formal nobody supplied: all three fail the same way
  let na = $"($d)/needs-arg.nix"
  '{ a, b ? 2 }: { sum = a + b; }' | save -f $na
  let first = {|s: string| $s | ansi strip | lines | where ($it | str starts-with "error:") | get -o 0 | default "" }
  let na_real = (^nix eval -f $na sum | complete)
  let na_eval = (^$ietsp eval $na -A sum | complete)
  let na_inst = (^$"($farm)/nix-instantiate" --eval $na -A sum | complete)
  let na_lines = [(do $first $na_real.stderr) (do $first $na_eval.stderr) (do $first $na_inst.stderr)]
  $ok = ($ok | append (verdict
    (([$na_real.exit_code $na_eval.exit_code $na_inst.exit_code] | all { $in == 1 })
      and ($na_lines | uniq | length) == 1 and ($na_lines | first) != "")
    "a required --arg that is missing: the same first error line everywhere"
    $"exit real=($na_real.exit_code) eval=($na_eval.exit_code) inst=($na_inst.exit_code), lines ($na_lines | to json -r)"))

  # --- nix search: a package set with nested recurseForDerivations sets, an
  # attribute that throws, a bad `type` and a description with a newline.
  r#'let
  mk = { pname, version ? "", description ? null }:
    derivation {
      name = if version == "" then pname else "${pname}-${version}";
      system = builtins.currentSystem;
      builder = "/bin/sh";
      args = [ "-c" "echo hi > $out" ];
    } // (if description == null then { } else { meta = { inherit description; }; });
in
{
  legacyPackages.${builtins.currentSystem} = {
    hello = mk { pname = "hello"; version = "2.12"; description = "A program that produces a familiar, friendly greeting"; };
    goodbye = mk { pname = "goodbye"; version = "1.0"; description = "Says goodbye\nacross two lines"; };
    plain = mk { pname = "plain"; };
    broken = throw "this attribute is broken";
    brokenSet = { recurseForDerivations = true; inner = throw "deep throw"; ok = mk { pname = "survivor"; version = "1"; description = "Found after a throw"; }; };
    notASet = 42;
    badType = { type = "derivation"; name = 7; };
    nested = {
      recurseForDerivations = true;
      tool = mk { pname = "nested-tool"; version = "0.1"; description = "A nested tool"; };
      deeper = { recurseForDerivations = true; deepest = mk { pname = "deepest"; version = "3"; description = "Deep package"; }; };
      unmarked = { hidden = mk { pname = "hidden"; version = "9"; description = "Not reachable"; }; };
    };
    metaNotASet = mk { pname = "metaless"; version = "2"; } // { meta = 5; };
    quotes = mk { pname = "quotes"; version = "1"; description = "Uses \"quotes\", a tab\tand a backslash \\"; };
    unicode = mk { pname = "unicode"; version = "1"; description = "Ünïcödé — em dash and ▲"; };
  };
  packages.${builtins.currentSystem}.direct = mk { pname = "direct"; version = "4"; description = "Under packages"; };
}
'# | save -f $"($d)/pkgs.nix"
  for case in [
    {label: "nix search, every package", args: ["^"]}
    {label: "nix search --json", args: ["^" --json]}
    {label: "nix search --json --pretty", args: ["^" --json --pretty]}
    {label: "nix search, two regexes are ANDed", args: [deep est]}
    {label: "nix search --exclude", args: [e -e good]}
    {label: "nix search is case insensitive", args: [HELLO]}
    {label: "nix search, a bracket expression", args: ['[Ff]riendly']}
    {label: "nix search, an alternation", args: ['hell|good']}
    {label: "nix search, no results", args: [zzz]}
    {label: "nix search --json, no results", args: [zzz --json]}
  ] {
    $ok = ($ok | append (cmp-case $case.label
      (run-both $farm nix search -f $"($d)/pkgs.nix" "" ...$case.args)))
  }
  # over the pinned nixpkgs: the walk that costs, under `legacyPackages` so
  # that both sides skip the attributes that throw.
  let npexpr = $"{ legacyPackages.${builtins.currentSystem} = import ($np) {}; }"
  for re in ["^hello$" ripgrep] {
    $ok = ($ok | append (cmp-case $"nix search nixpkgs --json ($re)"
      (run-both $farm nix search --impure --expr $npexpr "" $re --json)))
  }

  # --- --log-format internal-json: the same error record and exit code
  "let\n  x = 1;\nin\n  y + x\n" | save -f $"($d)/undef.nix"
  let json = [--log-format internal-json]
  $ok = ($ok | append (cmp-record "internal-json: undefined variable in a file" undefined-var
    (run-both $farm nix-instantiate ...$json --eval $"($d)/undef.nix")))
  $ok = ($ok | append (cmp-record "internal-json: missing attribute" missing-attr
    (run-both $farm nix-instantiate ...$json --eval -E "{ a = 1; }.b")))
  $ok = ($ok | append (cmp-record "internal-json: syntax error" parse
    (run-both $farm nix-instantiate ...$json --eval -E "1 +")))
  $ok = ($ok | append (cmp-record "internal-json: nix eval, missing attribute" missing-attr
    (run-both $farm nix eval ...$json --expr "{ a = 1; }.b")))
  # A daemon client nix exits 1 on any failed build, and so do the classic
  # names; `ietsp build` exits with the build status (docs/reference/errors.md).
  let fail = 'derivation { name = "classic-e2e-fail"; system = "x86_64-linux"; builder = "/bin/sh"; args = [ "-c" "echo oops; exit 3" ]; }'
  $ok = ($ok | append (cmp-record "internal-json: a failed build" build-failed
    (run-both $farm nix-build ...$json --no-out-link -E $fail)))
  let fod = 'derivation { name = "classic-e2e-fod"; system = "x86_64-linux"; builder = "/bin/sh"; args = [ "-c" "echo hi > $out" ]; outputHashMode = "flat"; outputHash = "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="; }'
  $ok = ($ok | append (cmp-record "internal-json: a hash mismatch" hash-mismatch
    (run-both $farm nix-build ...$json --no-out-link -E $fod)))
  let bf = (^$ietsp build -E $fail | complete)
  $ok = ($ok | append (verdict ($bf.exit_code == 100) "ietsp build: a failed build exits 100" $"exit ($bf.exit_code)"))
  # a successful build's block ends with its ✔: nothing follows it
  let okdrv = ('derivation { name = "classic-e2e-order"; system = "x86_64-linux"; builder = "/bin/sh"; args = [ "-c" "echo from-the-builder; echo x > $out" ]; salt = "SALT"; }' | str replace SALT (random uuid))
  let ob = (^$ietsp build -E $okdrv | complete)
  let olines = ($ob.stderr | ansi strip | lines)
  let at = {|pat: string| $olines | enumerate | where {|r| $r.item | str contains $pat } | get -o 0.index }
  let ilog = (do $at "from-the-builder")
  let iok = (do $at "✔")
  $ok = ($ok | append (verdict ($ob.exit_code == 0 and $ilog != null and $iok != null and $ilog < $iok)
    "ietsp build: the ✔ is the last line of a successful block" $"exit ($ob.exit_code), stderr ($ob.stderr)"))
  let hm = (^$ietsp build ...$json -E $fod | complete)
  $ok = ($ok | append (verdict ($hm.exit_code == 102 and ($hm.stderr | str contains '"hint":"set the expected hash to '))
    "ietsp build: a hash mismatch exits 102 and names the hash" $"exit ($hm.exit_code), stderr '($hm.stderr | str substring 0..400)'"))
  # every record of the run, activity records included, has the one sid
  let hm_recs = ($hm.stderr | lines | where ($it | str starts-with "@nix ") | each { str substring 5.. | from json })
  let hm_sids = ($hm_recs | get -o sid | uniq)
  $ok = ($ok | append (verdict (($hm_recs | where action == start | length) > 0 and ($hm_sids | length) == 1 and ($hm_recs | all { $in.sid? != null }))
    "ietsp build: every record, activities too, has one sid" $"sids ($hm_sids | to json -r)"))
  # fix-hashes: the fake hash in the file becomes the one the build got, and the rebuild succeeds
  mkdir $"($d)/fix"
  $"{ pkg = ($fod); }\n" | save -f $"($d)/fix/default.nix"
  let fx = (do { cd $"($d)/fix"; ^$ietsp --ietsp-option fix-hashes always build ./default.nix -A pkg | complete })
  let fixed = (open --raw $"($d)/fix/default.nix")
  $ok = ($ok | append (verdict ($fx.exit_code == 0 and not ($fixed | str contains "AAAAAAAA") and ($fx.stderr | str contains "fixed: "))
    "ietsp build with fix-hashes: rewrites the hash and builds" $"exit ($fx.exit_code), file '($fixed)', stderr '($fx.stderr | str substring 0..400)'"))
  # the default asks, and with no terminal nobody can answer: the file stays
  $"{ pkg = ($fod); }\n" | save -f $"($d)/fix/default.nix"
  let fa = (do { cd $"($d)/fix"; ^$ietsp build ./default.nix -A pkg | complete })
  $ok = ($ok | append (verdict ($fa.exit_code == 102 and (open --raw $"($d)/fix/default.nix" | str contains "AAAAAAAA"))
    "ietsp build without a terminal: fix-hashes asks nobody, the file stays" $"exit ($fa.exit_code)"))
  # a fixed-output derivation with an input derivation is built as a resolved
  # copy, whose mismatch the daemon reports as a line, not as the build's result
  let fodin = ('derivation { name = "classic-e2e-fod-in"; system = "x86_64-linux"; builder = "/bin/sh"; args = [ "-c" "echo hi > $out" ]; dep = derivation { name = "classic-e2e-fod-dep"; system = "x86_64-linux"; builder = "/bin/sh"; args = [ "-c" "echo SALT > $out" ]; }; outputHashMode = "flat"; outputHash = "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="; }' | str replace SALT (random uuid))
  $"{ pkg = ($fodin); }\n" | save -f $"($d)/fix/default.nix"
  let fr = (do { cd $"($d)/fix"; ^$ietsp --ietsp-option fix-hashes always build ./default.nix -A pkg | complete })
  let frfile = (open --raw $"($d)/fix/default.nix")
  $ok = ($ok | append (verdict ($fr.exit_code == 0 and not ($frfile | str contains "AAAAAAAA") and ($fr.stderr | str contains "fixed: "))
    "ietsp build with fix-hashes: a resolved derivation's mismatch is fixed too" $"exit ($fr.exit_code), file '($frfile)', stderr '($fr.stderr | str substring 0..600)'"))

  # --- activity records: a build's stream has the shape of Nix's, and nom reads it
  let act = {|| 'derivation { name = "classic-e2e-act"; system = "x86_64-linux"; builder = "/bin/sh"; args = [ "-c" "echo top; echo $0 > $out" (derivation { name = "classic-e2e-act-dep"; system = "x86_64-linux"; builder = "/bin/sh"; args = [ "-c" "echo dep; echo x > $out" ]; salt = "SALT"; }) ]; }' | str replace SALT (random uuid) }
  mut streams = []
  for cmd in [[nix-build --no-out-link -E] [nix build --no-link --expr]] {
    let run = {|bin| ^$bin --log-format internal-json ...($cmd | skip 1) (do $act) | complete }
    let o = (do $run $"($farm)/($cmd.0)")
    let r = (do $run $cmd.0)
    let a = (act-shape $r.stderr)
    let b = (act-shape $o.stderr)
    $ok = ($ok | append (verdict ($o.exit_code == 0 and $r.exit_code == 0 and $a == $b and ($a.order | length) > 0)
      $"internal-json: ($cmd | take 2 | str join ' ') activity records match Nix's" $"exit ours=($o.exit_code) real=($r.exit_code)\n  real: ($a | to json -r)\n  ours: ($b | to json -r)"))
    $streams = ($streams | append $o.stderr)
  }
  let nom = (^nix build --no-link --print-out-paths -f $np nix-output-monitor | complete)
  let seen = if $nom.exit_code == 0 {
    $streams | each {|s| $s | ^$"($nom.stdout | chomp)/bin/nom" --json | complete | get stderr | ansi strip }
  } else { [$"cannot build nom: ($nom.stderr | str substring 0..400)"] }
  for s in $seen {
    $ok = ($ok | append (verdict ((not ($s | str contains "nix-output-monitor error")) and ($s | str contains "classic-e2e-act-dep") and ($s | str contains "Finished at"))
      "nom --json reads ietsp's activity records" $"nom said '($s | str substring (-600)..)'"))
  }

  # --- the roots a build keeps. `--no-out-link` and `--no-link` ask for no
  # symlink; nix then lets the output go, and ietsp keeps a root in its roots
  # store instead (docs/reference/divergences.md) — a side effect on the
  # daemon, so stdout and the exit code still match nix's byte for byte.
  # Native `ietsp build` roots every path it prints, for the same reason: the
  # caller uses those paths after the run.
  let brdrv = ('derivation { name = "classic-e2e-noout"; system = "@SYS@"; builder = "/bin/sh"; args = [ "-c" "echo x > $out" ]; salt = "SALT"; }' | str replace SALT (random uuid) | str replace -a "@SYS@" (current-system))
  let brdir = $"($tmp)/noout"
  mkdir $brdir
  cd $brdir
  for case in [
    {label: "nix-build --no-out-link -E", name: "nix-build", args: [--no-out-link -E]}
    {label: "nix build --no-link --print-out-paths", name: "nix", args: [build --no-link --print-out-paths --expr]}
  ] {
    let cache = $"($tmp)/broots-($case.name)"
    let r = (with-env {IETSP_CACHE_DIR: $cache} { run-both $farm $case.name ...$case.args $brdrv })
    $ok = ($ok | append (cmp-case $case.label $r))
    let links = (try { ls $"($cache)/roots" | get name | sort } catch { [] })
    let link = ($links | get -o 0 | default "")
    let target = (if ($link | is-not-empty) { ^readlink $link | chomp } else { "" })
    let held = (if ($target | is-not-empty) { (^nix-store --query --roots $target | complete).stdout | str contains $link } else { false })
    $ok = ($ok | append (verdict (($links | length) == 1 and $target == ($r.ours.stdout | chomp) and $held and (not ("result" | path exists)))
      $"($case.label) keeps a root instead of nothing" $"($links | length) links, '($link)' -> '($target)', held ($held), printed ($r.ours.stdout | chomp)"))
  }
  let broff = $"($tmp)/broots-off"
  let nbo = (with-env {IETSP_CACHE_DIR: $broff} { ^$"($farm)/nix-build" --ietsp-option build-gc-root false --no-out-link -E $brdrv | complete })
  $ok = ($ok | append (verdict ($nbo.exit_code == 0 and (not ($"($broff)/roots" | path exists)))
    "build-gc-root = false keeps no root, as in nix" $"exit ($nbo.exit_code), stderr ($nbo.stderr | str substring 0..200)"))
  let nvcache = $"($tmp)/broots-native"
  let nv = (with-env {IETSP_CACHE_DIR: $nvcache} { ^$ietsp build -E $brdrv | complete })
  let nvlinks = (try { ls $"($nvcache)/roots" | get name } catch { [] })
  let nvtarget = (if (($nvlinks | length) == 1) { ^readlink ($nvlinks | first) | chomp } else { "" })
  $ok = ($ok | append (verdict ($nv.exit_code == 0 and $nvtarget == ($nv.stdout | chomp))
    "ietsp build roots the path it printed" $"exit ($nv.exit_code), ($nvlinks | length) links -> '($nvtarget)', printed ($nv.stdout | chomp)"))
  cd $tmp

  # --- ietsp build --dry-run: nix build --dry-run's text, and the same sets as JSON
  let dry = (do $act)
  let dr = (^nix build --dry-run --expr $dry | complete)
  let dj = (^$ietsp build --dry-run --json -E $dry | complete)
  let plan = ($dj.stdout | from json)
  $ok = ($ok | append (verdict ($dj.exit_code == 0 and $dj.stderr == $dr.stderr and ($plan.willBuild | length) == 2 and ($dr.stderr | str contains ($plan.willBuild | str join "\n  ")))
    "ietsp build --dry-run: nix build --dry-run's text, and the plan as JSON" $"exit ($dj.exit_code)\n  real: ($dr.stderr)\n  ours: ($dj.stderr)\n  json: ($dj.stdout)"))

  # --- json-log-path: stderr stays text, the file gets the same records unprefixed
  let jlog = {|f| {stderr: (open --raw $f | lines | each {|l| $"@nix ($l)" } | str join "\n")} }
  let jo = (^$"($farm)/nix" eval --option json-log-path $"($d)/jlog-ours" --expr "{ a = 1; }.b" | complete)
  let jr = (^$"($env.IETSP_NIX_BIN_DIR)/nix" eval --option json-log-path $"($d)/jlog-real" --expr "{ a = 1; }.b" | complete)
  $ok = ($ok | append (cmp-record "json-log-path: the file holds the error record" missing-attr
    {ours: ({exit_code: $jo.exit_code} | merge (do $jlog $"($d)/jlog-ours")),
     real: ({exit_code: $jr.exit_code} | merge (do $jlog $"($d)/jlog-real"))}))
  $ok = ($ok | append (verdict ($jo.stderr | str starts-with "error: attribute 'b' missing")
    "json-log-path: stderr stays text" $"stderr '($jo.stderr | str substring 0..200)'"))
  let jb = (^$ietsp build --json-log-path $"($d)/jlog-build" -E (do $act) | complete)
  let jrecs = (open --raw $"($d)/jlog-build" | lines | each { from json })
  $ok = ($ok | append (verdict ($jb.exit_code == 0 and ($jrecs | where {|r| $r.action == "start" and $r.type? == 105 } | length) == 2 and not ($jb.stderr | str contains "@nix"))
    "json-log-path: the file gets a build's activity records, stderr stays text" $"exit ($jb.exit_code), stderr '($jb.stderr | str substring 0..300)'"))

  # --- a Determinate Nix nix.conf: the same result, and one warning per key
  # where Nix says "unknown setting"
  let dconf = $"($d)/determinate.conf"
  "lazy-trees = true\neval-cores = 0\n" | save -f $dconf
  let dn = (with-env {NIX_USER_CONF_FILES: $dconf} { run-both $farm nix-instantiate --eval -E "1 + 1" })
  $ok = ($ok | append (cmp-case "a Determinate nix.conf changes nothing" $dn))
  let dw = ($dn.ours.stderr | lines | where ($it | str contains "is a Determinate Nix setting; ietsp ignores it") | length)
  $ok = ($ok | append (verdict ($dw == 2 and ($dn.real.stderr | str contains "unknown setting 'lazy-trees'"))
    "a Determinate setting is one warning per key" $"ours '($dn.ours.stderr)' real '($dn.real.stderr)'"))
  let dj = (with-env {NIX_USER_CONF_FILES: $dconf} { ^$ietsp eval --log-format internal-json --no-lazy-trees --option eval-cores 4 -E "1" | complete })
  let drec = ($dj.stderr | lines | where ($it | str starts-with "@nix ") | each { str substring 5.. | from json } | where code? == "determinate-setting")
  $ok = ($ok | append (verdict ($dj.exit_code == 0 and $dj.stdout == "1\n" and ($drec | length) == 2 and ($drec | all { $in.level == 1 }))
    "a Determinate flag or --option is a level-1 determinate-setting record, once per key" $"stderr '($dj.stderr)'"))

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

  # --- nix derivation show: the derivation JSON of Nix 2.34.8, byte for byte.
  # Nothing here is built; the fixed-output case carries a hash no build matches.
  let fodshow = $"($tmp)/show-fod.nix"
  r#'derivation {
  name = "classic-e2e-show-fod";
  system = "x86_64-linux";
  builder = "/bin/sh";
  args = [ "-c" "echo x > $out" ];
  outputHashMode = "recursive";
  outputHashAlgo = "sha1";
  outputHash = "0000000000000000000000000000000000000000";
}
'# | save -f $fodshow
  let depshow = $"($tmp)/show-dep.nix"
  r#'let
  dep = derivation {
    name = "classic-e2e-show-dep";
    system = "x86_64-linux";
    outputs = [ "out" "dev" ];
    builder = "/bin/sh";
    args = [ "-c" "echo a > $out; echo b > $dev" ];
  };
in {
  top = derivation {
    name = "classic-e2e-show-top";
    system = "x86_64-linux";
    builder = "/bin/sh";
    args = [ "-c" "true" ];
    d = "${dep.dev}";
    t = builtins.toFile "note" "hello";
    __structuredAttrs = true;
  };
  inherit dep;
}
'# | save -f $depshow
  for case in [
    {label: "derivation show a custom drv", args: [-f $"($d)/default.nix"]}
    {label: "derivation show a multi-output drv", args: [-f $"($d)/multi.nix"]}
    {label: "derivation show a fixed-output drv", args: [-f $fodshow]}
    {label: "derivation show structured attrs and an input drv", args: [-f $depshow top]}
    {label: "derivation show two installables", args: [-f $depshow top dep]}
    {label: "derivation show --pretty", args: [--pretty -f $"($d)/multi.nix"]}
    {label: "derivation show --no-pretty", args: [--no-pretty -f $"($d)/multi.nix"]}
    {label: "derivation show .#pkg", args: [$"($fref)#pkg"]}
    {label: "derivation show -r over a real closure", args: [-r -f $depshow top]}
    {label: "derivation show -r over nixpkgs hello.src", args: [-r -I $"nixpkgs=($np)" -f $np hello.src]}
  ] {
    $ok = ($ok | append (cmp-case $case.label (run-both $farm nix derivation show ...$case.args)))
  }
  let miss = (run-both $farm nix derivation show -f $depshow nope)
  $ok = ($ok | append (cmp-case "derivation show an installable that does not exist" $miss))
  # only the first line: nix adds a "Did you mean" line that ietsp has nowhere
  # (docs/reference/classic.md).
  $ok = ($ok | append (verdict (($miss.ours.stderr | lines | get -o 0) == ($miss.real.stderr | lines | get -o 0))
    "derivation show an installable that does not exist: the same error line"
    $"ours '($miss.ours.stderr)' real '($miss.real.stderr)'"))
  # ietsp answers from the derivations the evaluation holds, so the store does
  # not gain the `.drv` the real nix registers (docs/reference/divergences.md).
  let saltshow = $"($tmp)/show-salt.nix"
  ('derivation { name = "classic-e2e-show-salt"; system = "x86_64-linux"; builder = "/bin/sh"; args = [ "-c" "true" ]; salt = "SALT"; }' | str replace SALT (random uuid)) | save -f $saltshow
  let so = (^$"($farm)/nix" derivation show -f $saltshow | complete)
  let sp = $"/nix/store/(($so.stdout | from json).derivations | columns | get -o 0 | default missing)"
  let before = ($sp | path exists)
  let sr = (^nix derivation show -f $saltshow | complete)
  $ok = ($ok | append (verdict ($so.stdout == $sr.stdout and not $before and ($sp | path exists))
    "derivation show does not instantiate, where the real nix does"
    $"($sp): ours left it ($before), real left it ($sp | path exists)"))
  for f in [[--stdin] [/nix/store/00000000000000000000000000000000-x.drv]] {
    let why = (fallback-reason $farm ([nix derivation show] | append $f))
    $ok = ($ok | append (verdict ($why | str contains "ietsp: falling back to nix:")
      $"derivation show ($f | str join ' ') falls back" $"stderr was '($why)'"))
  }

  # --- nix-shell: the derivation's inputs, then bash on nix-build.cc's rc file.
  # `<nixpkgs>` resolves to nixpkgs/shell.nix, which fetches a pinned tree, so
  # the real code path needs default.nix by name.
  cd $tmp
  let pkgs_nix = $"($np)/default.nix"
  $ok = ($ok | append (cmp-case "nix-shell nixpkgs -A hello --run 'echo $name'"
    (run-both $farm nix-shell -I $"nixpkgs=($np)" $pkgs_nix -A hello --run 'echo $name')))
  $ok = ($ok | append (cmp-env "nix-shell nixpkgs -A hello --run env"
    (run-both $farm nix-shell -I $"nixpkgs=($np)" $pkgs_nix -A hello --run env)))
  $ok = ($ok | append (cmp-env "nix-shell --pure --run env"
    (run-both $farm nix-shell -I $"nixpkgs=($np)" $pkgs_nix -A hello --pure --run env)))
  $ok = ($ok | append (cmp-case "nix-shell --pure --keep FOO"
    (with-env {FOO: "bar"} { run-both $farm nix-shell -I $"nixpkgs=($np)" $pkgs_nix -A hello --pure --keep FOO --run 'echo "[$FOO]"' })))
  # --command is interactive unless stdin is not a tty: an empty pipe settles it.
  let cmd_ours = ("" | ^$"($farm)/nix-shell" -I $"nixpkgs=($np)" $pkgs_nix -A hello --command 'echo $name' | complete)
  let cmd_real = ("" | ^nix-shell -I $"nixpkgs=($np)" $pkgs_nix -A hello --command 'echo $name' | complete)
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

  # --- nix-shell keeps what it realised under GC roots of its own. The inputs
  # are not one store path, so the roots store takes one link each. The
  # collection is simulated on a scratch chroot daemon; `nix collect-garbage`
  # never runs on this machine's store. `NIX_BUILD_SHELL` keeps `<nixpkgs>`
  # out of a fixture that has none.
  let sdir = $"($tmp)/shellroot"
  mkdir $sdir
  r#'let
  dep = derivation {
    name = "shellroot-dep";
    system = "@SYS@";
    builder = "/bin/sh";
    args = [ "-c" "echo building the dep >&2; echo @NONCE@ > $out" ];
  };
in derivation {
  name = "shellroot-shell";
  system = "@SYS@";
  builder = "/bin/sh";
  args = [ "-c" "echo hi > $out" ];
  DEP = dep;
}
'# | str replace -a "@SYS@" (current-system) | str replace -a "@NONCE@" $"(random int 0..999999)" | save -f $"($sdir)/shell.nix"
  # a short name: the daemon socket path has to fit in `sockaddr_un`
  let sstore = $"($tmp)/sr"
  mkdir $sstore
  let ssock = (scratch-daemon $sstore --option substituters "" --option builders "")
  let sscratch = $"local?root=($sstore)"
  let scache = $"($tmp)/shellroots"
  let senv = {IETSP_CACHE_DIR: $scache, NIX_BUILD_SHELL: (^bash -c 'command -v bash' | chomp)}
  cd $sdir
  let s1 = (with-env $senv { ^$"($farm)/nix-shell" --option store $"unix://($ssock)" --run 'true' | complete })
  let slinks = (try { ls $"($scache)/roots" | get name | sort } catch { [] })
  let slink = ($slinks | get -o 0 | default "")
  let starget = (if ($slink | is-not-empty) { ^readlink $slink | chomp } else { "" })
  let sheld = (if ($starget | is-not-empty) { (^nix-store --store $sscratch --query --roots $starget | complete).stdout | str contains $slink } else { false })
  $ok = ($ok | append (verdict ($s1.exit_code == 0 and ($s1.stderr | str contains "building the dep") and ($slinks | length) == 1 and ($starget | str ends-with "-shellroot-dep") and $sheld)
    "nix-shell roots the input it realised" $"exit ($s1.exit_code), ($slinks | length) links, '($slink)' -> '($starget)', held: ($sheld)"))
  # the simulated collection keeps the rooted output, so the second entry
  # builds nothing
  let sgc = (^nix-store --store $sscratch --gc | complete)
  let skept = ($"($sstore)($starget)" | path exists)
  let s2 = (with-env $senv { ^$"($farm)/nix-shell" --option store $"unix://($ssock)" --run 'true' | complete })
  $ok = ($ok | append (verdict ($sgc.exit_code == 0 and $skept and $s2.exit_code == 0 and (not ($s2.stderr | str contains "building the dep")))
    "a collection leaves nix-shell's root, and the next entry builds nothing" $"kept ($skept), stderr ($s2.stderr | str substring 0..200)"))
  # `ietsp cache gc` walks the roots store and releases the link
  let scgc = (^$ietsp cache gc --ttl 0 --cache-dir $scache | complete)
  $ok = ($ok | append (verdict ($scgc.exit_code == 0 and ($scgc.stdout | str contains $"($scache)/roots: 1 of 1 entries") and (not ($slink | path exists)))
    "ietsp cache gc sweeps nix-shell's roots" $"exit ($scgc.exit_code), stdout ($scgc.stdout)"))
  # the switch, and the proof that the root is what held the output: with
  # `shell-gc-root` off, the same collection takes it and the next entry builds
  let soff = $"($tmp)/shellroots-off"
  let offenv = {IETSP_CACHE_DIR: $soff, NIX_BUILD_SHELL: ($senv.NIX_BUILD_SHELL)}
  let s3 = (with-env $offenv { ^$"($farm)/nix-shell" --ietsp-option shell-gc-root false --option store $"unix://($ssock)" --run 'true' | complete })
  let off_roots = ($"($soff)/roots" | path exists)
  let sgc2 = (^nix-store --store $sscratch --gc | complete)
  let s4 = (with-env $offenv { ^$"($farm)/nix-shell" --ietsp-option shell-gc-root false --option store $"unix://($ssock)" --run 'true' | complete })
  $ok = ($ok | append (verdict ($s3.exit_code == 0 and (not $off_roots) and $sgc2.exit_code == 0 and ($s4.stderr | str contains "building the dep"))
    "shell-gc-root = false keeps no root, and the collection takes the output" $"roots store ($off_roots), stderr ($s4.stderr | str substring 0..200)"))
  cd $tmp

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

  # nix develop keeps the `-env` output under a GC root of its own: one link in
  # the roots store, keyed on the flake directory and the attribute path, which
  # a second entry replaces in place. The output references the whole shell, so
  # one link holds all of it.
  let devcache = $"($tmp)/devcache"
  let dev = (with-env {IETSP_CACHE_DIR: $devcache} { ^$"($farm)/nix" develop $repo -c true | complete })
  let again = (with-env {IETSP_CACHE_DIR: $devcache} { ^$"($farm)/nix" develop $repo -c true | complete })
  let dlinks = (try { ls $"($devcache)/roots" | get name | sort } catch { [] })
  let dlink = ($dlinks | get -o 0 | default "")
  let dtarget = (if ($dlink | is-not-empty) { ^readlink $dlink | chomp } else { "" })
  let dheld = (if ($dtarget | is-not-empty) { (^nix-store --query --roots $dtarget | complete).stdout | str contains $dlink } else { false })
  $ok = ($ok | append (verdict ($dev.exit_code == 0 and $again.exit_code == 0 and ($dlinks | length) == 1 and ($dtarget | str ends-with "-env") and $dheld)
    "nix develop keeps the shell's environment under a GC root" $"($dlinks | length) links, '($dlink)' -> '($dtarget)', held: ($dheld)"))
  # the switch: no link, and no roots store either
  let devoff = $"($tmp)/devcache-off"
  let dv = (with-env {IETSP_CACHE_DIR: $devoff} { ^$"($farm)/nix" develop --ietsp-option shell-gc-root false $repo -c true | complete })
  $ok = ($ok | append (verdict ($dv.exit_code == 0 and (not ($"($devoff)/roots" | path exists)))
    "shell-gc-root = false keeps no root for nix develop" $"exit ($dv.exit_code), stderr ($dv.stderr | str substring 0..200)"))

  # --- nix fmt: the flake's formatter.<system>, built once and kept alive by
  # an out-link that is also a GC root. The formatter needs a `mkdir`, and the
  # build sandbox holds only the derivation's inputs, so nixpkgs comes in as a
  # `path:` input the real `nix flake lock` locks offline.
  let sys = (current-system)
  let fmtdir = $"($tmp)/fmtflake"
  let fmtcache = $"($tmp)/fmtcache"
  mkdir $fmtdir
  r##'{
  inputs.nixpkgs.url = "path:@NIXPKGS@";
  outputs = { self, nixpkgs }:
    let
      system = "@SYS@";
      cu = nixpkgs.legacyPackages.${system}.coreutils;
    in {
      formatter.${system} = derivation {
        name = "fmt-e2e-1.0";
        inherit system;
        builder = "/bin/sh";
        args = [ "-c" "${cu}/bin/mkdir -p $out/bin && echo '#!/bin/sh' > $out/bin/fmt-e2e && echo 'echo \"PRJ_ROOT=$PRJ_ROOT\"' >> $out/bin/fmt-e2e && echo 'echo \"args:$@\"' >> $out/bin/fmt-e2e && ${cu}/bin/chmod +x $out/bin/fmt-e2e" ];
      };
    };
}
'## | str replace -a "@NIXPKGS@" $np | str replace -a "@SYS@" $sys | save -f $"($fmtdir)/flake.nix"
  let flock = (do { cd $fmtdir; ^nix flake lock } | complete)
  if $flock.exit_code != 0 { print $"FAIL nix flake lock: ($flock.stderr)" }
  $ok = ($ok | append (verdict ($flock.exit_code == 0) "nix flake lock for the fmt fixture" $flock.stderr))
  # the out-link goes under this cache root, so the test can find it
  $env.IETSP_CACHE_DIR = $fmtcache
  cd $fmtdir
  # `name` has a version and there is no `meta.mainProgram`, so the program is
  # names.cc:DrvName's `fmt-e2e`
  $ok = ($ok | append (cmp-case "nix fmt with no arguments" (run-both $farm nix fmt)))
  $ok = ($ok | append (cmp-case "nix fmt with paths"
    (run-both $farm nix fmt a.nix sub/b.nix)))
  let dd_ours = (^$"($farm)/nix" fmt -- --write x.nix | complete)
  let dd_real = (^nix fmt -- --write x.nix | complete)
  $ok = ($ok | append (cmp-case "nix fmt -- --write x.nix" {ours: $dd_ours, real: $dd_real}))
  # an edit invalidates the entry. Nix says nothing when its evaluation cache
  # misses, so neither does the classic name; json-log-path gets the record
  "\n# edited\n" | save --append $"($fmtdir)/flake.nix"
  let inv = (^$"($farm)/nix" fmt --option json-log-path $"($tmp)/fmt-jlog" | complete)
  let inv_recs = (try { open --raw $"($tmp)/fmt-jlog" | lines | each { from json } | where code? == "cache-invalidated" | length } catch { 0 })
  $ok = ($ok | append (verdict ($inv.exit_code == 0 and (not ($inv.stderr | str contains "re-evaluating")) and $inv_recs == 1)
    "nix fmt: an invalidation is silent on stderr and one record in json-log-path" $"($inv_recs) records, stderr ($inv.stderr | str substring 0..200)"))

  # the link is in the roots store, points into /nix/store, and the daemon
  # holds it: `nix-store --query --roots` names it
  let links = (try { ls $"($fmtcache)/roots" | get name } catch { [] })
  let link = ($links | get -o 0 | default "")
  let target = (if ($link | is-not-empty) { ^readlink $link | chomp } else { "" })
  let held = (if ($target | is-not-empty) { (^nix-store --query --roots $target | complete).stdout | str contains $link } else { false })
  $ok = ($ok | append (verdict (($links | length) == 1 and ($target | str starts-with "/nix/store/") and $held)
    "nix fmt keeps the formatter under a GC root" $"link '($link)' -> '($target)', held: ($held)"))
  # `ietsp cache gc` walks the roots store: a zero TTL drops the one link
  let gc = (^$ietsp cache gc --ttl 0 --cache-dir $fmtcache | complete)
  $ok = ($ok | append (verdict ($gc.exit_code == 0 and ($gc.stdout | str contains $"($fmtcache)/roots: 1 of 1 entries") and (not ($link | path exists)))
    "ietsp cache gc sweeps the roots store" $"exit ($gc.exit_code), stdout ($gc.stdout)"))
  hide-env IETSP_CACHE_DIR

  # a flake with no `formatter` output: nix's own message, and no fallback
  cd $fdir
  let nofmt = (run-both $farm nix fmt)
  let want_msg = $"does not provide attribute 'formatter.($sys)'"
  $ok = ($ok | append (cmp-case "nix fmt without a formatter output" $nofmt))
  $ok = ($ok | append (verdict (($nofmt.ours.stderr | str contains $want_msg) and ($nofmt.real.stderr | str contains $want_msg) and (not ($nofmt.ours.stderr | str contains "falling back")))
    "nix fmt names the missing attribute itself" $"ours stderr '($nofmt.ours.stderr)'"))
  cd $tmp

  # --- fallbacks, priced with IETSP_NO_FALLBACK so the real run never happens
  for case in [
    {label: "nix fmt --json falls back", want: "'--json' is not implemented"
     args: [nix fmt --json]}
    {label: "nix-shell -p falls back", want: "'-p' is not implemented"
     args: [nix-shell -p hello --run "true"]}
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
  $ok = ($ok | append (cmp-case "nix-shell <nixpkgs> fetches the sha256-pinned tarball of nixpkgs/shell.nix itself"
    (with-env {IETSP_NO_FALLBACK: "1"} { run-both $farm nix-shell -I $"nixpkgs=($np)" "<nixpkgs>" -A hello --run 'echo $name' })))
  let shebang = $"($tmp)/script.sh"
  "#! /usr/bin/env nix-shell\n#! nix-shell -p hello -i bash\necho hi\n" | save -f $shebang
  let err = (fallback-reason $farm [nix-shell $shebang])
  $ok = ($ok | append (verdict ($err | str contains "a '#!' script") "nix-shell falls back on a #! script" $"stderr was '($err)'"))

  let meta = (^$"($farm)/nix" flake metadata $fref | complete)
  $ok = ($ok | append (verdict ($meta.stderr | str contains "falling back to nix: subcommand 'flake'")
    "nix flake metadata falls back" $"stderr was '($meta.stderr)'"))
  for case in [
    {ref: "nixpkgs#lib.version", want: "flake registry references (nixpkgs#…) are not supported"}
    {ref: "github:NixOS/nixpkgs#lib.version", want: "only local paths are supported"}
  ] {
    let err = (fallback-reason $farm [nix eval $case.ref])
    $ok = ($ok | append (verdict (($err | str contains "falling back to nix: ietsp: ") and ($err | str contains $case.want))
      $"nix eval ($case.ref) falls back" $"stderr was '($err)'"))
    let r = (^$ietsp eval $case.ref | complete)
    $ok = ($ok | append (verdict ($r.exit_code == 1 and ($r.stderr | str contains $case.want))
      $"ietsp eval ($case.ref) says why" $"exit ($r.exit_code), stderr '($r.stderr)'"))
  }
  let apply = (^$"($farm)/nix" eval --apply "x: x" --expr 1 | complete)
  $ok = ($ok | append (verdict ($apply.stderr | str contains "'--apply' is not implemented")
    "nix eval --apply falls back" $"stderr was '($apply.stderr)'"))

  cd /
  # The scratch store's directories are read-only, as every Nix store is.
  ^chmod -R u+w $"($tmp)/scratch" err> /dev/null | complete | ignore
  rm -rf $tmp
  let failed = ($ok | where $it == false | length)
  print $"($ok | length) cases, ($failed) failed"
  if $failed > 0 { exit 1 }
}

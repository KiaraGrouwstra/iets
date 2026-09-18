#!/usr/bin/env nu
# Scheduling benchmark over the worker protocol: every lever of `ietsp build` against a
# scratch nix-daemon, plus plain Nix (`nix copy` then `nix build`) as the baseline.
# Results and the reading are in docs/scheduling.md.
#
# Targets:
#   shim   ssh-ng://shim: an `ssh` shim in PATH runs `nix-daemon --stdio` on a scratch store
#          here, under ietsp-shim, which adds IETSP_SCHED_LATENCY_MS per direction and logs
#          the daemon's own CPU time. One row per value of --latency.
#   unix   unix://SOCK: a socat listener that runs the same daemon per connection.
#   host   ssh-ng://HOST (--host): a real ssh target; the remote program is a scratch-store
#          `nix-daemon --stdio` under /tmp/ietsp-sched, run under a shipped ietsp-shim for
#          the same CPU accounting. Rows with and without ControlMaster (NIX_SSHOPTS).
#          Upload only (`--no-build`, upload configs): a chroot store under /tmp cannot
#          build on bld1 (docs/scheduling.md, "bld1 leg"), and the hello seed lives here.
#
# usage: nu scripts/sched-bench.nu [--host HOST] [--runs 3] [--latency 0,20]
#          [--workloads g8x6,g32x4,chain30,hello,nixos-desktop] [--configs a,b,...]
#          [--targets shim,unix,host] [--out bench/results/sched]
#        BIN=target/release NIXPKGS=/nix/store/...-source
# Needs the dev shell (nix-daemon 2.34, socat) and a hello seed store at
# ROOT/hello-seed/store (see docs/scheduling.md, "hello").
use lib.nu [chomp]

const NIXPKGS_DEFAULT = "/nix/store/ah3pxz4xa52hd6vhbx17vcsci0b8bmbr-source"
const HELLO_EXPR = '(import <nixpkgs> {}).hello.overrideAttrs (_: { name = "hello-ietsp-2.12.3"; })'

# The scratch root: ROOT/bin (shims), ROOT/<target>/store, ROOT/hello-seed/store.
def root []: nothing -> string { $env.IETSP_SCHED_ROOT? | default $"($env.HOME)/tmp/ietsp-sched" }

def --wrapped out-of [...cmd: string]: nothing -> string {
  let r = (^($cmd | first) ...($cmd | skip 1) | complete)
  if $r.exit_code != 0 { error make -u {msg: $"($cmd | str join ' ') failed: ($r.stderr)"} }
  $r.stdout
}

# ---- workloads -------------------------------------------------------------------------

def workloads [nixpkgs: string]: nothing -> list<record> {
  let graph = {|name, w, d| {
    name: $name, class: graph, build: true,
    args: [--arg width $"($w)" --arg depth $"($d)" bench/graph.nix],
    instantiate: [bench/graph.nix --arg width $"($w)" --arg depth $"($d)"],
  } }
  [
    (do $graph g8x6 8 6)
    (do $graph g32x4 32 4)
    (do $graph chain30 1 30)
    {
      name: hello, class: hello, build: true,
      args: [-I $"nixpkgs=($nixpkgs)" -E $HELLO_EXPR],
      instantiate: [-I $"nixpkgs=($nixpkgs)" -E $HELLO_EXPR],
      seed: hello-seed, substituters: "https://cache.nixos.org",
    }
    {
      name: nixos-desktop, class: upload, build: false,
      args: [--no-build -I $"nixpkgs=($nixpkgs)" bench/workloads/nixos-desktop.nix],
      instantiate: [--eval --strict --raw -I $"nixpkgs=($nixpkgs)" bench/workloads/nixos-desktop.nix],
    }
  ]
}

# ---- configurations: the levers ----------------------------------------------------------

def configs []: nothing -> list<record> {
  [
    {name: ietsp,           classes: [graph hello upload], flags: []}
    {name: nix,             classes: [graph hello upload], flags: null}
    {name: per-drv,         classes: [graph],              flags: [--per-drv]}
    {name: batch4,          classes: [graph],              flags: [--build-batch 4]}
    {name: batch16,         classes: [graph],              flags: [--build-batch 16]}
    {name: batch-all,       classes: [graph hello],        flags: [--build-batch 0]}
    {name: conn2-batch-all, classes: [graph],              flags: [--connections 2 --build-batch 0]}
    {name: conn4-batch-all, classes: [graph hello],        flags: [--connections 4 --build-batch 0]}
    {name: conn4-per-drv,   classes: [graph],              flags: [--connections 4 --per-drv]}
    {name: overlap4,        classes: [graph hello],        flags: [--connections 4 --overlap --upload-batch 32]}
    {name: critical,        classes: [graph],              flags: [--critical-path --build-batch 0]}
    {name: critical-conn4,  classes: [graph],              flags: [--connections 4 --critical-path --build-batch 0]}
    {name: skip-sub,        classes: [hello upload],       flags: [--skip-substitutable]}
    {name: ub256,           classes: [hello upload],       flags: [--upload-batch 256]}
    {name: ub32,            classes: [upload],             flags: [--upload-batch 32]}
    {name: ub256-2c,        classes: [hello upload],       flags: [--connections 2 --upload-connections 2 --upload-batch 256]}
    {name: all,             classes: [graph hello],        flags: [--connections 4 --overlap --critical-path --upload-batch 256 --build-batch 0]}
  ]
}

# ---- scratch daemons ---------------------------------------------------------------------

const DAEMON_STDIO = '#!/bin/sh
# nix-daemon --stdio on the scratch store, under ietsp-shim for latency and CPU accounting.
# Not exec'"'"'d: Nix SIGKILLs its ssh child at teardown (processes.cc:Pid::kill), which must
# not take ietsp-shim down before it logs the daemon'"'"'s CPU time.
root=${IETSP_SCHED_ROOT:?}
ietsp-shim --latency-ms "${IETSP_SCHED_LATENCY_MS:-0}" --cpu-log "$root/cpu.log" -- \
  nix-daemon --store "local?root=$root/store" --option builders "" \
  --option substituters "${IETSP_SCHED_SUBSTITUTERS:-}" --stdio 2>>"$root/daemon.log"
'

const SSH_SHIM = '#!/bin/sh
# ssh shim: ignore host and command, serve the scratch daemon on stdio here.
# Nix (libstore/ssh.cc) passes -oLocalCommand="echo started" and reads that line from
# ssh'"'"'s stdout before it speaks the protocol; `ssh -O check` probes for a master (none);
# a -M master only has to answer and idle until Nix kills it.
started=
for a in "$@"; do
  case "$a" in
    -O) exit 255 ;;
    -M) echo started; exec sleep 100000 ;;
    "-oLocalCommand=echo started") started=1 ;;
  esac
done
[ -n "$started" ] && echo started
exec "$(dirname "$0")/daemon-stdio"
'

# The remote program on a real host: the same daemon under the shipped ietsp-shim, run
# through its own loader from /tmp/ietsp-sched/lib. Uploads only, so no build settings.
const REMOTE_STDIO = '#!/bin/sh
root=/tmp/ietsp-sched
$root/lib/@LD@ --library-path $root/lib $root/ietsp-shim --cpu-log $root/cpu.log -- \
  nix-daemon --store "local?root=$root/store" --option builders "" --option substituters "" \
  --stdio 2>>$root/daemon.log
'

def write-shims [] {
  let bin = $"(root)/bin"
  mkdir $bin
  $DAEMON_STDIO | save -f $"($bin)/daemon-stdio"
  $SSH_SHIM | save -f $"($bin)/ssh"
  # socat's EXEC gets no environment of ours: a wrapper pins the unix target's root
  $"#!/bin/sh\nexport IETSP_SCHED_ROOT=(root)/unix\nexec (root)/bin/daemon-stdio\n" | save -f $"($bin)/unix-stdio"
  ^chmod +x $"($bin)/daemon-stdio" $"($bin)/ssh" $"($bin)/unix-stdio"
}

def target-root [t: record]: nothing -> string { $"(root)/($t.name)" }

# Clears the store of a target for a cold run; `seed` copies a prepared store instead.
def wipe-store [t: record, seed?: string] {
  # A fresh store gets its SQLite schema now: N daemons creating it at once can die with
  # "SQLite database is busy" (the busy handler is not yet installed while the schema is
  # created), and a real remote store has one already.
  if $t.kind == host {
    ^ssh $t.host "rm -rf /tmp/ietsp-sched/store /tmp/ietsp-sched/cpu.log; mkdir -p /tmp/ietsp-sched/store; nix-store --store 'local?root=/tmp/ietsp-sched/store' --init 2>/dev/null"
    return
  }
  let store = $"(target-root $t)/store"
  if ($store | path exists) {
    ^chmod -R u+w $store
    rm -rf $store
  }
  if $seed != null {
    ^cp -a $"(root)/($seed)/store" $store
  } else {
    mkdir $store
    ^nix-store --store $"local?root=($store)" --init err> /dev/null
  }
}

def reset-cpu [t: record] {
  if $t.kind == host {
    ^ssh $t.host "rm -f /tmp/ietsp-sched/cpu.log"
  } else {
    rm -f $"(target-root $t)/cpu.log"
  }
}

# Seconds of daemon CPU logged since `reset-cpu`.
def read-cpu [t: record]: nothing -> float {
  sleep 300ms
  let f = $"(target-root $t)/cpu.log"
  let text = if $t.kind == host {
    ^ssh $t.host "cat /tmp/ietsp-sched/cpu.log 2>/dev/null"
  } else if ($f | path exists) {
    open --raw $f
  } else { "" }
  # a shim may still be appending: take complete `pid seconds` lines only
  let v = ($text | lines | parse -r '^(?<pid>\d+) (?<s>\d+\.\d+)$' | get s | each { into float })
  if ($v | is-empty) { 0.0 } else { $v | math sum }
}

# ---- one run -----------------------------------------------------------------------------

def num [s: string]: nothing -> float { $s | into float }

# `ietsp build` once; the parsed `--timings` lines plus wall and daemon CPU.
def run-ietsp [t: record, wl: record, cfg: record, bin: string]: nothing -> record {
  reset-cpu $t
  let t0 = (date now)
  let r = (with-env (target-env $t $wl) {
    ^$"($bin)/ietsp" build --builder $t.url --timings ...$wl.args ...$cfg.flags | complete
  })
  let wall = (((date now) - $t0) | into int) / 1e9
  if $r.exit_code != 0 {
    print -e ($r.stderr | lines | last 5 | str join "\n")
    error make -u {msg: $"ietsp build failed: ($wl.name) ($cfg.name) on ($t.name)"}
  }
  let lines = ($r.stderr | lines)
  let pick = {|re| $lines | where $it =~ $re | last | default "" }
  let q = (do $pick '^query:' | parse -r 'query: (?<paths>\d+) paths, (?<missing>\d+) missing, (?<s>[\d.]+)s' | get -o 0)
  let a = (do $pick '^add:' | parse -r 'add: (?<paths>\d+) paths, (?<bytes>\d+) bytes, (?<s>[\d.]+)s' | get -o 0)
  let b = (do $pick '^build:' | parse -r 'build: (?<s>[\d.]+)s' | get -o 0)
  let w = (do $pick '^worker:' | parse -r 'worker: (?<ops>\d+) ops, (?<rtt>\d+) round trips, (?<sent>\d+) bytes sent' | get -o 0)
  let s = (do $pick '^sched:' | parse -r 'sched: (?<conns>\d+) connections, (?<addops>\d+) add ops, (?<buildops>\d+) build ops, first build (?<first>[\d.]+s|none), total (?<total>[\d.]+)s' | get -o 0)
  let k = (do $pick '^skip:' | parse -r 'skip: (?<sub>\d+) substitutable, (?<skipped>\d+) skipped' | get -o 0)
  {
    wall: $wall,
    total: (num $s.total),
    first_build: (if $s.first == "none" { null } else { $s.first | str replace "s" "" | into float }),
    query: (num $q.s), missing: ($q.missing | into int),
    add: (if $a == null { 0.0 } else { num $a.s }), bytes: (if $a == null { 0 } else { $a.bytes | into int }),
    build: (if $b == null { 0.0 } else { num $b.s }),
    ops: ($w.ops | into int), sent: ($w.sent | into int), conns: ($s.conns | into int),
    skipped: (if $k == null { 0 } else { $k.skipped | into int }),
    cpu: (read-cpu $t),
  }
}

# Plain Nix once: `nix copy` of the drv closure, then `nix build` through the same daemon.
def run-nix [t: record, wl: record, drv: string]: nothing -> record {
  reset-cpu $t
  let e = (target-env $t $wl)
  let t0 = (date now)
  let c = (with-env $e { ^nix copy --to $t.url --derivation $drv | complete })
  let copy = (((date now) - $t0) | into int) / 1e9
  if $c.exit_code != 0 { print -e ($c.stderr | lines | last 3 | str join "\n"); error make -u {msg: $"nix copy failed on ($t.name)"} }
  mut build = 0.0
  if $wl.build {
    let t1 = (date now)
    let b = (with-env $e { ^nix build --max-jobs 16 --store $t.url --eval-store auto --no-link $"($drv)^*" | complete })
    $build = (((date now) - $t1) | into int) / 1e9
    if $b.exit_code != 0 { print -e ($b.stderr | lines | last 3 | str join "\n"); error make -u {msg: $"nix build failed on ($t.name)"} }
  }
  {
    wall: ($copy + $build), total: ($copy + $build), first_build: null,
    query: 0.0, missing: 0, add: $copy, bytes: 0, build: $build,
    ops: 0, sent: 0, conns: 1, skipped: 0, cpu: (read-cpu $t),
  }
}

def target-env [t: record, wl: record]: nothing -> record {
  mut e = {
    IETSP_SCHED_ROOT: (target-root $t),
    IETSP_SCHED_LATENCY_MS: $"($t.latency)",
    IETSP_SCHED_SUBSTITUTERS: ($wl.substituters? | default ""),
    PATH: ($env.PATH | prepend $"(root)/bin"),
  }
  if $t.kind == host {
    $e.PATH = $env.PATH
    $e.NIX_SSHOPTS = ($t.sshopts? | default "")
  }
  $e
}

# ---- targets -----------------------------------------------------------------------------

def make-targets [host: string, latencies: list<int>, names: list<string>]: nothing -> list<record> {
  mut ts = []
  if "shim" in $names {
    for l in $latencies {
      $ts = ($ts | append {name: $"shim($l)", kind: shim, url: "ssh-ng://shim", latency: $l, label: $"local shim, ($l) ms per direction"})
    }
  }
  if "unix" in $names {
    let sock = $"(root)/unix/sock"
    $ts = ($ts | append {name: unix, kind: unix, url: $"unix://($sock)", latency: 0, label: "unix socket, socat listener"})
  }
  if "host" in $names and $host != "" {
    let prog = "/tmp/ietsp-sched/daemon-stdio"
    let url = $"ssh-ng://($host)?remote-program=($prog)"
    let cm = $"-o ControlMaster=auto -o ControlPath=(root)/cm-%C -o ControlPersist=300"
    $ts = ($ts | append {name: host, kind: host, url: $url, latency: 0, host: $host, sshopts: "", label: $"($host), no ControlMaster"})
    $ts = ($ts | append {name: host-cm, kind: host, url: $url, latency: 0, host: $host, sshopts: $cm, label: $"($host), ControlMaster"})
  }
  $ts
}

# Ships ietsp-shim with its loader and libraries under /tmp/ietsp-sched on the host (the
# host's real store is never touched), and the remote program that runs the daemon under it.
def setup-host [host: string, bin: string] {
  let shim = ($"($bin)/ietsp-shim" | path expand)
  let libs = (^ldd $shim | lines | parse -r '(?<path>/nix/store/\S+)' | get path | each { |p| ^readlink -f $p | chomp } | uniq)
  let ld = ($libs | where $it =~ 'ld-linux' | first)
  let stage = (mktemp -d)
  mkdir $"($stage)/lib"
  for l in $libs { ^cp $l $"($stage)/lib/" }
  ^cp $shim $"($stage)/ietsp-shim"
  $REMOTE_STDIO | str replace "@LD@" ($ld | path basename) | save -f $"($stage)/daemon-stdio"
  ^chmod +x $"($stage)/daemon-stdio"
  ^sh -c $"tar -C ($stage) -cf - . | ssh ($host) 'mkdir -p /tmp/ietsp-sched/store && tar -C /tmp/ietsp-sched -xf -'"
  rm -rf $stage
  let rtt = (^sh -c $"start=$\(date +%s%N\); ssh -o ControlMaster=no ($host) true; echo $\(\( \($\(date +%s%N\) - start\) / 1000000 \))" | chomp)
  print $"host ($host): ssh round trip incl. login ($rtt) ms"
}

# ---- the matrix --------------------------------------------------------------------------

def median-of [rows: list<record>, col: string]: nothing -> any {
  let v = ($rows | get $col | where $it != null)
  if ($v | is-empty) { null } else { $v | math median }
}

def fmt-s [v: any]: nothing -> string { if $v == null { "-" } else { $v | math round --precision 2 | into string } }
def fmt-b [v: any]: nothing -> string { if $v == null or $v == 0 { "-" } else { ($v / 1e6) | math round --precision 2 | $"($in) MB" } }

# One markdown table per target and workload: rows = configs, cold and warm medians.
def report [results: list<record>]: nothing -> string {
  mut out = []
  for t in ($results | get target | uniq) {
    let rt = ($results | where target == $t)
    $out = ($out | append $"### ($rt.0.label)\n")
    for wl in ($rt | get workload | uniq) {
      let rw = ($rt | where workload == $wl)
      $out = ($out | append $"**($wl)**\n")
      $out = ($out | append "| config | cold s | first build s | warm s | upload s | build s | bytes sent | ops | conns | daemon CPU s |")
      $out = ($out | append "|---|---|---|---|---|---|---|---|---|---|")
      for cfg in ($rw | get config | uniq) {
        let cold = ($rw | where config == $cfg and state == cold)
        let warm = ($rw | where config == $cfg and state == warm)
        $out = ($out | append ($"| ($cfg) | (fmt-s (median-of $cold total)) | (fmt-s (median-of $cold first_build)) | (fmt-s (median-of $warm total)) | (fmt-s (median-of $cold add)) | (fmt-s (median-of $cold build)) | (fmt-b (median-of $cold sent)) | ($cold.0.ops) | ($cold.0.conns) | (fmt-s (median-of $cold cpu)) |"))
      }
      $out = ($out | append "")
    }
  }
  $out | str join "\n"
}

def main [
  --host: string = "",            # a real ssh-ng target, e.g. root@bld1.ztm.io
  --runs: int = 3,
  --latency: string = "0,20",     # ms per direction for the local shim
  --workloads: string = "g8x6,g32x4,chain30,hello,nixos-desktop",
  --configs: string = "",         # default: every config that applies
  --targets: string = "shim,unix,host",
  --out: string = "bench/results/sched",
] {
  cd ($env.FILE_PWD | path join "..")
  $env.NIX_REMOTE = ($env.NIX_REMOTE? | default "daemon")
  let bin = ($env.BIN? | default "target/release")
  let nixpkgs = ($env.NIXPKGS? | default $NIXPKGS_DEFAULT)
  $env.PATH = ($env.PATH | prepend ($bin | path expand))
  write-shims
  let latencies = ($latency | split row "," | each { into int })
  let tnames = ($targets | split row ",")
  let ts = (make-targets $host $latencies $tnames)
  if ($ts | is-empty) { error make -u {msg: "no targets"} }
  let wanted = ($workloads | split row ",")
  let wls = (workloads $nixpkgs | where name in $wanted)
  let cfg_names = (if $configs == "" { [] } else { $configs | split row "," })
  mkdir $out
  let stamp = (date now | format date "%Y%m%d-%H%M%S")
  let hello_seed = $"(root)/hello-seed/store"

  # drv paths for the nix baseline; the same expressions ietsp evaluates
  let drvs = ($wls | reduce -f {} { |wl, acc| $acc | insert $wl.name (out-of nix-instantiate ...$wl.instantiate | chomp) })

  if $host != "" and "host" in $tnames { setup-host $host $bin }
  mut jobs = []
  if "unix" in $tnames {
    mkdir $"(root)/unix/store"
    let sock = $"(root)/unix/sock"
    $jobs = ($jobs | append (job spawn {
      ^socat $"UNIX-LISTEN:($sock),fork,unlink-early" $"EXEC:(root)/bin/unix-stdio"
    }))
    sleep 300ms
  }

  mut results = []
  let code = (try {
    for t in $ts {
      for wl in $wls {
        if $t.kind == host and $wl.class == hello { continue }
        if $wl.class == hello and not ($hello_seed | path exists) {
          print $"skip hello: no seed store at ($hello_seed)"
          continue
        }
        # on a host every workload is an upload: --no-build, upload configs
        let wl = if $t.kind == host and $wl.build {
          $wl | merge {build: false, class: upload, args: ($wl.args | prepend "--no-build")}
        } else { $wl }
        let cfgs = (configs | where { |c| $wl.class in $c.classes } | where { |c| ($cfg_names | is-empty) or ($c.name in $cfg_names) })
        for cfg in $cfgs {
          for run in 1..$runs {
            wipe-store $t $wl.seed?
            print -n $"($t.name) ($wl.name) ($cfg.name) run ($run): cold "
            let cold = (if $cfg.flags == null { run-nix $t $wl ($drvs | get $wl.name) } else { run-ietsp $t $wl $cfg $bin })
            print -n $"(fmt-s $cold.total)s, warm "
            let warm = (if $cfg.flags == null { run-nix $t $wl ($drvs | get $wl.name) } else { run-ietsp $t $wl $cfg $bin })
            print $"(fmt-s $warm.total)s"
            let base = {target: $t.name, label: $t.label, workload: $wl.name, config: $cfg.name, run: $run}
            $results = ($results | append ($base | merge {state: cold} | merge $cold))
            $results = ($results | append ($base | merge {state: warm} | merge $warm))
            $results | to json | save -f $"($out)/($stamp).json"
          }
        }
      }
    }
    0
  } catch { |e| print -e $e.rendered; 1 })

  for j in $jobs { job kill $j }
  if $host != "" and "host" in $tnames { ^ssh $host "rm -rf /tmp/ietsp-sched" }
  if not ($results | is-empty) {
    let md = (report $results)
    $md | save -f $"($out)/($stamp).md"
    print $md
    print $"results: ($out)/($stamp).json and .md"
  }
  exit $code
}

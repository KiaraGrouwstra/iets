#!/usr/bin/env nu
# `ietsp diff` on two small derivations that differ in an env var, in an
# input derivation and in an input source (docs/diff.md): the text, the
# JSON, the exit codes and the depth limit.
use lib.nu [chomp fail ietsp-bin isolate run-in verdict]

# The two sides: a.GREETING/src/dep differ from b's, c is a again.
def write-demo [work: string] {
  mkdir $"($work)/data"
  "one\n" | save -f $"($work)/data/one.txt"
  "two\n" | save -f $"($work)/data/two.txt"
  r#'let
  dep = msg: derivation {
    name = "diffdep";
    system = "x86_64-linux";
    builder = "/bin/sh";
    args = [ "-c" "echo ${msg} > $out" ];
  };
  top = { msg, greeting, src }: derivation {
    name = "diffdemo";
    system = "x86_64-linux";
    builder = "/bin/sh";
    args = [ "-c" "cat ${dep msg} ${src} > $out" ];
    GREETING = greeting;
  };
in {
  a = top { msg = "one"; greeting = "hello"; src = ./data/one.txt; };
  b = top { msg = "two"; greeting = "goodbye"; src = ./data/two.txt; };
  c = top { msg = "one"; greeting = "hello"; src = ./data/one.txt; };
}
'# | save -f $"($work)/demo.nix"
}

def --wrapped diff-run [ietsp: string, ...args: string]: nothing -> record {
  let r = (^$ietsp diff ...$args | complete)
  {rc: $r.exit_code, out: ($r.stdout | chomp), err: ($r.stderr | chomp)}
}

def run-tests [ietsp: string, work: string]: nothing -> int {
  write-demo $work
  let f = $"($work)/demo.nix"
  mut res = []

  let r = (diff-run $ietsp -A a -A c $f)
  $res ++= [(verdict ($r.rc == 0 and ($r.out | str contains "are the same derivation")) "equal derivations exit 0" $"equal exits 0 \(rc=($r.rc), ($r.out)\)")]

  let r = (diff-run $ietsp -A a -A b $f)
  let want = [
    'env GREETING:'
    '- hello'
    '+ goodbye'
    'input source removed:'
    'input source added:'
    'input drv changed: diffdep'
    '"echo one > $out"'
  ]
  let missing = ($want | where {|w| not ($r.out | str contains $w) })
  $res ++= [(verdict ($r.rc == 1 and ($missing | is-empty)) "the text names the env, the source and the input drv" $"text misses ($missing) \(rc=($r.rc)\)\n($r.out)")]

  let r = (diff-run $ietsp -A a -A b --json $f)
  let j = (try { $r.out | from json } catch { {} })
  let ch = ($j.changes? | default [])
  let srcs = ($ch | where what == "input source")
  let inner = ($j.inputDrvs? | default [] | get -o 0 | get -o changes | default [])
  let ok = ($r.rc == 1
    and ($j.equal? | default true) == false
    and ($ch | any {|c| $c.what == "env" and $c.key == "GREETING" and $c.b == "goodbye" })
    and ($srcs | where b == null | length) == 1
    and ($srcs | where a == null | length) == 1
    and ($j.inputDrvs? | default [] | length) == 1
    and ($inner | any {|c| $c.what == "args" and ($c.b | str contains "echo two") }))
  $res ++= [(verdict $ok "the json object names the same differences" $"json wrong: ($r.out)")]

  let r = (diff-run $ietsp -A a -A b --depth 0 $f)
  $res ++= [(verdict ($r.rc == 1 and ($r.out | str contains "stopped: depth")) "--depth 0 stops at the input derivation" $"depth limit \(rc=($r.rc)\)\n($r.out)")]

  let r = (diff-run $ietsp -A a $f)
  $res ++= [(verdict ($r.rc == 2 and ($r.err | str contains "error:")) "one side is a usage error (exit 2)" $"one side \(rc=($r.rc)\): ($r.err)")]

  let r = (diff-run $ietsp -A a -A b --bogus $f)
  $res ++= [(verdict ($r.rc == 2 and ($r.err | str contains "unknown flag")) "an unknown flag is a usage error (exit 2)" $"unknown flag \(rc=($r.rc)\): ($r.err)")]

  # the other grammar: two installables, one evaluation each
  $"\(import ./demo.nix\).a\n" | save -f $"($work)/a.nix"
  $"\(import ./demo.nix\).b\n" | save -f $"($work)/b.nix"
  let r = (diff-run $ietsp $"($work)/a.nix" $"($work)/b.nix")
  $res ++= [(verdict ($r.rc == 1 and ($r.out | str contains "input drv changed: diffdep")) "two installables, one evaluation each" $"two installables \(rc=($r.rc)\)\n($r.out)")]

  if ($res | all {|x| $x }) { 0 } else { 1 }
}

def main [] {
  let ietsp = (ietsp-bin)
  let work = (isolate "diff-e2e")
  run-in $work { run-tests $ietsp $work }
}

# Helpers shared by the scripts in this directory.

# Strip trailing newlines, like bash's `$(...)`.
export def chomp []: string -> string { str trim -r -c "\n" }

# Print a list of lines; nothing for an empty list.
export def show []: list<string> -> nothing { each {|l| print $l } | ignore }

# The first line of NAME.flags split on whitespace, or [].
export def flags-of [n: string]: nothing -> list<string> {
  let f = $"($n).flags"
  if not ($f | path exists) { return [] }
  open --raw $f | lines | get -o 0 | default "" | str trim | split row -r '\s+' | where $it != ""
}

# `diff A B | head -N`. The `let` keeps diff's exit code 1 from raising.
export def diff-head [a: string, b: string, n: int]: nothing -> list<string> {
  let d = (^diff $a $b | lines | first $n)
  $d
}

# Exit code of CMD as bash reports it: 128+N when signal N killed it.
export def --wrapped exit-code [...cmd: string]: nothing -> int {
  let r = (try { ^($cmd | first) ...($cmd | skip 1) | complete } catch {|e| {exit_code: ($e.exit_code? | default 128)} })
  if $r.exit_code < 0 { 128 - $r.exit_code } else { $r.exit_code }
}

# $NAME split on spaces, so NIX="nix eval" style values keep working.
export def env-cmd [name: string, default: string]: nothing -> list<string> {
  $env | get -o $name | default $default | split row " " | where $it != ""
}

# Files matching PAT in bash glob order (strcoll of the current locale).
export def glob-sorted [pat: string]: nothing -> list<string> {
  let names = (ls ($pat | into glob) | get name | str join "\n" | ^sort | lines)
  $names
}

# Print "ok   NAME" or "FAIL DETAIL"; returns OK.
export def verdict [ok: bool, name: string, detail: string]: nothing -> bool {
  if $ok { print $"ok   ($name)" } else { print $"FAIL ($detail)" }
  $ok
}

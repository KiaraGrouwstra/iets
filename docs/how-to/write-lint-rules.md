# Write your own lint rules

You want a rule that no `with` appears in `pkgs/`, or that every `let` has a
binding. iets leaves style to you
([What iets lints](../explanation/lint-scope.md) says why) and gives you the
syntax tree. A rule is a small program that reads it.

The examples use `jq`, but any language that reads JSON works.

## Find the nodes you care about

`iets parse --json FILE...` prints the syntax tree of each file as one line
of JSON. Every node has a kind and a byte span in the file
([The syntax tree as JSON](../reference/syntax-tree.md) lists the fields).

Take this `rule.nix`:

```nix
pkgs: x:
 with pkgs; let in [ x y z ]
```

To find every `with`:

```
$ iets parse --json rule.nix |
    jq -r '.file as $f | .root | .. | objects | select(.kind == "with")
           | "\($f): with at bytes \(.start) to \(.end)"'
rule.nix: with at bytes 10 to 37
```

To find every `let` that has no bindings:

```
$ iets parse --json rule.nix |
    jq -c '.file as $f | .root | .. | objects
           | select(.kind == "let" and (.binds | length) == 0)
           | {file: $f, start, end}'
{"file":"rule.nix","start":21,"end":37}
```

## Use different rules for different directories

Your program chooses the files, so run one set of rules over
`pkgs/by-name` and another over `nixos/modules`.

## Fix what you find

A span is a pair of byte offsets, and `end` is one past the last byte. To
fix a finding, replace bytes `start` through `end - 1` with the new text.

The empty `let` above covers bytes 21 to 36. This replaces it with its body:

```sh
{ head -c 21 rule.nix; printf '[ x y z ]'; tail -c +38 rule.nix; }
```

```
pkgs: x:
 with pkgs; [ x y z ]
```

If you change more than one span in a file, start with the one that has the
largest `start`. The offsets of the spans before it stay correct.

## Read iets's own findings the same way

iets's own findings use the same fields. Run a command with
`--log-format internal-json` and each finding is a record with the byte span
of its site (`start`, `end`) and, when the lint has a fix, the text that fixes
it (`replacement`). "A finding and its edit" in
[Errors, exit codes and log records](../reference/errors.md) describes it,
and the `hash-fixed` record from
[Fix a wrong fixed-output hash](fix-hashes.md) has the same keys. iets
doesn't apply the edit. Your program decides.

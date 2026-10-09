# How to write your own lint rules

iets has no rules about how code looks
([explanation/lint-scope.md](../explanation/lint-scope.md) says why). But it
gives you the data, so you can write those rules yourself. A rule is a small
program that reads the syntax tree.

## Find the nodes you care about

`iets parse --json FILE...` prints each file's syntax tree as one line of
JSON. Every node has its kind and its byte span in the file
([reference/syntax-tree.md](../reference/syntax-tree.md)).

Take this `rule.nix`:

```nix
pkgs: x:
 with pkgs; let in [ x y z ]
```

This finds every `with`:

```
$ iets parse --json rule.nix |
    jq -r '.file as $f | .root | .. | objects | select(.kind == "with")
           | "\($f): with at bytes \(.start) to \(.end)"'
rule.nix: with at bytes 10 to 37
```

And this finds every `let` with no bindings:

```
$ iets parse --json rule.nix |
    jq -c '.file as $f | .root | .. | objects
           | select(.kind == "let" and (.binds | length) == 0)
           | {file: $f, start, end}'
{"file":"rule.nix","start":21,"end":37}
```

## Use different rules for different directories

Your program picks the files, so iets does not need to know about your
layout. For example, you can have one set of rules for `pkgs/by-name` and
another for `nixos/modules`.

## Fix what you find

The spans are byte offsets, and `end` is one past the last byte. To fix a
finding, replace bytes `start` to `end - 1` with the new text.

When you change more than one span in a file, start with the largest
`start`. That way the other offsets stay correct.

## Read iets's own findings the same way

iets's own findings have the same shape. Run with `--log-format
internal-json`, and each finding is a record with its site's byte span
(`start`, `end`). When the lint has a fix, the record also has the text
that fixes it (`replacement`). See "A finding and its edit" in
[reference/errors.md](../reference/errors.md). The `hash-fixed` record of
[fix-hashes](fix-hashes.md) uses the same keys.

iets does not apply the edit itself. Your program decides.

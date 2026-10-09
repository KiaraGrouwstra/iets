# What iets lints, and what it does not

Why some lints are in iets and others never will be, and how you can write
your own rules with the data iets gives you.

## The rule

> iets reports what only an evaluator can know. It has no opinion about
> how code looks.

A lint belongs in the registry when both of these hold:

1. It needs evaluation to decide. Its stage in `iets lint list` is
   `evaluator`.
2. It tells you about what the code does (correctness, performance or
   purity), not about how the code reads.

Take `eq-type-mismatch`. `x == "a"` is fine syntax. The comparison is
always false only when `x` is a number, and only the evaluator knows the
value of `x`. Or take `git-in-source`: only the evaluator knows which
directory a coercion copies to the store.

The full list is in `docs/reference/lints.md`.

## Why

Style is the project's call, not the tool's. nixpkgs has its own style, and
it changes with the kind of file and the path. A lint about `with`, an
unused binding or `a = a;` asks you to change code that is correct. iets
should not do that.

Tools that read only the syntax already exist: statix and deadnix. Copying
their rules into iets makes the binary bigger and tells you nothing new.
iets wants to make the Nix stack smaller, with good abstractions and good
defaults. One big binary that does everything is the opposite.

## The lints the rule does not explain

Four lints do not need evaluation. They stay for other reasons.

| Lint | Reason |
|---|---|
| `absolute-path-literal`, `short-path-literal`, `url-literal` | Nix 2.34 has a setting for each (`lint-absolute-path-literals`, `lint-short-path-literals`, `lint-url-literals`). They are Nix's behaviour, not iets's opinion. |
| `unknown-allow` | It checks that a `# iets: allow` comment names a lint in the registry. Without it, a typo silences nothing and tells nobody. |

`impure-builtin` comes in two parts. The evaluator reports a `getEnv`
call. The compiler reports a reference to `currentTime`, because that
value sits in the table of globals and has no position of its own.

## Where a finding is reported

A lint finds a site, and the site's location tells iets whether the
finding is yours. A site in your repository is reported. A site in an
input stays quiet. `docs/reference/lints.md` ("Scope") has the rule.

Two lints break it: `tree-meta-in-drv` and `rev-count`.

`tree-meta-in-drv` breaks it on purpose. It reports source tree metadata
(`self.rev`, `self.lastModifiedDate`) that ends up in a derivation. That
value changes with every commit, so every commit rebuilds the derivation
and everything that depends on it.

Here the site's location does not say whose finding it is. Two examples
show why:

- You set `system.configurationRevision = self.rev`. nixpkgs writes the
  value into the `nixos-version` derivation. The site is in nixpkgs, but
  the value is yours, and your commits cause the rebuild.
- nixpkgs writes its own `lastModifiedDate` and `shortRev` into the NixOS
  version. That value changes only when you update the lock file, and then
  a rebuild is right.

So the tree the value comes from sets the level. The root tree gets the
lint's level, `warn` by default. A locked input stays quiet unless
`[lint.inputs.<node>]` gives it a level.

`rev-count` splits the same way, for the same reason. It reports a read of
`revCount`, which is deprecated. The root tree's count changes with every
commit, and iets has to compute it. A locked input's count is a number in
the lock file. iets knows the tree because `revCount` is a lazy value that
carries its tree's name.

A `rev-count` finding points at the selection that reads the attribute,
in your repository or in an input. The compiler marks every selection
named `revCount`, and the evaluator then looks at the value it reads. A
read that is not a selection has no site, so the finding names only the
tree. `docs/reference/lints.md` has the list ("The tree that a value comes
from").

iets knows the tree because each metadata string carries a mark. The mark
is a string context element that Nix does not have. It travels with the
string through `+`, interpolation, `substring` and `concatStringsSep`, just
like a store path reference. Nix never sees it: `getContext`,
`hasContext`, `==`, `toJSON` and the `.drv` files stay the same.

## Make your own rules

iets gives you the data. A rule is a program you write that reads it.

`iets parse --json FILE...` prints each file's syntax tree as one line of
JSON. Every node has its kind and its byte span in the file
(`docs/reference/syntax-tree.md`). This finds every `with`:

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

You can have one set of rules for `pkgs/by-name` and another for
`nixos/modules`. Your program picks the files. iets does not need to know.

The spans are byte offsets, so your program can also edit the file:
replace bytes `start` to `end - 1` with new text. When you change more than
one span in a file, start with the largest `start`. That way the other
offsets stay correct.

## Findings as data

iets's own findings have the same shape. With `--log-format
internal-json`, each finding is a record with its site's byte span
(`start`, `end`). When the lint has a fix, the record also has the text
that fixes it (`replacement`; see `docs/reference/errors.md`, "A finding
and its edit"). The `hash-fixed` record of `fix-hashes` uses the same keys.

iets does not apply the edit itself. The program that reads the record
decides.

## What iets does not have

- A language for rules.
- A plug-in loader.
- A syntax tree that keeps every byte of the file. Comments and
  parentheses are not in the tree.
- A command that applies the edits of the findings.

## A name that is not in the registry

Your `iets.toml` can name a lint that iets does not have, for example after
you update iets. iets warns once for each such name, with the code
`unknown-lint`, and carries on. `--lint` on the command line gives an
error instead. `docs/reference/lints.md` says why.

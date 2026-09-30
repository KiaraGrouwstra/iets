# What iets lints, and what it does not

The rule that decides if a lint is in iets, the reason for the rule, and
how a project makes its own rules with the data that iets gives.

## The rule

> iets reports what only an evaluator can know. It has no opinion about
> how code looks.

A lint is in the registry only if these two conditions are true:

1. The lint needs evaluation to decide. Its stage in `iets lint list` is
   `evaluator`.
2. The lint tells you something about what the code does: correctness,
   performance or purity. It does not tell you how the code reads.

`eq-type-mismatch` is an example. `x == "a"` is correct syntax. The
comparison is always false only if `x` is a number, and only the
evaluator has the value of `x`. `git-in-source` is a second example: only
the evaluator knows which directory a coercion copies to the store.

`docs/reference/lints.md` has the list of the lints.

## Why

Style is a decision of the project, not of the tool. nixpkgs has its own
style, and that style is not the same for each type of file or for each
path. A lint about `with`, about a binding that is not used, or about
`a = a;` makes a person change code that is correct. iets must not do
that.

Tools that read only the syntax are available: statix and deadnix. A
second copy of their rules in iets makes the binary larger and gives no
new data. The mission of iets is to make the Nix stack smaller with good
abstractions and good defaults. One large binary that does all things is
the opposite.

## The lints that the rule does not explain

Four lints do not need evaluation. They stay for other reasons.

| Lint | Reason |
|---|---|
| `absolute-path-literal`, `short-path-literal`, `url-literal` | Nix 2.34 has a setting for each of them (`lint-absolute-path-literals`, `lint-short-path-literals`, `lint-url-literals`). They are the behaviour of Nix, not an opinion of iets. |
| `unknown-allow` | It makes sure that a `# iets: allow` comment names a lint that is in the registry. Without it, an incorrect name silences nothing and tells nobody. |

`impure-builtin` has two parts. The evaluator reports a `getEnv` call.
The compiler reports a reference to `currentTime`, because the value is
in the table of the globals and has no position of its own.

## Where a finding is reported

A lint finds a site. The location of the site tells iets if the finding
is yours: a site in your repository is reported, and a site in an input
is silent. `docs/reference/lints.md` ("Scope") has the rule.

Two lints do not obey this rule: `tree-meta-in-drv` and `rev-count`.

`tree-meta-in-drv` does not obey this rule, and that is a decision. The
lint reports the metadata of a source tree (`self.rev`,
`self.lastModifiedDate`) that goes into a derivation. The value changes
with each commit, so each commit builds the derivation and all that
depend on it again.

For this lint, the location of the site does not tell iets if the
finding is yours. These two examples show why:

- You set `system.configurationRevision = self.rev`. nixpkgs writes the
  value into the derivation `nixos-version`. The site is in nixpkgs, but
  the value is yours, and your commits cause the rebuild.
- nixpkgs writes its own `lastModifiedDate` and `shortRev` into the
  version of NixOS. The value changes only when you update the lock
  file. A rebuild is then correct.

Thus the tree that the value comes from gives the level. The root tree
has the level of the lint, which is `warn` by default. A locked input is
silent, unless `[lint.inputs.<node>]` gives it a level.

`rev-count` has the same partition for the same reason. It reports a read
of `revCount`, which is deprecated. The count of the root tree changes
with each commit and iets must compute it. The count of a locked input
is a number in the lock file. iets knows the tree because `revCount` is
a lazy value that has the name of its tree.

The finding of `rev-count` goes to the selection that reads the
attribute, in your repository or in an input. The compiler marks each
selection that has the name `revCount`. The evaluator then looks at the
value that such a selection reads. A read that is not a selection has no
site, and the finding names only the tree. `docs/reference/lints.md` has
the list ("The tree that a value comes from").

iets knows the tree because each metadata string has a mark. The mark
is a string context element that Nix does not have. It moves with the
string through `+`, interpolation, `substring` and `concatStringsSep`, as
a store path reference does. Nix cannot see it: `getContext`,
`hasContext`, `==`, `toJSON` and the `.drv` files do not change.

## Make your own rules

iets gives the data. A rule is an external program that reads the data.

`iets parse --json FILE...` prints the syntax tree of each file as one
line of JSON. Each node has its kind and its byte span in the file
(`docs/reference/syntax-tree.md`). This command finds each `with` in a
file:

```
$ iets parse --json rule.nix |
    jq -r '.file as $f | .root | .. | objects | select(.kind == "with")
           | "\($f): with at bytes \(.start) to \(.end)"'
rule.nix: with at bytes 10 to 37
```

This command finds each `let` that has no binding:

```
$ iets parse --json rule.nix |
    jq -c '.file as $f | .root | .. | objects
           | select(.kind == "let" and (.binds | length) == 0)
           | {file: $f, start, end}'
{"file":"rule.nix","start":21,"end":37}
```

A project can have one set of rules for `pkgs/by-name` and a different
set for `nixos/modules`. The program selects the files, and iets does
not know the difference.

The spans are byte offsets, so a program can change the file. It replaces
the bytes `start` to `end - 1` with new text. If a program changes more
than one span of a file, it must start with the span that has the largest
`start`. Then the offsets of the other spans stay correct.

## Findings as data

A finding of iets has the same form. With `--log-format internal-json`,
each finding is a record with the byte span of its site (`start`, `end`).
A record has the text that corrects the finding (`replacement`) when the
lint has one (`docs/reference/errors.md`, "A finding and its edit"). The
`hash-fixed` record of `fix-hashes` has the same keys.

iets does not apply the edit of a finding. The program that reads the
record decides.

## What iets does not have

- A language for rules.
- A loader for plug-ins.
- A syntax tree that keeps each byte of the file. Comments and
  parentheses are not in the tree.
- A command that applies the edits of the findings.

## A name that is not in the registry

An `iets.toml` can name a lint that iets does not have, for example
after an update of iets. iets gives one warning for each name, with the
code `unknown-lint`, and continues. `--lint` on the command line gives an
error. `docs/reference/lints.md` has the reason.

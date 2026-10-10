# Fix a wrong fixed-output hash

You bumped a version, the `fetchurl` hash is stale, and the build fails.
`iets build` can write the right hash into your file and build again. The
`fix-hashes` setting controls this, with the values `ask` (the default),
`always` and `never` ([Settings](../reference/settings.md)).

The examples build a file that reads `<nixpkgs>`, so they need `--impure` and
a search path, either `NIX_PATH` or `-I nixpkgs=PATH`.

## Terminal

With `ask`, iets stops at each fix and asks, as long as stdin and stderr are
both terminals and the log format isn't `internal-json`:

```
fix the hash at /home/you/tour/src.nix:4:13: 'sha256-AAAA…=' to 'sha256-/Sb+…='? [y/n/a]
```

Answer `y` to take this fix, `n` to refuse it, or `a` to take this one and
every later fix in the run. Anywhere else (a pipe, a CI job, a JSON log)
`ask` acts like `never`: the build fails with the mismatch and iets changes
nothing.

## Automation

With `always`, iets rewrites the file without asking, which is what a CI job
or an agent needs. Take this `src.nix`, whose hash is wrong:

```nix
let pkgs = import <nixpkgs> { };
in pkgs.fetchurl {
  url = "https://github.com/numtide/treefmt/archive/v2.0.0.tar.gz";
  sha256 = "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
}
```

Build it with `always`:

```sh
iets --iets-option fix-hashes always build --impure --builder daemon src.nix
```

iets shows the mismatch, tells you what it changed, and finishes the build.
Here is the output without the progress and log lines:

```
error: hash mismatch in fixed-output derivation '/nix/store/7ssr…-v2.0.0.tar.gz.drv':
         specified: sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=
            got:    sha256-/Sb+qZOLHhmr1D63ANxTxkhA4uTOpn0IcUO2UmQyRuo=
fixed: /home/you/tour/src.nix:4:13: 'sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=' is now 'sha256-/Sb+qZOLHhmr1D63ANxTxkhA4uTOpn0IcUO2UmQyRuo='
re-evaluating: /home/you/tour/src.nix changed
/nix/store/ljk32s16w758r5yi91dp39icx5yraw0r-v2.0.0.tar.gz
```

Your file now has the right hash. In the JSON log, each fix is also a
`hash-fixed` record with the byte span of the old hash and the new text
([Errors, exit codes and log records](../reference/errors.md)).

To make `always` the default, put this in `~/.config/iets/iets.toml`:

```
fix-hashes = "always"
```

## Turning it off

With `never`, the build reports the mismatch and fails. No file is edited:

```sh
iets --iets-option fix-hashes never build --impure --builder daemon src.nix
```

## Finding the edit

After a mismatch, iets evaluates once more without the tracing cache, to list
the files the evaluation read, and looks for the old hash in them in any
spelling: SRI, base32 or base16.

A derivation with inputs works too. The daemon builds a resolved copy of it
and reports the mismatch as a line in the build log, which iets reads when
the log names a derivation with the same name. `__structuredAttrs` works
too.

## Limits

**The hash isn't in exactly one place.** If you wrote the old hash out
twice, iets can't tell which one to change. It says so and edits nothing:

```
warning: fix-hashes: 'sha256-BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBA=' is in 2 places in the files evaluation read; set it to
'sha256-Kw6NHsC8+M7737QcmPwyXSdONageyi4+3cssDXbKKwY=' by hand
```

Zero places gets the same answer. That happens when the hash isn't written
out in a file this evaluation read: it was computed, or it came from JSON.

**The build isn't reproducible.** If a hash that iets wrote mismatches again,
iets stops, because no hash is the right one.

**The hash is in a store path or a data file.** iets rewrites only the hash
literal in your files. It doesn't edit a store path or search a file that the
evaluation read with `readFile` or `importJSON`.

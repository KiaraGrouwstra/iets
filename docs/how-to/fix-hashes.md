# How to fix a wrong fixed-output hash

When a fixed-output derivation has the wrong hash, the build fails. `iets
build` can put the right hash into your file and build again. The setting
is `fix-hashes`, with three values: `ask` (the default), `always` and
`never`.

The setting is defined in [reference/settings.md](../reference/settings.md).

## In a terminal: `ask`

`ask` is the default. When stdin and stderr are both terminals, and the log
format is not `internal-json`, iets asks you about each fix:

```
[y/n/a]
```

`y` takes this fix, `n` refuses it, and `a` takes this one and every other
fix in the run. Anywhere else (a pipe, a CI job, a JSON log), `ask` only
prints the hint and changes nothing.

## In CI or for an agent: `always`

`always` rewrites without asking:

```sh
iets --iets-option fix-hashes always build --builder daemon src.nix
```

Or put it in `~/.config/iets/iets.toml` once:

```
fix-hashes = "always"
```

Given this file:

```nix
let pkgs = import <nixpkgs> { };
in pkgs.fetchurl {
  url = "https://github.com/numtide/treefmt/archive/v2.0.0.tar.gz";
  sha256 = "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
}
```

iets shows the mismatch, tells you what it changed, and finishes the build:

```
error: hash mismatch in fixed-output derivation '/nix/store/7ssr…-v2.0.0.tar.gz.drv':
         specified: sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=
            got:    sha256-/Sb+qZOLHhmr1D63ANxTxkhA4uTOpn0IcUO2UmQyRuo=
fixed: /home/you/tour/src.nix:4:13: 'sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=' is now 'sha256-/Sb+qZOLHhmr1D63ANxTxkhA4uTOpn0IcUO2UmQyRuo='
/nix/store/ljk32s16w758r5yi91dp39icx5yraw0r-v2.0.0.tar.gz
```

Your file now has the right hash. In the JSON log, each fix is also a
`hash-fixed` record with the byte span of the old hash and the new text
(see [reference/errors.md](../reference/errors.md)).

## To turn it off: `never`

```sh
iets --iets-option fix-hashes never build --builder daemon src.nix
```

The mismatch is reported and nothing is edited.

## How it finds the place to edit

After a mismatch, iets evaluates once more without the tracing cache, to
list the files the evaluation read. Then it looks for the old hash in them.
Any spelling of the hash works: SRI, base32 and base16.

A derivation with inputs works too. The daemon builds it as a resolved copy
of itself, and reports the mismatch as a line in the build log instead of
as a result. iets reads it from there when the log names a derivation with
the same name.

`__structuredAttrs` works as well. Nix fills in the output paths when it
builds, so nothing in the JSON goes stale.

## What it will not do

**It will not edit when the hash is not in exactly one place.** If the old
hash appears twice (say, bound to a variable and repeated elsewhere), iets
tells you and leaves your files alone:

```
warning: fix-hashes: 'sha256-BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBA=' is in
2 places in the files evaluation read, not one; set it to
'sha256-Kw6NHsC8+M7737QcmPwyXSdONageyi4+3cssDXbKKwY=' by hand
```

The same goes for zero places. That happens when the hash is not written
out in a file this evaluation read: it was computed, or it came from JSON.

**It will not chase a build that is not reproducible.** If a hash iets
wrote mismatches again on the next build, it stops. A second mismatch means
the output is not deterministic, and no hash is the right one.

**It never edits a store path.** Only the hash literal is rewritten.

**It does not search data files.** A hash that evaluation obtained through
`readFile` or `importJSON` is not searched for.

# How to fix a wrong fixed-output hash

A fixed-output derivation whose hash is wrong fails the build. `ietsp
build` can rewrite that hash in the file it came from and build again. The
setting is `fix-hashes`, and it has three values: `ask` (the default),
`always` and `never`.

The setting is defined in [reference/settings.md](../reference/settings.md).

## In a terminal: `ask`

`ask` is the default. When stdin and stderr are both terminals, and the log
format is not `internal-json`, each fix is offered:

```
[y/n/a]
```

`y` takes this fix, `n` refuses it, and `a` takes this fix and every
remaining one in the run. Anywhere else — a pipe, a CI job, a JSON log —
`ask` only prints the hint and changes nothing.

## In CI or for an agent: `always`

`always` rewrites without asking:

```sh
ietsp --ietsp-option fix-hashes always build --builder daemon src.nix
```

Or put it in `ietsp.conf` once:

```
fix-hashes = always
```

Given this file:

```nix
let pkgs = import <nixpkgs> { };
in pkgs.fetchurl {
  url = "https://github.com/numtide/treefmt/archive/v2.0.0.tar.gz";
  sha256 = "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
}
```

the run reports the daemon's mismatch, names the edit, and finishes the
build:

```
error: hash mismatch in fixed-output derivation '/nix/store/7ssr…-v2.0.0.tar.gz.drv':
         specified: sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=
            got:    sha256-/Sb+qZOLHhmr1D63ANxTxkhA4uTOpn0IcUO2UmQyRuo=
fixed: /home/you/tour/src.nix:4:13: 'sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=' is now 'sha256-/Sb+qZOLHhmr1D63ANxTxkhA4uTOpn0IcUO2UmQyRuo='
/nix/store/ljk32s16w758r5yi91dp39icx5yraw0r-v2.0.0.tar.gz
```

The file now holds the right hash. Each fix is also a `hash-fixed` record
in the JSON log; see [reference/errors.md](../reference/errors.md).

## To turn it off: `never`

```sh
ietsp --ietsp-option fix-hashes never build --builder daemon src.nix
```

The mismatch is reported and nothing is edited.

## How it finds the place to edit

After a mismatch, ietsp runs one more evaluation without the tracing cache
to list the files that evaluation read, then looks for the old hash in
them. It matches any spelling of the hash, so SRI, base32 and base16 all
work.

It also handles the case where the derivation has inputs. Such a derivation
is built as a resolved copy of itself, and the daemon reports the mismatch
as a line in the build log instead of as a result. ietsp reads it from
there when the log names a derivation of the same name.

`__structuredAttrs` is fine: Nix fills the output paths in when it builds,
so nothing in the JSON goes stale.

## What it will not do

**It will not edit when the hash is not in exactly one place.** If the old
hash appears twice — bound to a variable and repeated elsewhere, say — the
run says so and leaves your files alone:

```
warning: fix-hashes: 'sha256-BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBA=' is in
2 places in the files evaluation read, not one; set it to
'sha256-Kw6NHsC8+M7737QcmPwyXSdONageyi4+3cssDXbKKwY=' by hand
```

Zero places gets the same treatment. That happens when the hash is not
written literally in a file this evaluation read — computed, or fetched
from JSON.

**It will not chase a build that is not reproducible.** If a hash ietsp
wrote mismatches again on the next build, it stops. A second mismatch means
the output is not deterministic, and no hash is the right one.

**It never edits a store path.** Only the hash literal is rewritten.

**It does not search data files.** A hash that evaluation obtained through
`readFile` or `importJSON` is not searched for.

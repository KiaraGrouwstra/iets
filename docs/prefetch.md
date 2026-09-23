# `ietsp prefetch`

The hash of something that is not fetched yet, on stdout, in one line:

```
$ ietsp prefetch https://github.com/numtide/treefmt/archive/v2.0.0.tar.gz
sha256-/Sb+qZOLHhmr1D63ANxTxkhA4uTOpn0IcUO2UmQyRuo=

$ ietsp prefetch --unpack https://github.com/numtide/treefmt/archive/v2.0.0.tar.gz
sha256-Ckvpb2at7lg7AB0XNtIxeo8lWXX+04MVHHKoUs876dg=

$ ietsp prefetch -E '(import <nixpkgs> {}).hello.src'
sha256-DV9gFUOC/uELEUocNOeF2LH0kgc64tOm97FHaHs2aqA=
```

It replaces `nurl`, `nix-prefetch` and `nix-prefetch-url` for the common
cases: no file is edited to learn a hash, and `$(ietsp prefetch …)` is the
hash and nothing else.

## The two forms

`ietsp prefetch URL` downloads the URL and hashes it. A positional that
starts with `https://`, `http://` or `file://` is a URL; anything else is an
installable and takes the other form.

- The flat hash of the bytes the URL serves, as `nix store prefetch-file`
  hashes them: nothing is decompressed.
- `--unpack`: the NAR hash of the unpacked tarball, which is what
  `fetchTarball`, `fetchzip` and `fetchFromGitHub` want. The archive may be
  compressed with gzip, xz, zstd or bzip2. Zip files are not supported.
- `--hash-type ALGO` picks `md5`, `sha1`, `sha256` (the default) or `sha512`.

`ietsp prefetch -E EXPR`, `ietsp prefetch FILE -A ATTR` and `ietsp prefetch
FLAKEREF#ATTR` evaluate to one fixed-output derivation and build it, with
every flag of `ietsp eval` for the evaluation. So the hash of anything a
`fetch*` function makes is one command away, whatever it fetches from:

```
$ ietsp prefetch -E 'with import <nixpkgs> {}; fetchzip {
    url = "https://github.com/numtide/treefmt/archive/v2.0.0.tar.gz";
    hash = ""; }'
sha256-Ckvpb2at7lg7AB0XNtIxeo8lWXX+04MVHHKoUs876dg=

$ ietsp prefetch default.nix -A mypkg.cargoDeps   # a cargoHash / vendorHash
```

The hash the expression names is ignored, so `hash = ""`, `lib.fakeHash` and
last month's hash all work. There is no `-f FILE`: the file is a positional,
as it is for `ietsp eval` and `ietsp build`.

## How the expression form works

ietsp rewrites the derivation instead of the file that defines it. The
evaluated `.drv` gets its fixed output hash replaced by the all-zero hash of
the same algorithm, its output path recomputed from that hash, and its own
path recomputed from the ATerm. Nothing else moves: the `outputHash` the
builder itself may read keeps the value the expression gave it, since the
daemon checks the output against the derivation, not against that variable.
The rewritten derivation is built on the `builder` setting
(`docs/config.md`) like any other, the daemon reports

```
hash mismatch in fixed-output derivation '…':
  specified: sha256-AAAA…
     got:    sha256-GzabkpW8…
```

and the `got` hash is the answer. The rest of the closure is untouched, the
fetcher runs exactly as it would have, and the result is the hash the real
build will check. The mismatch is expected, so it is never reported as an
error, on stderr or in the `internal-json` stream.

A derivation with input derivations is built as a resolved copy of itself,
and the daemon then reports the mismatch in the build log instead of in the
result. ietsp reads it from there when the log names a derivation of the
same name. `__structuredAttrs` is fine: Nix fills the output paths in when
it builds, so nothing in the JSON goes stale.

## Output

Without `--json`, the SRI hash, one line on stdout, and nothing else on
stdout. With `--json`, one object:

| Form | Object |
| --- | --- |
| URL | `{"hash": "sha256-…", "url": "https://…"}` |
| expression | `{"hash": "sha256-…", "drvPath": "/nix/store/….drv"}` |

`drvPath` is the rewritten derivation, the one that was built; the one the
expression evaluated to has the hash the file still names.

Exit codes: 0 with a hash, 1 when the download or the build failed, 2 for a
bad command line (docs/cli.md).

## What differs from Nix

`nix store prefetch-file` and `nix-prefetch-url` add the file to the store
and print its path. ietsp is a client with no store of its own: it prints
hashes and writes nothing, which is why it also has no `--name`,
`--executable` or `--expected-hash`, and why the classic names keep falling
back to the real nix (`docs/classic.md`). The hash itself is the one Nix
prints: for the URL form it is the `hash` field of `nix store prefetch-file
--json`, in SRI, and for the expression form it is the `got` hash of the
build.

Two more differences of `--unpack`:

- A tarball whose single member is a file: Nix hashes that file, ietsp hashes
  the directory that holds it. A single directory is dereferenced by both.
- The tree is the one `docs/flakes.md` describes, built from the archive with
  git-tree rules: directories, symlinks, and files with one bit of mode.
  That is the tree `fetchTarball` hashes.

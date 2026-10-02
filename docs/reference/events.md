# Events

`IETS_EVENTS=PATH` makes a run write one JSON object per line to `PATH`; `-`
is stderr, and a unix socket is connected to. The stream says what an
evaluation did. The `IETS_STATS` counters say how much of it
(`docs/reference/settings.md`).

A reader matches on the `event` field and reads the fields it knows. A new
field, and a new event name, are not changes a reader must follow. A field
that is there keeps its meaning.

Nothing is written when the variable is unset, and no hook builds a record,
so an unwatched run pays nothing.

## `store-add`

One store path the evaluation made from a source: the `addToStore` and
`addTextToStore` calls Nix makes. iets writes no store, so the path names
content that only an upload or a build puts there
(`docs/explanation/evaluation.md`).

| Field | Meaning |
|---|---|
| `path` | the store path |
| `kind` | `tree` for the NAR of a tree, `flat` for one file added as itself, `text` for `builtins.toFile` |
| `size` | NAR bytes for `tree`, the file's or the text's own bytes for `flat` and `text` |
| `from` | the local tree or input store entry the bytes came from, with `(filtered)` when a filter keeps part of it out of the copy; `text` for a `toFile` |
| `at` | the expression that made it, as `file:line:column` |

One line per call, in the order the calls finished. A path appears once per
call: two derivations that name the same patch add it twice, as they do
under Nix. A locked flake input is already in the store and is no add.

The `store_adds` counter in the `IETS_STATS` line counts the same calls, but
a worker that has not folded its last task is missing from it. The stream is
the exact record.

```
$ IETS_EVENTS=- iets eval --no-cache --raw -f ./nixpkgs -A hello.drvPath
{"at":"/tmp/nixpkgs/pkgs/stdenv/generic/make-derivation.nix:693:11","event":"store-add","from":"/tmp/nixpkgs/pkgs/build-support/fetchurl/builder.sh","kind":"tree","path":"/nix/store/d0h3b1fj9hahy3lvh2kpny75w47la15s-builder.sh","size":5288}
```

Totals, biggest first:

```
$ IETS_EVENTS=/tmp/e.json iets eval --no-cache --raw -f ./nixpkgs -A hello.drvPath
$ jq -s 'map(.size) | add' /tmp/e.json
$ jq -s -r 'sort_by(-.size)[:5][] | "\(.size)\t\(.from)"' /tmp/e.json
```

# Events

`IETS_EVENTS` names the parts of a run to watch, and `IETS_EVENTS_TO` says
where the records go: a file, a unix socket, or `-` for stderr, which is the
default. Each record is one JSON object on one line. The stream says what an
evaluation did; the `IETS_STATS` counters say how much of it
(`docs/reference/settings.md`); `IETS_PROFILE=eval` says where the time
went (`docs/reference/profile.md`).

```
IETS_EVENTS=store                       # one group, to stderr
IETS_EVENTS=store,fetch                 # two
IETS_EVENTS=all IETS_EVENTS_TO=/tmp/e.jsonl
```

A reader matches on the `event` field and reads the fields it knows. A new
field, and a new event name, are not changes a reader must follow. A field
that is there keeps its meaning. The order the fields appear in is not part
of the promise, and `event` is first only as a courtesy to `grep`.

Nothing is written when no group is named, and no hook builds a record, so
an unwatched run pays nothing. A watched one pays about 0.4% of its
instructions, and one line is one `write`: 337 events of a `hello`
evaluation cost 0.42%.

## The groups

A group is the unit that is turned on, and its class is what it costs. A
cold group may be on in any run, and `all` takes every one of them. A warm
group runs often enough to be worth naming, so `all` leaves it out and it
is asked for by name.

A group that is not yet instrumented is still a name this binary accepts,
and it writes nothing. The **Events** column is what exists today.

| Group | Covers | Class | Events |
|:---|:---|:---|:---|
| `syntax` | a file parsed | cold | — |
| `ir` | compiled, or loaded from the IR cache | cold | — |
| `import` | a file imported, and the cache hit that answered it | cold | — |
| `store` | a path added, a `.drv` written | cold | `store-add` |
| `drv` | a derivation instantiated | warm | — |
| `fetch` | an input fetched | cold | — |
| `flake` | a reference resolved, a lock node used | cold | — |
| `cache` | the tracing cache and the input store: hit, miss, write | cold | — |
| `io` | `readFile`, `readDir`, `pathExists`, `hashFile` | warm | — |
| `build` | plan, missing, build, upload | cold | — |
| `lint` | each warning | cold | — |
| `error` | each error raised | cold | — |

A name that is not a group is an error, not a silence: a mistyped group
would otherwise watch nothing and look like a quiet run.

```
$ IETS_EVENTS=stor iets eval -f . -A x
error: IETS_EVENTS: 'stor' is not an event group (all, syntax, ir, import, …)
```

Evaluation itself has no group. A `hello` evaluation makes 48 million
calls, and nobody reads a line for each. The `IETS_STATS` counters count
that work, and `IETS_PROFILE=eval` samples where its time went
(`docs/reference/profile.md`).

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
{"event":"store-add","path":"/nix/store/d0h3b1fj9hahy3lvh2kpny75w47la15s-builder.sh","kind":"tree","size":5288,"from":"/tmp/nixpkgs/pkgs/build-support/fetchurl/builder.sh","at":"/tmp/nixpkgs/pkgs/stdenv/generic/make-derivation.nix:693:11"}
```

Totals, biggest first:

```
$ IETS_EVENTS=/tmp/e.json iets eval --no-cache --raw -f ./nixpkgs -A hello.drvPath
$ jq -s 'map(.size) | add' /tmp/e.json
$ jq -s -r 'sort_by(-.size)[:5][] | "\(.size)\t\(.from)"' /tmp/e.json
```

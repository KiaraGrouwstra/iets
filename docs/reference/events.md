# Events

Set `IETS_EVENTS` to watch parts of a run. Each thing that happens becomes
one JSON object on one line:

```
$ IETS_EVENTS=store iets eval --no-cache --expr 'builtins.toFile "x" "y"'
{"event":"store-add","path":"/nix/store/lfngsssysp6h1v4ccqg23c52s9sjl779-x","kind":"text","size":1,"from":"text","at":"«string»:1:1"}
"/nix/store/lfngsssysp6h1v4ccqg23c52s9sjl779-x"
```

`IETS_EVENTS` takes one or more groups. `IETS_EVENTS_TO` says where the
records go: a file, a unix socket, or `-` for stderr, which is the default.

```
IETS_EVENTS=store                       # one group, to stderr
IETS_EVENTS=store,fetch                 # two
IETS_EVENTS=all IETS_EVENTS_TO=/tmp/e.jsonl
```

Events tell you what an evaluation did. The `IETS_STATS` counters tell you
how much of it (`docs/reference/settings.md`), and `IETS_PROFILE=eval`
tells you where the time went (`docs/reference/profile.md`).

Match on the `event` field and read the fields you know. A new field or a
new event name may appear at any time, so ignore what you do not know. A
field that is there keeps its meaning. The order of the fields is not
fixed; `event` comes first only to help `grep`.

When you name no group, nothing is written and no record is built, so the
run costs nothing extra. A watched run costs about 0.4% of its
instructions, and each line is one `write`: the 337 events of a `hello`
evaluation cost 0.42%.

## The groups

You turn events on one group at a time. A cold group is cheap, and `all`
turns on every cold group. A warm group fires often, so `all` leaves it out
and you must name it.

Every group below is a name you can use today. Some groups have no events
yet and write nothing. The **Events** column shows what each one writes.

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

A name that is not a group is an error. Otherwise a typo would watch
nothing and look like a quiet run:

```
$ IETS_EVENTS=stor iets eval -f . -A x
error: IETS_EVENTS: 'stor' is not an event group (all, syntax, ir, import, …)
```

Evaluation itself has no group. A `hello` evaluation makes 48 million
calls, and nobody wants a line for each. Use the `IETS_STATS` counters to
count that work, and `IETS_PROFILE=eval` to see where its time went
(`docs/reference/profile.md`).

## `store-add`

One store path that the evaluation made from a source. These are the
`addToStore` and `addTextToStore` calls of Nix. iets writes nothing to the
store, so the path names content that only an upload or a build puts there
(`docs/explanation/evaluation.md`).

| Field | Meaning |
|---|---|
| `path` | the store path |
| `kind` | `tree` for the NAR of a tree, `flat` for one file added as itself, `text` for `builtins.toFile` |
| `size` | NAR bytes for `tree`, the file's or the text's own bytes for `flat` and `text` |
| `from` | the local tree or input store entry the bytes came from, with `(filtered)` when a filter keeps part of it out of the copy; `text` for a `toFile` |
| `at` | the expression that made it, as `file:line:column` |

You get one line per call, in the order the calls finished. A path shows
up once per call: two derivations that name the same patch add it twice,
as they do in Nix. A locked flake input is already in the store, so it is
not an add.

The `store_adds` counter of `IETS_STATS` counts the same calls, but it can
miss the last task of a worker. The events are the exact record.

```
$ IETS_EVENTS=store iets eval --no-cache --raw -f ./nixpkgs -A hello.drvPath
{"event":"store-add","path":"/nix/store/d0h3b1fj9hahy3lvh2kpny75w47la15s-builder.sh","kind":"tree","size":5288,"from":"/tmp/nixpkgs/pkgs/build-support/fetchurl/builder.sh","at":"/tmp/nixpkgs/pkgs/stdenv/generic/make-derivation.nix:693:11"}
```

To get the total size, and the five biggest adds:

```
$ IETS_EVENTS=store IETS_EVENTS_TO=/tmp/e.json iets eval --no-cache --raw -f ./nixpkgs -A hello.drvPath
$ jq -s 'map(.size) | add' /tmp/e.json
$ jq -s -r 'sort_by(-.size)[:5][] | "\(.size)\t\(.from)"' /tmp/e.json
```

# A proposal: one event vocabulary for the whole evaluator

`IETS_EVENTS` answers one question today: which paths did an evaluation add
to the store. Every other question about a run has its own knob, its own
format and its own parser. This says how to instrument every part of the
language without that happening again, and without the hot paths paying for
it.

Status: a proposal. Nothing here is built except the `store-add` event.

## What exists today

| Surface | What it gives | Shape |
|:---|:---|:---|
| `IETS_STATS=1` | counters at the end: files, thunks, calls, drvs, store adds, arena bytes per object kind | text, one line plus a table |
| `IETS_STATS=2` | the memory census: histograms, duplicates, objects never read | text; hooks need `--features stats2` |
| `IETS_PAR_STATS=1` | the scheduler summary | text |
| `IETS_PAR_STATS=2` | a 20 ms sample of running workers, queued tasks and where main is | text, one line per sample |
| `IETS_IR_STATS` | one line per compiled file | text |
| `--timings` | each phase and the cache | text |
| `IETS_EVENTS` | one JSON line per store add | JSON, one object per line |
| `--log-format internal-json` | Nix's activity records, with ids | JSON, Nix's own schema |
| `src/trace/` | the read set of a run, for the tracing cache | not instrumentation; a cache |

Six text formats, each with its own grammar. A program that wants two of
these answers at once parses two grammars and correlates them by nothing.

## Three kinds of instrumentation, not one

A single event stream cannot carry everything. The rates differ by five
orders of magnitude.

| Kind | Rate per run | Mechanism | Cost it may pay |
|:---|:---|:---|:---|
| **aggregate** | up to 10^8 (thunk forces, calls, allocations) | a counter folded into the run totals | one add on the hot path |
| **event** | 10^2 to 10^5 (a store add, a fetch, a cache miss, a derivation) | one record with fields | ~1 k instructions, one write |
| **sample** | fixed, ~50/s | a thread that reads shared counters | nothing on the hot path |

So: three emitters, one vocabulary. A site declares which group it belongs
to and what it is called. The group decides whether that name is a counter,
a record or a sampled gauge, and a reader meets the same names everywhere.

## Groups

A group is a part of the evaluator, and it is the unit that is turned on.
Each group has a rate class, which decides how it may be built.

| Group | Covers | Class |
|:---|:---|:---|
| `syntax` | a file parsed: bytes, lines, time | cold |
| `ir` | compiled or loaded from the IR cache (replaces `IETS_IR_STATS`) | cold |
| `import` | a file imported, and the cache hit that answered it | cold |
| `store` | a path added, a `.drv` written, a references scan | cold |
| `drv` | a derivation instantiated: name, system, inputs | warm |
| `fetch` | an input fetched: url, bytes, where it came from, duration | cold |
| `flake` | a reference resolved, a lock node used, a `narHash` checked | cold |
| `cache` | the tracing cache and the input store: hit, miss, write | cold |
| `io` | `readFile`, `readDir`, `pathExists`, `hashFile` | warm |
| `build` | plan, missing, build start and end, upload | cold |
| `lint` | each warning, with its id and position | cold |
| `error` | each error raised, with its code | cold |
| `sched` | task spawned, stolen, parked; a fiber switch | hot |
| `eval` | a thunk forced, a call, a select miss, a `tryEval` scope | hot |

Cold groups may be on in any run. Warm groups are off unless named. Hot
groups are not in the default binary at all: they are behind a cargo
feature, as the census is, so the force path keeps no branch it does not
use.

## The knobs

`IETS_EVENTS` means "which events", not "where they go". The sink moves to
its own variable. The stream is two hours old and unreleased, so this breaks
nothing.

```
IETS_EVENTS=store,fetch         # these groups
IETS_EVENTS=all                 # every group this binary carries
IETS_EVENTS_TO=/tmp/e.jsonl     # a file, a unix socket, or '-' for stderr (the default)
```

`IETS_STATS` keeps its meaning: the counters at the end of a run. A `count!`
site registers its metric there, so a new counter needs no new printing
code.

## The record

```json
{"t":31204,"g":"store","event":"store-add","path":"/nix/store/…-builder.sh","kind":"tree","size":5288,"from":"/home/j/nixpkgs/…/builder.sh","at":"…/make-derivation.nix:693:11"}
```

* `t` is microseconds since the run started. It is the only correlation a
  reader needs for a timeline, and it costs one clock read.
* `g` is the group, so one stream can be filtered without a schema.
* `event` is the name, unique inside its group.
* `fiber` is added where a reader must tell concurrent work apart.
* Everything else belongs to that event.

A record is built by a borrowed builder, not by a `serde_json::Value`:

```rust
if let Some(mut r) = ev.events.rec(Group::Store, "store-add") {
    r.str("path", &path);
    r.u64("size", size);
    r.pos("at", ev, pos);
}   // Drop closes the object and writes the line, once
```

`rec` answers `None` when the group is off, so a call site builds nothing.
When it is on, the builder appends into a buffer the thread owns and escapes
with `print::json_escape`, which is what `builtins.toJSON`, `eval-jobs` and
`parse --json` already use. No `String` per key, no map, no serializer pass.
The mutex is taken for the write, not for the building.

Today's `store-add` uses `serde_json::json!`, which builds a
`BTreeMap<String, Value>` and serialises it into the sink one fragment at a
time. That cost 7.1% of a `hello` evaluation's instructions until one write
per line brought it to 1.0%. The builder is how it gets to a tenth of that.

## The schema is a registry, not prose

One table in the source names every group, every event and its fields.

* `iets help events` renders the table.
* `iets events --schema` prints it as JSON, for an agent or a program.
* A test asserts that every name a `rec` call uses is in the registry, so
  the documentation cannot drift from the code.

This is the three-audience rule applied to instrumentation: a human reads
the table, an agent reads the JSON, a program reads the stream.

## The cost contract

These are the rules a reviewer holds a new site to.

1. A group that is off costs one relaxed load and a branch.
2. A cold event costs about 1 k instructions and one `write`.
3. No event allocates a `String`, a `Vec` of its own or a `Value`.
4. A hot group is absent from a binary built without its feature.
5. Turning on every cold group costs less than 1% of a run.

## What this replaces, and what it leaves alone

Replaces: `IETS_IR_STATS` (becomes the `ir` group), the ad-hoc text lines,
and the next knob somebody would have added.

Leaves alone: `IETS_STATS`, because counters are not events;
`--log-format internal-json`, because that schema is Nix's and a Nix client
reads it; `src/trace/`, which is a cache and only shares a word.

Not a goal: OpenTelemetry, spans, or a second binary encoding. A duration
is an event with a `dur_us` field. A script turns the stream into a Chrome
trace for perfetto if somebody wants to look at it that way.

## Steps

1. The record builder and `json_escape`, with `store-add` as its first user.
   Measurable on its own: the stream should cost ~0.1% instead of 1.0%.
2. The group enum, the two knobs, and the off-group check.
3. The registry, `iets events --schema`, and the drift test.
4. The cold groups: `ir`, `import`, `fetch`, `cache`, `flake`, `build`,
   `lint`, `error`.
5. The warm groups: `drv`, `io`.
6. The hot groups behind `--features trace-eval`, and `IETS_PAR_STATS=2`
   re-expressed as the `sched` group's sampler.

Step 1 is worth doing whatever happens to the rest.

## Open questions

* Group granularity. Fourteen groups may be too many to remember and too
  few to filter on. `store` and `drv` could be one; `syntax`, `ir` and
  `import` could be one `load` group.
* One knob or two. `IETS_EVENTS=store,fetch@/tmp/e.jsonl` keeps one
  variable and costs a grammar.
* `t` in every record. A clock read per event is not free, and a reader
  that only wants a count does not need it. It could be opt-in.
* Whether a hot group needs a binary encoding. 10^8 records is 10 GB of
  JSON; at that point the honest answer is a counter, not an event.

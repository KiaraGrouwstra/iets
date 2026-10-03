# A proposal: one event vocabulary for the whole evaluator

`IETS_EVENTS` answers one question today: which paths did an evaluation add
to the store. Every other question about a run has its own knob, its own
format and its own parser. This says how to instrument every part of the
language without that happening again, and without the hot paths paying for
it.

Status: steps 1 and 2 are built, and the sampler of step 6 collects lambda
and builtin frames. The rest is still a proposal.

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

A hot group is not a stream of records. Nobody reads 10^8 lines. What a hot
group gives is a counter, or a sampled stack — see **A flame graph for
evaluation**.

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

Not a goal: a second binary encoding for a hot group. At 10^8 records the
honest answer is a counter, not a smaller record.

## Where OpenTelemetry fits

The question splits three ways: the protocol, the SDK, and the model.

**The protocol is cheap here.** iets already depends on tonic and prost for
the worker and store protocols, so an OTLP exporter is a prost message and a
channel — a few hundred lines and no new dependency tree. The backend
exists too: mon runs an OTLP collector on `:4317` over mTLS against the
fleet CA, with pipelines for logs, metrics and traces into ClickHouse
`otel`, and Grafana's traces explorer already points at `otel_traces`. So
"nobody would see it" is not true.

**The SDK is not cheap.** `opentelemetry` plus `opentelemetry_sdk` plus
`opentelemetry-otlp` plus `tracing`, `tracing-subscriber` and
`tracing-opentelemetry` is a stack, and it is async-first: a span costs an
`Arc`, a provider read, a `Vec` of owned key-value pairs, and a batch queue
behind a tokio task. It buys nothing that a prost message does not.

**The model fits one half of iets.** It fits work that has a beginning, an
end and one parent: a fetch, a file compiled, a plan, a build, an upload,
and above all a *remote* build. A trace id carried into a grpc builder or a
farm worker stitches eval, fetch, build and upload into one trace. A flat
local stream cannot do that, and for the farm that is the whole point.

It does not fit evaluation. A span is a scope on a thread; a thunk force
parks, migrates to another worker and is resumed somewhere else. A forced
value has many readers, so "what caused this store add" is a DAG, and a
span parent would name whichever fiber forced it first — an artefact of
scheduling, not a fact about the program. At 10^8 forces the answer is a
counter anyway, so spans never reach that part of the evaluator.

**Two things break if OTLP becomes the source of truth.** The stream is
deterministic: two runs of the same evaluation produce the same bytes, which
is how the cost of one write per line was proved to change nothing. Span ids
are random and batches arrive out of order. And a 70 ms process must flush
before it exits, while a collector that cannot be reached must never stall
an evaluation.

**So the stream stays the source of truth and OTLP is a sink.**
`IETS_EVENTS_TO=otlp://host:4317` exports the groups that are span-shaped;
`-` or a path keeps the JSON lines. One vocabulary, two renderings: an event
with a `dur_us` becomes a span, the rest ride as span events. The semantic
conventions have nothing for a build system, so the attribute names are ours
either way. A script turns the JSON stream into a Chrome trace for perfetto,
for the local case where no collector is wanted.

## A flame graph for evaluation

An event per call is the wrong instrument for evaluation, and so is a span.
The question about an evaluation is "where did the time go", and the answer
has a shape: a stack with a weight. That is the third mechanism, the
sampler, and it is the one part of this proposal with a finished design
elsewhere to learn from.

### What Nix does

`--eval-profiler flamegraph` (Nix 2.30) keeps a shadow stack through a
`preFunctionCall` and a `postFunctionCall` hook, and on **every** call it
reads `high_resolution_clock`; when 1/99 s has passed since the last sample
it does `callCount[stack] += 1`, hashing the whole stack as the key. It
flushes folded lines every two seconds: `frame;frame;frame <count>`, which
`flamegraph.pl` and every compatible tool read
(`src/libexpr/eval-profiler.cc`).

The output format is right and iets should produce exactly it. The sampling
is not sampling: the clock read is paid per call, whether or not a sample
is taken. At 48 million calls for one `hello` that is a syscall-free but
real ~20 ns each, about a second of overhead to collect 99 samples a second.

### What iets should do instead

**Do not read the clock on the call path.** A timer thread sets a flag; the
call path tests it. iets already has that shape: `force::SAMPLE_POS` is an
`AtomicBool` and `MAIN_POS` an `AtomicU64` that the `IETS_PAR_STATS=2`
thread samples. Generalise it: one relaxed load and a branch per call, and
the stack is hashed only when the flag is set, 99 times a second.

**Keep the shadow stack out of the default binary.** A per-fiber
`Vec<u32>` of frame ids costs two stores per call. That is affordable but
not free, so it lives behind `--features profile-eval`, as the memory
census lives behind `stats2`. A binary without the feature has no push, no
pop and no branch.

**A frame is a `u32`, symbolised at the end.** The call-site `Pos` plus the
lambda's name symbol identify a frame, and both are already interned;
`describe_pos` turns them into `file:line:col:name` when the profile is
written, not while it is collected. Cap the depth, so hashing a sample
stays bounded.

**Sample on the worker, at a call boundary.** Reading another thread's
shadow stack while it runs is a race, so each worker samples its own fiber
when it sees the flag. The bias is the same one Nix has: time inside one
long primop — a NAR hash, a regex — lands at the call that entered it, not
inside it. `perf` already answers that side, and a sample at every thunk
force narrows it.

**Weigh by what is scarce, not only by time.** The sampler adds 1 to a
stack's count today. Add the arena bytes allocated since the last sample
instead, and the same machinery draws an allocation flame graph — which
stack built the 630 MB that `nixos-desktop` holds. That is the question
`IETS_STATS=2` answers by object kind and cannot answer by call stack, and
Nix's profiler cannot answer at all.

**One graph, merged over workers.** Each worker folds its own samples; the
counts merge at the end into CPU time across the run. Laziness makes the
cost of a thunk land under whichever fiber forced it first, so two runs at
four workers do not fold identically. A profile is still honest — the
stack really was on CPU — but a profile to *compare* wants `--workers 1`,
as the bench workloads already use.

### The flags

`eval-profiler`, `eval-profiler-frequency` and `eval-profile-file` are
Nix's names, and `docs/reference/settings.md` lists all three as not
implemented. This implements them, so a classic `nix eval --eval-profiler
flamegraph` works and its output feeds the same tools. The native knob is
`IETS_PROFILE=eval`, with `IETS_PROFILE_TO` for the file, matching the
event stream's split of what from where.

## Steps

1. The record builder and `json_escape`, with `store-add` as its first user.
   Done: a `Field` trait the domain types implement, and the stream costs
   0.42% of a `hello` evaluation instead of 1.0%.
2. The group enum, the two knobs, and the off-group check. Done: twelve
   groups, `IETS_EVENTS` names them and `IETS_EVENTS_TO` takes the sink, and
   a group that is not watched costs one relaxed load and a branch.
3. The registry, `iets events --schema`, and the drift test.
4. The cold groups: `ir`, `import`, `fetch`, `cache`, `flake`, `build`,
   `lint`, `error`.
5. The warm groups: `drv`, `io`.
6. The sampler: the flag, the shadow stack behind `--features profile-eval`,
   folded output, the three Nix flags, and the allocation weight. It needs
   nothing from steps 2 to 5 and is the answer to "where did the time go".
   Done for lambda and builtin frames: `IETS_PROFILE=eval` writes folded
   lines, at about 1% of a `hello` evaluation and nothing at all in a stock
   binary (docs/reference/profile.md). The allocation weight and the three
   Nix flags are still open.
7. The OTLP sink, for the groups whose events carry a duration, and trace
   propagation into a remote builder.
8. The hot `sched` group, and `IETS_PAR_STATS=2` re-expressed as its
   sampler.

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
* Whether the shadow stack earns its place in the default binary. The guess
  was 1% to 3%; measured it is 0.1% to 0.4% of a `hello` evaluation with the
  feature on and the profile off, inside the run-to-run spread. It is
  behind `--features profile-eval` today, and that number is an argument for
  moving it.
* Whether the worker protocol carries a `traceparent`. Step 7 needs one
  field in the grpc request for a remote build to join the caller's trace,
  and an external protocol does not grow a field without a conversation
  first. The alternative keeps the remote side a trace of its own, which
  costs the join.

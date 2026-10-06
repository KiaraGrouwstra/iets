# More than one executor

`builder` names one place today. This is what it takes to let it name
several, and to choose between them from measurement instead of from a
Boolean an evaluation guessed.

Status: nothing here is built. Stage 1 is the only part worth starting
before the open questions at the end are answered.

## What is wrong now

Nix places a build from static priors:

| Prior | Set where | What it claims |
|---|---|---|
| `preferLocalBuild` | the derivation, at eval time | "this one is cheap" |
| `allowSubstitutes` | the derivation, at eval time | "do not bother asking a cache" |
| `speedFactor` | `/etc/nix/machines`, by hand | "this machine is 2.5 of those" |
| `max-jobs`, `cores` | config | a cap, not a rate |
| `requiredSystemFeatures` | the derivation, at eval time | a hard constraint |

Only the last one is honest. A constraint is a fact about the derivation
and belongs on it. The rest are cost claims, and none of the quantities
they stand for are known at eval time: how fast this machine is against
that one, what the link between them carries, whether the remote is up,
whether a cache already holds the output.

`preferLocalBuild` is the clearest case. In nixpkgs it marks plumbing:
`writeText`, `symlinkJoin`, a `runCommand` over small inputs. What it
really asserts is that the output is about the size of the inputs and the
build takes no time. iets knows the first at plan time and can measure the
second. So the flag is not a policy to respect. It is two numbers to
derive, and a cold-start prior until they exist.

## What iets has today

- `Transport` resolves one URL. `build::backend::open` returns one
  `Box<dyn Backend>`, and `Dispatcher` holds exactly that one
  (`src/build/mod.rs`).
- `backend::local()` already exists beside it: out-links and GC roots go to
  the local daemon whatever the builder is. So a second connection is
  already in the picture, for one narrow job.
- nix.conf `builders` is forwarded to the daemon in the `SetOptions`
  override map. Remote-builder placement therefore happens inside the
  daemon, where iets cannot see it or learn from it.
- `grpc://` delegates placement to the farm's scheduler, which is right:
  that scheduler knows its own workers.

So iets does not place builds. It picks a store and hands over.

## The hard part: validity is per store

`Backend::query_valid` asks one store what it has. `Uploader` ships what
that store lacks. Both are per store, and `StreamUpload` keeps its
registered set per store as well.

With two executors, A and B:

- what is valid on A says nothing about B;
- an output built on A is not valid on B;
- a derivation on B whose input was built on A needs those bytes moved,
  and nothing in iets moves them.

Nix solves this with the local store as a hub: inputs are copied out to the
remote builder, outputs are copied back. iets has no local store by
design, so it has no hub. This is the decision the rest of the proposal
hangs on, and it is not a cost model.

Three ways to bridge an edge that crosses executors:

1. **A hub store.** Make the local daemon the hub, as Nix does. Costs a
   round trip through the local disk for every cut edge, and makes a local
   store mandatory again. It contradicts the one property that makes iets's
   build path short.
2. **Executor to executor.** `FetchNars` from A, `AddMultipleToStore` into
   B, through the client. One hop, no disk, but the bytes cross the client,
   which is usually the worst-placed machine on the link, and B must accept
   an unsigned input-addressed path, which needs the `trusted` role
   (`docs/reference/builders.md`).
3. **A cache as the hub.** A pushes the output to a binary cache, B
   substitutes it. This is what the farm's workers already do between
   themselves. It needs push credentials and it costs two transfers, but
   neither crosses the client, and signatures make the trust question go
   away.

**Proposed: 3 where a cache is configured, 2 as the fallback, never 1.**

## Therefore: place subtrees, not derivations

If a cut edge is expensive, the scheduler must make few of them. Choosing
per derivation produces the most cuts possible. Choosing per *subtree*
produces few, and makes each one explicit.

So the unit of placement is a connected part of the derivation graph, and
the cost of a placement has a term for the cut:

```
cost(part, executor) =
      ship(inputs the executor lacks)
    + queue(executor)
    + build(part, executor)
    + cut(outputs another part needs elsewhere)
    + return(outputs the user asked for)
```

iets can evaluate this before it builds anything, because `remote::plan`
already walks the whole closure up front. That is the one structural
advantage over the daemon, which discovers goals lazily and can only ever
decide locally.

## The transfer term is per path, not per byte

Measured and already recorded in `docs/reference/builders.md`: the upload
is daemon-bound at about 0.5 ms per path for the sequential `addToStore`,
and no client-side cut of the upload changes it. The nixos-desktop cold
upload is 4,184 paths and 40.4 MB in 1.89 s — 0.45 ms per path, 21 MB/s,
on a loopback socket where bandwidth is not the limit.

So `ship` is `a * paths + b * bytes` with `a` around 0.5 ms, and for
typical nixpkgs path sizes the path count dominates. A cost model that
counts only megabytes will be wrong in the common case. Over `grpc://` the
same 776 paths are 490 KB instead of 2.66 MB, because one zstd stream
spans them all, which moves `b` but not `a`.

## Constraints filter, hints only start you off

Two kinds of input, never mixed:

- **Constraints** remove candidates: `platform`, `requiredSystemFeatures`,
  `__noChroot`, and whether the executor will accept what must be
  registered on it. A violated constraint is not a cost.
- **Cost** ranks what is left, from measurement.

`preferLocalBuild` and `allowSubstitutes` enter only as cold-start priors,
for a derivation with no history: `preferLocalBuild` means "assume build
time near zero and output size near input size". Once there are
observations for that derivation, the flag is ignored. This is a divergence
from Nix and goes in `docs/reference/divergences.md`.

## What to measure, and where it lives

A fourth cache store beside `trace/`, `ir/` and `inputs/`, with the same
version file, lock, stamp and 30-day sweep
(`docs/explanation/caching.md`).

Per executor, from the runs themselves, no probing:

- bytes per second observed on `AddMultipleToStore` and on `FetchNars`;
- round-trip time from the ops already timed in `--timings`;
- builds in flight and queue wait seen between a `Want` and an `Assigned`;
- liveness, which `grpc::health` already holds per worker address for the
  span of one batch and which this store makes outlive it.

Per derivation:

- wall time of the build, from the `BuildResult` both transports return;
- output size, from the path infos already fetched.

**Keyed on the derivation name, not the drvPath.** A drvPath changes on
every edit of anything beneath it, so a history keyed on it is empty
exactly when it is needed. `name` plus `platform` is stable across
revisions and is the only key with a hit rate worth having. It is a
coarse key: two unrelated derivations can share a name. That is acceptable
for a prior and must not be treated as more.

## How we will know it works

A cost model that is confidently wrong is worse than a dumb rule. So every
placement writes what it chose, the terms it chose from, and what the
alternatives were estimated to cost. Then:

- the run reports the estimate against the outcome, so a term that is
  always wrong is visible;
- a recorded run replays against a changed model offline, with no farm and
  no builds, which is how the model gets changed without guessing;
- `iets plumbing schedule` prints the placement and the terms for a closure
  without building it, as `iets plumbing plan` prints the walk.

If a stage cannot be checked this way, it does not land.

## Stages

Each is a branch and a PR of its own, with its line delta.

1. **`builder` becomes a list.** Config, `Transport` resolution, and
   `Dispatcher` holding a set of backends with per-executor validity and
   upload state. No policy: the first executor that satisfies the
   constraints gets the work, so behaviour with one entry is unchanged.
   This is most of the work and most of the lines. Estimate +400.
2. **Liveness outlives the batch.** `grpc::health` generalised to an
   executor, persisted in the new store, so a dead builder stays known
   across runs. Estimate +120.
3. **Cut edges.** Cache-as-hub, with the client hop as the fallback.
   Estimate +250.
4. **Observations.** The store, the two measurements, the decision log.
   No scheduling change: it only records. Estimate +300.
5. **The model.** Subtree partition and the cost function, behind a setting
   that defaults to the stage-1 behaviour until the replay says it wins.
   Estimate +350.

Stage 1 is worth landing on its own: two executors with a dumb rule is
already more than one executor, and nothing after it is reviewable without
it.

## Open questions

1. **Does a local store come back?** Option 1 above is rejected here, but
   it is the cheapest to build and the only one that needs no cache
   credentials. Is "iets owns no local store" a property to keep at this
   price?
2. **Does the farm count as one executor or many?** Treating it as one
   keeps iets out of a scheduling argument it would lose, because the farm
   knows its workers and iets does not. It also means iets cannot route
   around one bad worker, which is what stage 2 is for.
3. **Which cache for the cut edges?** niks3 is the Numtide answer. A cut
   edge through a cache that the user has no push credentials for falls
   back to the client hop, which needs the `trusted` role on the
   destination. Both can be unavailable, and then the placement must be
   refused before the build, not after.
4. **Is a name a good enough key?** It is the only stable one. The
   alternative is no history at all.

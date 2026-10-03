# The evaluation profile

`IETS_PROFILE` collects a sampled flame graph of an evaluation — `eval` for
the time it spent, `alloc` for the memory it built — and
`IETS_PROFILE_TO` says where it goes: a file, or `-` for stderr, which is
the default. The output is the folded form, one line per stack:

```
frame;frame;frame <count>
```

`flamegraph.pl` and every tool that reads its input take it unchanged.

```
IETS_PROFILE=eval IETS_PROFILE_TO=/tmp/p.folded iets eval --workers 1 -f . -A x
flamegraph.pl /tmp/p.folded > /tmp/p.svg
```

## Two weights

`IETS_PROFILE=eval` counts one per sample, so a stack's number is the time
it was on CPU. `IETS_PROFILE=alloc` counts the arena bytes allocated since
the sample before it, so a stack's number is the memory it built, and the
same graph answers "which stack made the 630 MB this run holds". Nix's
profiler cannot answer that, and `IETS_STATS=2` answers it by object kind
and not by call stack.

Every byte is credited to the next sample, so the folded counts add up to
what the arena allocated and only what came after the last sample is
missing: 27,414,293 of the 27,427,237 bytes of a `hello` evaluation, which
a test asserts.

A frame is `file:line:column:name` — the call site and the lambda it
entered — and `file:line:column` for a lambda with no name. A builtin is a
frame too, written `file:line:column:«__genList»`, so time inside one is
not blamed on the lambda that called it. A frame holds
no `;` and no space, so a path that has either is rewritten.

## It needs its own binary

The shadow stack is two stores per call, which is not free, so it lives
behind a Cargo feature:

```
cargo build --release --features profile-eval
```

A binary without the feature has no push, no pop and no branch, and says so
when `IETS_PROFILE` asks it for a profile. With the feature and no
`IETS_PROFILE`, a `hello` evaluation costs between 0.1% and 0.4% more
instructions than a stock binary, which is inside the run-to-run spread of
either; collecting a profile costs about 0.9%, and `alloc` costs the same
as `eval` because the arena is read only where a sample is taken. Measured
interleaved with
`perf stat -e instructions:u`.

The cost is low because no clock is read on the call path. A tick thread
bumps a counter 99 times a second — `IETS_PROFILE_HZ` sets the rate — and a
call tests one relaxed load against the last tick it answered. Nix's
`--eval-profiler` reads `high_resolution_clock` on every call instead, which
is about a second of work for the 48 million calls of a `hello`.

## Read it at one worker

Use `--workers 1`.

Each fiber samples its own shadow stack, because reading another thread's
while it runs is a race. A fiber's stack starts at the thunk it was handed,
not at the root of the evaluation, so a run at many workers gives many
shallow stacks — often one frame — and the graph has no shape. The same
`hello` evaluation gives 78-frame stacks at one worker.

Laziness biases the result the same way Nix's profiler is biased: the cost
of a thunk lands under whichever fiber forced it first, so two runs at the
same worker count do not fold identically either. A profile to compare
wants one worker.

## What it does not see

A sample is taken at a call boundary, so time with no call in it is
invisible: parsing and compiling a file, and the inside of one long builtin.
A NAR hash lands on its `«__hashFile»` frame and not under the loop that
does the work, which says which call cost the time but not where inside it
the time went. `perf` answers that side, and `IETS_STATS` says how much of
each kind of work a run did (`docs/reference/settings.md`).

A run too short for one tick writes nothing and says so, rather than leave
an empty file behind.

## Nix's own flags

`--eval-profiler`, `--eval-profiler-frequency` and `--eval-profile-file` are
not implemented, and `iets eval --help` does not offer them. They are
nix.conf settings, and no evaluator-wide knob in iets reads nix.conf yet:
the ones that exist come from `iets.toml`, `--iets-option` and the `IETS_*`
variables. Wiring them is a change to that route, not to this profiler, so
it waits for a reason to make it.

The output format is already Nix's, so a profile collected here feeds the
same tools as one collected there.

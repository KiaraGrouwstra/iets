# The evaluation profile

Set `IETS_PROFILE` to get a sampled flame graph of an evaluation. Use
`eval` to see where the time went, and `alloc` to see what built the
memory.

```
IETS_PROFILE=eval IETS_PROFILE_TO=/tmp/p.folded iets eval -f . -A x
flamegraph.pl /tmp/p.folded > /tmp/p.svg
```

`IETS_PROFILE_TO` says where the profile goes: a file, or `-` for stderr,
which is the default. The output is the folded form, one line per stack:

```
frame;frame;frame <count>
```

`flamegraph.pl`, and every tool that reads its input, takes it as it is.

## Two weights

With `IETS_PROFILE=eval`, each sample counts one, so a stack's number is
the time it spent on the CPU.

With `IETS_PROFILE=alloc`, each sample counts the arena bytes allocated
since the sample before it, so a stack's number is the memory it built.
The same graph then answers "which stack made the 630 MB this run holds".
Nix's profiler cannot answer that. `IETS_STATS=2` answers it by kind of
object, not by call stack.

Every byte goes to the next sample, so the counts add up to what the arena
allocated. Only the bytes after the last sample are missing: a `hello`
evaluation counts 27,414,293 of its 27,427,237 bytes, and a test checks
this.

A frame is `file:line:column:name`, the call site and the lambda it
entered. A lambda with no name is `file:line:column`. A builtin is a frame
too, written `file:line:column:«__genList»`, so the time inside it is not
charged to the lambda that called it. A frame holds no `;` and no space,
so a path that has either is rewritten.

## What it costs

When `IETS_PROFILE` is not set, the cost is too small to measure. The
sampler is always compiled in, and `IETS_PROFILE` is the only switch.

A profile costs about 0.9% of a `hello` evaluation, measured with
`perf stat -e instructions:u`. `alloc` costs the same as `eval`.

`IETS_PROFILE_HZ` sets the sample rate. The default is 99 samples a
second.

## It runs at one worker

A profile pins the run to one worker. If you set `--workers` or `workers`
to anything but 1, the run stops with an error.

Each worker can only sample its own stack, and that stack starts at the
thunk the worker was handed, not at the root of the evaluation. At many
workers you would get many short stacks, often one frame each. At one
worker, the same `hello` evaluation gives stacks of 78 frames.

Laziness still biases the result, as it does in Nix's profiler: the cost
of a thunk goes to whichever caller forced it first.

## What it does not see

A sample is taken when a function is called, so you do not see time spent
with no call in it: parsing and compiling a file, and the inside of one long
builtin. A NAR hash shows up on its `«__hashFile»` frame, not under the
loop that does the work. Use `perf` to look inside a builtin, and
`IETS_STATS` to count each kind of work (`docs/reference/settings.md`).

A run too short for one sample writes nothing, and says so.

## Nix's own flags

iets does not implement `--eval-profiler`, `--eval-profiler-frequency` or
`--eval-profile-file`, and `iets eval --help` does not offer them. They are
nix.conf settings, and iets reads its evaluator settings only from
`iets.toml`, `--iets-option` and the `IETS_*` variables.

The folded output has the same shape that Nix writes: frames joined by
`;`, a space, then the count. A lambda frame is the same `callsite:name`.
One label differs: Nix writes a builtin as `callsite:primop foo`, and iets
writes `callsite:«__foo»`. Flame graph tools treat both as plain text, so
both work with the same tools. A frame-by-frame diff of a Nix profile and an
iets profile does not line up on builtins.

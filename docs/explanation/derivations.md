# Why a derivation is kept as its ATerm

A `.drv` file looks like an implementation detail. You could ask: what if
iets held a typed recipe inside, and wrote the ATerm only as an export?
This page answers that question. It starts from what a build task needs,
then compares that design with what iets does, and ends with the
experiment that decided it.

The short answer: iets already treats the `.drv` as an export. It writes
nothing to a store while it evaluates. What it keeps in memory, when it
keeps anything, is the ATerm text, and the text is the cheapest form to
keep. A typed form kept beside or in place of it costs more on every
workload that exports, and parity does not leave room for the benefits
a typed form would bring.

Numbers on this page come from bld1, nixpkgs `ci/eval/outpaths.nix`,
`eval-jobs --workers 1 --chunks 8 --chunk 0`, iets 4ae08336. Claims about
Nix cite the Nix 2.34.8 source.

## The design from the requirement

The requirement, with nothing else: given a description of how to produce
something, name the result so that equal descriptions share a name, and
tell the caller whether it exists.

A build task needs a platform, a builder, its arguments, an environment,
its inputs (store paths and other tasks' outputs), and its outputs, each
input-addressed, fixed or floating content-addressed. That is all.

To name a task, hash a canonical form of it, with each input task replaced
by its own name. That is one recursive hash per task. Output paths follow
from that name. "Does it exist" is a store query on the output paths.

Inside one process the tasks form a graph already: each task points at its
inputs. No second build graph is needed. The canonical form is a function
that writes into a sink (a hasher, a socket, a file), so it never has to
be held as a buffer. You write it out only when a task leaves the process:
a remote build, `--dump-drvs`, a `readFile` of its path, a GC root.

## What iets does, and why each difference stands

| the design above | iets | verdict |
|---|---|---|
| no store writes while evaluating | no store writes while evaluating; `.drv` files go up after evaluation in one upload (`src/builtins/derivation.rs`, the comment above `produces_drv`) | same. Nix writes each `.drv` from `derivationStrict` unless `readOnlyMode` is set (`src/libexpr/primops.cc:1826`) |
| one hash per task | three per task: the masked modulo hash, the unmasked modulo hash, and the hash of the ATerm itself | compatibility. Output paths come from the masked hash and drvPaths from the plain ATerm (`src/libstore/derivations.cc:641`, `:897`, `:944`). drvPath and outPath are values a user sees |
| canonical form streamed into the hasher | rendered into one reused per-thread buffer, then hashed | same in effect: no allocation, one extra memcpy |
| the task graph is the build graph | `DrvState::drvs` holds each derivation's outputs and references; `remote::plan::walk` orders it | same |
| keep a typed task, render on export | drop the typed `Derivation` after `derivationStrict`; keep the ATerm text when the run will export, keep nothing otherwise | iets is better here; see the experiment |
| tasks named by an in-process handle | tasks named by drvPath in a hash map | equivalent. The drvPath must exist anyway |

The cost of naming is real and it is all compatibility. On the outpaths
chunk, SHA-256 is 4.5 % of samples and ATerm escaping (`print_string`) is
3.9 %. Parsing ATerms back is about 0.2 %.

## Where the format reaches code that should not care

| place | what it touches | real or accident |
|---|---|---|
| values (`src/runtime/value.rs`) | the drvPath string in string context (`DrvDeep`, `Built`); no derivation struct, no ATerm | real: drvPath and its context are Nix values |
| tracing cache key (`src/trace/key.rs`) | nothing from derivations | clean |
| tracing cache value (`src/trace/wire.rs`, `encode_drvs`) | the ATerm bytes, for `build` runs | accident of the kept form, and a harmless one: the text is the smallest stable form found |
| cache read set | the bytes of `.drv` files read from `/nix/store` | real: they are inputs to the evaluation |
| provenance (`src/provenance.rs`) | the drvPath string, positions, attribute paths | clean |
| worker protocol (`src/remote/worker/wire.rs`) | the ATerm as a NAR for a text path, then build `drv!out` | real: this is what the daemon accepts |
| gRPC build (`src/remote/grpc/build.rs`, `drv_table`) | parses every ATerm of the closure back to send a `BasicDerivation` | accident; costs little |
| store adds (`src/remote/plan.rs`, `walk`) | each `.drv` after its inputs | real: the daemon registers a path only when its references are valid, and the order is observable |
| read-back in jobs, build, devenv, `outputOf` | `Derivation::parse` of the kept text | accident; about 0.2 % of instructions |
| `readFile` or `import` of a drvPath this run made | served from the kept text; without it, an error | accident that shows as behaviour; see below |

**`readFile` of a fresh drvPath.** With `--no-tracing-cache`, `iets eval`
keeps no text, so `builtins.readFile drv.drvPath` fails with "is not in the
store and this run did not keep its text". With the cache on it returns
the ATerm, and so does Nix. This is the cost of keeping nothing. Serving it
always would mean keeping some form of every derivation, which is the
331 MB that `155cbdf5` removed from `eval-jobs --no-instantiate`.

**Non-UTF-8 bytes.** iets keeps environment values and arguments as bytes
and renders them byte for byte, as Nix does. A typed boundary that refused
non-UTF-8 values would change which expressions produce a derivation,
which moves errors, and for every value it encodes instead it would change
drvPaths. Parity forbids both. The boundary can reject only where Nix
also rejects. So a typed boundary cannot make this class of bug impossible
without breaking parity.

## What an export-only `.drv` would buy

- **Streaming a task to a builder with no disk round trip.** iets does
  this already: it uploads from memory and writes nothing locally.
- **A typed, versioned recipe.** No consumer reads one. The daemon reads
  ATerm and `BasicDerivation`.
- **Relocatable outputs, content addressing first.** Output paths go into
  the environment, and the environment goes into the drvPath. Floating
  content-addressed outputs exist (`DerivationOutput::CaFloating`) as Nix
  defines them. Anything more changes drvPaths.
- **Less memory.** This is the only item open to measurement, so it is the
  experiment.

## The experiment

Hypothesis: the kept form can be a typed derivation that differs from the
export, with parity, and with no extra cost. To test it, the spike changed
the kept text to `Arc<Derivation>`, rendered the ATerm only on export, and
let `eval-jobs` take the typed value with no parse.

Parity held. `eval-jobs` output was identical. Into a fresh daemon store
(`--chunks 64 --chunk 0`), base and spike added the same 25,247 paths in
the same order, and the 23,821 `.drv` files were identical byte for byte.

The cost did not hold:

| build | instructions | peak RSS |
|---|---|---|
| base | 230.6 G, 230.7 G | 4,222 MB, 4,223 MB |
| typed kept form | 241.2 G, 251.0 G | 4,246 MB, 4,263 MB |

That is +4.6 % to +8.8 % instructions and +22 to +41 MB. The instructions
are one more render per derivation: `eval-jobs` exports every derivation,
so a lazy render only moves work to later and does it twice. The memory is
the owned struct: per-entry `String` and B-tree overhead is larger than
the text.

A tighter typed form would borrow evaluator strings instead of copying
them. A counter on the same chunk bounds what that could save: of
175.8 MB of ATerm text, only 25.8 MB is environment values already in the
arena; 9.8 MB are built fresh, and 61.5 MB are structured-attrs JSON. The
borrowed bytes are 0.6 % of peak RSS. Even if all 175.8 MB of text went
away, that is 4.2 %, and the extra render on export remains.

## When to ask again

The conclusion depends on two facts. Most derivations an evaluation makes
leave the process, and the only consumer outside the process speaks ATerm.
If a common workload made many derivations but exported few, or a builder
took a typed recipe over gRPC, a typed kept form would be worth measuring
again.

One cheaper idea did not get a test: the three hashes of one derivation
escape the same environment values three times. Escaping them once and
reusing the bytes could cut part of the 3.9 % without changing any form.

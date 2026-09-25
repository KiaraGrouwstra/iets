# Plumbing and porcelain

ietsp has one code path. Every capability is an operation in the library,
with a machine-first contract. A plumbing command exposes that operation
with JSON in and JSON out. A user-facing command — the porcelain — is
written against the same operation. This is git's split.

The operation contracts and the JSON shapes are in
[reference/plumbing.md](../reference/plumbing.md). This page is the rule,
how it is enforced, and the honest record of what the migration to it
cost.

## The rule

**A capability is implemented once, below the command line.** Evaluation,
store access, build dispatch, fetching and flakes live in the library. No
command re-implements one.

**Porcelain may have its own libraries.** Presentation logic, interactive
flows, Nix argv compatibility, colour, tables, progress and UI caches are
porcelain-side code. They may be large. They may be shared between
commands. They are not a second implementation of a plumbing capability.

**The plumbing must expose what the porcelain needs.** If a porcelain
command reaches past `crate::api` into `crate::eval`, `crate::store`,
`crate::build`, `crate::remote`, `crate::flake` or `crate::jobs`, the
plumbing has a gap. Close the gap in the operation layer first.

The split test is one question: **would a plumbing operation emit this
too?** If yes, it is a value and it belongs below the command line. If it
only ever reaches a terminal, it is presentation and it belongs in the
porcelain.

### Why a namespace and not a second binary

`ietsp plumbing OP`, not `ietsp-plumbing`:

- A second binary doubles a large static binary and the Nix package, for no
  gain.
- Classic mode dispatches on `argv[0]`. A second binary complicates the
  symlink set and the exec fallback.
- A hidden prefix hides a contract that must be stable, and a contract
  nobody can find gets copied instead of called.

`ietsp plumbing` stays out of the top-level overview, because that help is
for people and the operations are for programs. It is discoverable through
`ietsp plumbing --help` and through the reference page.

## How the rule is enforced

**A module boundary, checked by a test.** `tests/layering.rs` reads every
file under `src/cli/` and fails when one names `crate::eval`,
`crate::store`, `crate::build`, `crate::remote`, `crate::flake`,
`crate::jobs`, `crate::devenv`, `crate::trace`, `crate::compile` or
`crate::parallel`, outside a per-file allow-list.

**Types.** The end state is that an evaluator value does not appear in a
porcelain signature: a command that cannot name a value cannot evaluate
one. The same test checks it against a second list, so the rule is adopted
file by file.

Both lists have their length asserted against a constant, and the test
fails on an entry *wider* than the code needs as well as on a missing one.
So a reach that goes cannot come back unnoticed, and the lists cannot stay
ahead of the code.

The lists started at:

| list | length | what is on it |
|---|---|---|
| reaches below `api` | 17 files | `build.rs`, `cache_dump.rs`, `classic/{build,develop,instantiate,mod,nix,shell}.rs`, `devenv.rs`, `diff.rs`, `eval.rs`, `fastbuild/mod.rs`, `jobs.rs`, `mod.rs`, `plumbing.rs`, `render/text.rs`, `search.rs` |
| evaluator value in a signature | 7 files | `classic/{instantiate,mod,nix,shell}.rs`, `jobs.rs`, `mod.rs`, `search.rs` |

`crate::flake` is on neither, because the flake migration took it. The
layer named most often is `crate::eval` (13 files) — the evaluator type
itself, and the last one that will go. `crate::remote` (11) is the
transport a command still names to open a connection.

**What the test does not ban is the point of it.** A porcelain-side library
may be as large as it needs to be: the render module is 264 lines of
presentation and passes; the classic module is 2,254 lines of Nix argv
compatibility and passes. The one question is whether a file holds a second
implementation of something the operation layer owns. Both failure messages
say so, and both name the two ways out: close the gap in `crate::api`, or
say why the entry belongs on the list and move the constant.

Beyond the test there is a review checklist. Before merging a change under
`src/cli`: does it call anything below `crate::api`, and if so, is it
presentation, compatibility or interaction? Does it do something an
operation already does, in its own way? Does a new operation have a JSON
shape in the reference and an end-to-end case? Did the allow-list grow?

## What the migration cost

Nine stages ran, one was cancelled, and the measured net is **+1,262 lines**
in `src`, against a plan that promised **−450**. That record is the most
useful thing on this page.

| # | stage | measured |
|--:|---|---:|
| 1 | `api::installable` and `plumbing resolve`; move four commands onto it | +220 |
| 2 | `api::render` and the porcelain render module: one JSON writer, one size and table formatter | +178 |
| 3 | `api::run::Run`: the evaluation scaffold as one call | −7 |
| 4 | `api::build`: one dispatch path; `plumbing plan`, `missing`, `build` | +417 |
| 5 | `api::installable` grows the get-drvs walk and `outputsToInstall` | +113 |
| 6 | `src/fetch` split out of `src/flake`; `plumbing fetch` | +68 |
| 7 | `src/treestore` on the shared cache layer; the input store collapses into it | +117 |
| 8 | `src/source`: a reference resolves to a tree; `api::flake`; `crate::flake` private | +156 |
| 9 | six more operations | cancelled |
| 10 | `tests/layering.rs` and the two allow-lists | 0 in `src`, +304 in tests |

Every estimate was negative. Every estimate was wrong. Five rules came out
of finding out why.

### 1. A shared module costs 100-175 lines before it saves one

`api::installable` started at 175 lines; `api::run` is 102. A stage that
removes N copies of a K-line sequence nets `N*K − 150`, not `N*K`.

### 2. The saving comes from deleting a repeated *type*, not a repeated line

Folding the cache lookup, the trace switch, the lint call and the parallel
run into one call saved about 100 lines over sixteen sites — six lines each.
Deleting four per-command `enum Run { Failed, Unsupported, … }` and the
match that fed each one saved as much again, from five sites. Look for the
type, not for the line.

Stage 4 is the same lesson positively. The collapse worked: the per-command
types went with the sequence, the merge-into-targets loop and its
`HashMap<String, BTreeMap<String, Option<String>>>` and its "derivation has
no output" match — written out twice at about 45 lines each — became one
call. That turned a predicted −80 into −220. The module still cost 187, so
the refactor was +33 and the stage was +417 because of the three operations
on top.

### 3. A plumbing operation costs more than the operation it exposes

Stage 4's three operations are 384 lines against a dispatch layer they call
in four lines each. Flag parsing, the root vocabulary, the help text and
the JSON shaping *are* the operation; the capability was already there. A
stage that adds operations nets positive unless the porcelain it replaces
is deleted in the same stage.

This rule is why stage 9 was cancelled. `index`, `jobs`, `dev-env`,
`drv-show`, `drv-diff` and `closure-diff` were charged +150 for six on the
same arithmetic that cost 384 for three, so about +720 — and every one of
them exposes something a caller can already reach. `ietsp eval-jobs` and
`ietsp print-dev-env` are the JSON contract already. `ietsp diff` composes
`resolve` with its own comparison. `api::installable` and `api::build` own
the rest. Six operations at that price buys capability that already exists.

The same arithmetic stopped two more. `flake-info` and `flake-tree` would
have been about +240, and nothing needed them: the layering rule asks that
a porcelain command reach a capability through `crate::api`, and
`api::flake` *is* that, as a library surface. An operation is warranted
when a program *outside* ietsp needs the contract, or when a porcelain
command would otherwise reach past `crate::api`. Neither was true.

### 4. Count the copies, not the call sites

Nine renderers, sixteen evaluation scaffolds and eight dispatch sequences
are three different numbers of copies: sixteen and eight were copies, nine
were not. Stage 2's nine renderers were nine different *outputs*. What
actually repeated across them was one byte writer, one JSON dump and four
`if json { … } else { … }` branches — about 60 lines. Only one per-command
output type existed to delete.

Before estimating a stage, say what the second copy is. If the answer is
"they all do the same *kind* of thing", the saving is the kind, not the
thing. Stage 5 had no second copy at all, and this rule said so in advance:
the plan charged −180 to "two functions lose their callers", and losing a
caller deletes nothing.

### 5. A move is not a deletion

Stage 7 was estimated at −300 and measured +117. The counts were right
about the modules and wrong about what a module count means: −372 for one
file and −190 for another were the same lines the table charged +380 for
under their new home. Netting a move against its destination is how −0
turns into −562 on paper. Count only what no longer exists anywhere.

Stage 8's move was +3, exactly as this rule predicts: 712 lines left one
file to arrive in two others.

## What it bought

Not a line count. Two things:

**One implementation and one name per capability.** `api::installable`
turns a string into derivations, `api::build` dispatches a build,
`api::run` runs an evaluation, `api::fetch` fetches, `api::flake` resolves
a flake, `api::render` says what a value's JSON is. Before the first stage
there were three attribute-path tables, eight build-dispatch sequences,
sixteen evaluation scaffolds and two NAR framings. There is one of each. Ten
porcelain files call `api::installable`; five no longer reach into
`crate::store`.

**A contract.** `ietsp plumbing resolve | ietsp plumbing plan` composes
across processes, `missing` is byte-for-byte `build --dry-run --json`, and
every operation's JSON shape is in the reference with an end-to-end case
that checks it. That is the part a program outside ietsp can use, and it
did not exist before.

Several stages were worth it for reasons the ledger cannot show. Stage 5
moved a capability that had been reachable only by the command that
happened to hold it. Stage 7 gave a name to a content-addressed store
nobody had named, and made the only way in a function that hashes what it
is given and refuses anything else — which is what let stage 8 put a
resolver over it. Stage 8 made `crate::flake` private, so "flakes" stopped
being a thing four command files each knew a different piece of.

## Two gaps stay open

`nix-shell` and `print-dev-env` each open a connection of their own. They
upload a plan and then speak the worker protocol themselves — build the
derivation's *inputs* and exec a shell, or read the environment output
back — so they use the plan and the store lookup but drive the upload
directly, because routing them through the dispatcher would open a second
connection. Stage 9's `plumbing dev-env` was to close this, and stage 9 is
cancelled. It is recorded as a gap, not as a plan.

`drv-show` was to be the reader of a dumped-derivation directory, which
`api::build::drvs_in_dir` already is.

## What the line count says about the next refactor

Not "do not refactor". Say what the second copy is before starting, and
expect the number to be positive when there is not one. Every stage that
named a real second copy — stage 1, stage 3, stage 4's collapse, stage 5's
attribute readers, stage 7's NAR framing — got a real deletion out of it.
Every stage that moved a module got +0 for the move and paid for the
interface.

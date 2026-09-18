# fast-build: nix-fast-build's command line, in one process

`ietsp fast-build` evaluates an attribute tree and builds it, with
nix-fast-build's flags and result formats. It is a drop-in for
[nix-fast-build](https://github.com/Mic92/nix-fast-build) a3e9a5a, so a
wrapper that execs `nix-fast-build --store … <user flags>` can exec
`ietsp fast-build --store … <user flags>` instead.

```
ietsp fast-build                                   # .#checks for this system
ietsp fast-build --flake .#checks.x86_64-linux --skip-cached
ietsp fast-build --file release.nix -A ci --argstr system x86_64-linux
ietsp fast-build --store ssh-ng://builder --result-format junit --result-file r.xml
nix-fast-build --flake .#checks                    # the same, under the classic name
```

## What is different from nix-fast-build, and why

nix-fast-build orchestrates processes: `nix-eval-jobs` for the evaluation
and one `nix build` per attribute, with `nix copy` and `nix log` beside
them. ietsp does all of it in one process against one `nix-daemon`
connection. That removes every per-attribute process spawn, every
re-evaluation of the same flake by a second `nix` client, and the
`nix copy`/`nix log` round trips.

Three consequences are visible to a user.

**No `--remote` mode.** nix-fast-build's `--remote` uploads the flake and
runs the whole pipeline over ssh. ietsp evaluates locally and speaks the
worker protocol to the builder, which is what `--store ssh-ng://host`
already does, better: the flake is never copied and the evaluation never
runs twice. `--remote` and `--remote-ssh-option` are errors, not silent
no-ops.

**No uploads to binary caches.** `--copy-to`, `--cachix-cache`,
`--attic-cache`, `--niks3-server`, `--push-build-closure` and
`--always-upload-source` are errors. Pushing what a build produced is the
build farm's job, not the client's: the farm already holds the outputs and
can sign and push them without a second copy over the client's uplink. A
client that also pushes doubles the bytes on the slowest link in the
system.

**Downloads stay where the build happened.** For the default local daemon
there is nothing to download: the outputs are already in `/nix/store` and
`--out-link` points at them. For a remote `--store`, ietsp leaves the
outputs on the builder, as `--no-download` asks for, and says so once. To
register a foreign input-addressed path in the local store a client needs
`QueryPathInfo` (worker op 26, not implemented) *and* trusted-user status
on the local daemon, because an untrusted client can only add
content-addressed paths. Neither holds in CI, which wants exit codes and
logs rather than bytes. `--no-download` is therefore accepted and is the
effective behaviour.

Smaller differences:

- `--eval-max-memory-size` and `--stall-timeout` are accepted and ignored.
  ietsp has no worker processes to restart at a memory limit, and the
  stall timeout is a constant (`build::render::STALL`, 300 s).
- `--nix`, `--nix-eval-jobs` and `--nix-build` are accepted and ignored
  with a note on stderr: there is no subprocess to point at.
- `--override-input` and `--reference-lock-file` are errors. ietsp
  evaluates locked flakes only; an override makes the lock incomplete.
- `--no-nom` is the default, because ietsp renders the logs itself.
- No gc-roots directory. nix-fast-build roots each job's `.drv` in a temp
  directory for the duration of the run. ietsp holds the drvs on its own
  connection and needs none; a garbage collection racing the run is the
  one case this loses, and it loses it by rebuilding.
- Provenance. Every state-change line names the attribute and, when the
  evaluation knows it, the source position of the derivation:
  `▶ building checks.x86_64-linux.foo (pkgs/foo.nix:12:5)`. That comes
  from the `pos` key `ietsp eval-jobs --provenance` adds
  (`docs/provenance.md`); nix-eval-jobs has no such key.
- An unknown flag is not an error. `nix-fast-build` is a real binary on
  many hosts, so an unknown flag falls back to it with
  `nixbin::real("nix-fast-build")`, exactly as classic mode falls back
  (`docs/classic.md`). A missing feature degrades instead of breaking a
  pipeline.

## Flag coverage

| covered | erroring | accepted and ignored |
|---|---|---|
| `--flake`/`-f`, `--file` (+ `-A`, `--arg`, `--argstr`, `-I`, `--pure`), `--impure`, `--select`, `--option`, `-j`/`--max-jobs`, `--store`, `--eval-workers`, `--skip-cached`, `--skip-unsupported`, `--systems`, `--out-link`, `--retries`, `--fail-fast`, `--result-file`, `--result-format`,  `--stream-json-lines`, `--no-fold` | `--remote`, `--remote-ssh-option`, `--always-upload-source`, `--copy-to`, `--cachix-cache`, `--attic-cache`, `--attic-ignore-upstream-cache-filter`, `--push-build-closure`, `--niks3-server`, `--override-input`, `--reference-lock-file` | `--no-nom`, `--no-link`, `--no-download`, `--eval-max-memory-size`, `--stall-timeout`, `--nix`, `--nix-eval-jobs`, `--nix-build`, `--debug` |

## The dispatcher is not fast-build-only

Every ietsp command that realises a derivation goes through
`crate::build::Dispatcher`: `ietsp build`, classic `nix-build` and classic
`nix build` as well as `fast-build`. Builds are fast by default, not in a
special mode.

One dispatcher, two ways of feeding it:

- `build::upload::PlanUpload` ships a `remote::plan::Plan` computed once,
  for the commands that know every root before they build anything. A
  single-root build is one job through the same dispatcher.
- `build::upload::StreamUpload` is incremental, for the eval-jobs walk.
  Per batch it walks only the part of the closure it has not registered
  yet, then one `QueryValidPaths` and one `AddMultipleToStore`. It
  duplicates `remote::plan::plan`'s walk on purpose: `plan` cannot prune
  at a derivation already known valid, and re-planning a whole closure per
  batch is quadratic on a large `checks` set.

The call sites in `src/cli/build.rs`, `src/cli/classic/build.rs` and
`src/cli/classic/nix.rs` are one call each. Stdout of the classic names is
byte-identical to what it was (`scripts/classic-e2e.nu` compares it); all
the new progress goes to stderr.

## Dispatch design

```
main fiber  ─ eval-jobs walk ─┬─ job ─┬─ job ─────┬─ job ─ ... ─ end
                              │       │           │
                              ▼       ▼           ▼
dispatch thread        ┌─ batch 1 ──┐ ┌─ batch 2 ─┐
                       │ upload     │ │ upload    │
one connection         │ build op   │ │ build op  │
                       └────────────┘ └───────────┘
```

The walk is `jobs::run` with a `jobs::stream::Stream` sink instead of the
emitter, so the eval-jobs walk is shared, not copied. A `Batcher` takes
the first line blocking, then everything else already queued (20 ms
settle, capped at `-j`), so a batch is whatever finished while the last
build op ran. That is the overlap: building starts before the tree is
fully evaluated.

Each batch, in order:

1. `--skip-cached`: the existing cache-status resolver
   (`jobs::cache::Resolver`, `docs/eval-jobs.md`) on a second connection.
   It runs before the upload so a cached attribute costs no bytes. It
   reads input derivations from `/nix/store`, which an in-process
   evaluation never wrote, so a walk it cannot finish just means "build
   it", reported once on stderr.
2. Filter: `--systems`, then `cacheStatus` (`cached`, or `local` without
   `--out-link`). A filtered attribute gets an `EVAL` record carrying its
   cache status and no `BUILD` record, which is what nix-fast-build does.
3. Dedupe by derivation path.
4. Upload the missing part of the batch's closure.
5. One `BuildPathsWithResults` with every derivation in the batch.

Step 5 is one op per batch, not one per derivation, because
`docs/scheduling.md` measured `--per-drv` at 7.3x slower on `g8x6` and 10x
on `g32x4`: a per-derivation op serialises what the daemon runs in
parallel, and each op costs a `Worker` plus a round trip in which nothing
builds. The worker protocol is serial per connection, so exactly one op is
in flight; `-j` reaches the daemon through `SetOptions`, where it is that
connection's `Worker` limit (daemon.cc:240) and therefore the real build
parallelism.

`--fail-fast` sets `keep-going` off on the connection and stops dispatch
at the first failed batch; the attributes left are recorded skipped.
`--retries N` re-submits the failed targets of a batch N more times.

### Measured on the e2e flake

`scripts/fastbuild-e2e.nu`'s flake: four `checks.<system>` derivations for
two systems, one of which fails and one of which depends on another.
Against the local daemon, `--systems x86_64-linux`:

| | first build | total | build ops | paths uploaded |
|---|---|---|---|---|
| cold (fresh nonce) | 1.83 s | 2.61 s | 2 | 4 |
| warm (same flake again) | 0.49 s | 0.49 s | 2 | 0 |

Time to first build is the flake fetch, the evaluation of the first
attribute, the connect and the first upload. Two build ops for four
attributes: the first op takes the attributes the walk had finished when
the connection came up, the second takes the rest, which arrived while the
first op ran.

On a 13-check flake whose builders each spin for about half a second,
`ietsp fast-build` takes 7.37 s and `nix-fast-build` 7.46 s wall on the
same machine; the builds dominate, and the point of the numbers is that
the single-process dispatch does not lose to a process per attribute.

## Results and reporting

- `--result-file` with `--result-format json|junit`, byte-compatible with
  `report.py:dump_json` and `report.py:dump_junit_xml`: an `EVAL` record
  per attribute the walk saw and a `BUILD` record per attribute
  dispatched, the failure's log tail as the JUnit `failure` body, ANSI
  escapes and XML-illegal control characters stripped.
- `--stream-json-lines`: one JSON object per record on stdout as it
  happens, the schema of `results.py:28-43`. All progress stays on stderr.
- `GITHUB_STEP_SUMMARY`, `GITEA_STEP_SUMMARY` or `FORGEJO_STEP_SUMMARY`:
  the markdown summary of `report.py:35-101`, failures first with the last
  100 log lines in a `<details>`, successes collapsed.
- Exit code 1 when any attribute failed to evaluate or build, 0 otherwise.

## Rendering

`crate::build::render`. One buffer per build, flushed as one block when
that build's activity stops, so the logs of concurrent builds never
interleave. A terminal gets one line per state change (started, phase with
elapsed, verdict) and the log only on a failure; without a terminal every
build's whole log is emitted in its block, folded with `::group::` markers
on `GITHUB_ACTIONS`/`GITEA_ACTIONS`/`FORGEJO_ACTIONS` unless `--no-fold`.
A failure prints the last 200 lines and is never folded. A heartbeat
thread names the running builds every 30 s, and a build silent for 300 s
has its last lines dumped and switches to live streaming
(`ci_renderer.py:34-197`).

The worker protocol carries no per-derivation verdict: `STDERR_STOP_ACTIVITY`
says a build ended, not whether it succeeded, and only the op's own
`BuildResult` covers the roots. The renderer therefore reads the
derivation paths out of the daemon's failure messages, which is how a
failing *dependency* gets a `✘` instead of a `✔`. Two additions to
`remote::event::Event` support this: `Phase` (the `setPhase` result) and
`Stopped`. Log lines the daemon did not attribute are folded into the
running build when there is exactly one, which is what a remote builder's
own stderr looks like coming back through a daemon.

## The wrapper switch

Numtide's `nix-farm` execs

```
nix-fast-build --store grpc://farm.numtide.com:50051?token-file=… <user flags>
```

To switch, exec `ietsp fast-build` with the same arguments. `grpc://` needs
the grpc transport (`Transport::Grpc`, in progress); until it lands, use
`--store ssh-ng://…`. Nothing else in the wrapper changes: the flags, the
result files and the exit codes are the same. A flag ietsp does not know
falls back to the real `nix-fast-build`, so the switch cannot strand a
pipeline.

## Tests

`scripts/fastbuild-e2e.nu` (32 cases). A flake with four `checks` per
system and no nixpkgs, built against the local daemon: the exit code, the
failing attribute named, the failure log tail, the dependent check built,
the JSON and JUnit result files, `--out-link`, `--fail-fast`,
`--stream-json-lines`, `--systems` with no match, every refused flag, and
the fallback on an unknown flag. Case 9 runs the same flake through the
real `nix-fast-build` and compares the JSON result file's attributes and
per-attribute outcomes, not timings; it is skipped with a note when
`nix-fast-build` is not on `PATH` (the dev shell does not ship it, so
build it with
`nix build github:Mic92/nix-fast-build/a3e9a5a17fa66f84bec8538ef8f98f0a1a9c6228`).

Unit tests cover the flag parser (`cli::fastbuild::flags`), the JSON,
JUnit and step-summary rendering (`build::results`), the renderer's
folding, log separation and verdicts (`build::render`), and the
event-to-record mapping (`build`).

# eval-jobs: whole-nixpkgs evaluation

`ietsp eval-jobs` prints the JSON lines `nix-eval-jobs` 2.35 prints, from
one process, using the fiber runtime (`docs/parallel.md`). This document
records how Hydra and nixpkgs CI evaluate nixpkgs, the exact walk and
schema of nix-eval-jobs, the measurements against it, and what is
missing.

Sources read: the pinned nixpkgs `ci/eval/{README.md,default.nix,
chunk.nix,outpaths.nix,pre-eval.nix,diff.nix}` and
`.github/workflows/eval.yml`; nix-eval-jobs 2.35.2
(`nixpkgs#nix-eval-jobs.src`, `src/nix-eval-jobs.cc`, `worker.cc`,
`drv.cc`, `response.cc`, `eval-args.cc`); Hydra
(`nixpkgs#hydra.src`, `subprojects/hydra/script/hydra-eval-jobset`);
`pkgs/top-level/release.nix`, `release-lib.nix`,
`nixos/release-combined.nix`, `nixos/release.nix`.

## How nixpkgs CI evaluates

Nixpkgs CI (`.github/workflows/eval.yml`, `ci/eval/default.nix`) does
**not** use nix-eval-jobs. It uses `nix-env`:

1. `pre-eval.nix` (single-threaded `nix-instantiate --eval --strict
   --json`) evaluates `outpaths.nix` with `attrNamesOnly = true` and lists
   every attribute path that may be a job on any platform, plus the paths
   with disallowed `meta.problems`.
2. The path list is cut into chunks of `chunkSize` (5000 default, 8000 in
   the workflow). `xargs -P$cores` runs one `nix-env` per chunk:

   ```
   nix-env -f ci/eval/chunk.nix --eval-system $system \
     --option restrict-eval true --option allow-import-from-derivation false \
     --query --available --out-path --json --meta --show-trace \
     --arg chunkSize N --arg myChunk I --arg preEvalFile F \
     --arg systems '[ "$system" ]' --arg includeBroken false \
     --argstr extraNixpkgsConfigJson '{}' -I nixpkgs -I $preEvalFile
   ```

   `chunk.nix` imports `outpaths.nix` and rebuilds the attribute tree so
   that only the chunk's paths remain, with `recurseForDerivations = true`
   on every inner set so `nix-env` recurses.
3. `outpaths.nix` imports `pkgs/top-level/release.nix` with
   `supportedSystems = systems`, `nixpkgsArgs.config = { allowAliases =
   false; allowBroken = includeBroken; allowUnfree = true;
   allowInsecurePredicate = x: true; allowVariants = !attrNamesOnly;
   checkMeta = true; problems.matchers = [broken -> error];
   handleEvalIssue = ...; inHydra = true; }` and `__allowFileset = false`,
   removes `tarball metrics manual darwin-tested unstable
   stdenvBootstrapTools moduleSystem lib-tests`, adds
   `nixosTests.{simple-container,simple-vm}` from `nixos/release.nix`,
   and `tweak`s the result: every non-derivation set gets
   `recurseForDerivations = true`. Broken and unsupported packages
   `throw` (`"broken"`, `"unsupported"`) from `handleEvalIssue`;
   `unknown-meta` and `broken-outputs` `abort`.
4. Each chunk's `--json` output is merged with `jq` into `paths.json`
   (`attr -> outputs`) and `meta.json`; `diff.nix` computes added,
   removed, changed and rebuild attrs between two commits.

A worker with warnings on stderr aborts the run (`kill $PPID`). The
README recommends 16 GB; a single chunk of 5000 paths runs at 3-5 GB
RSS under Nix's GC. A `nix-env -qa '*'` over the whole tree is the
"eval sanity" step.

## How Hydra evaluates

`hydra-eval-jobset` used to run `hydra-eval-jobs` (Hydra's own C++
evaluator). Since Hydra 2023 it execs nix-eval-jobs:

```
nix-eval-jobs --option restrict-eval true '<input/path>' \
  -I input=/nix/store/... --arg/--argstr <jobset inputs> \
  --gc-roots-dir DIR --max-jobs 1 --meta --constituents --force-recurse \
  --option allow-import-from-derivation false \
  --workers $evaluator_workers --max-memory-size $evaluator_max_memory_size
```

(flake jobsets: `--option eval-cache false --expr 'let flake =
builtins.getFlake "..."; in flake.hydraJobs or flake.checks or throw'`.)
Every job line becomes a build; `error` lines become evaluation errors
shown in the Hydra UI. `--constituents` resolves the `_hydraAggregate`
jobs (`constituents` by name or drv path) and rewrites the aggregate's
`.drv`; `--gc-roots-dir` roots each `.drv`. Hydra does not pass
`--check-cache-status` (it queries the store itself when scheduling).
The version read here (hydra flake 0.1, 2026) does not pass it either.

The nixpkgs jobsets:

- `nixpkgs/trunk`: `pkgs/top-level/release.nix` with the `nixpkgs` input
  and `supportedSystems` (`release-supported-systems.json`: aarch64-linux,
  aarch64-darwin, x86_64-linux). `release.nix` builds `jobs =
  unionOfDisjoint nonPackageJobs (mapTestOn (packagePlatforms pkgs))`:
  every package becomes `<attr>.<system>` for each of its
  `meta.hydraPlatforms` (or `platforms - badPlatforms`) that is in
  `supportedSystems`, through `hydraJob` (`scrubJobs = true`), which
  strips a derivation to `name system meta outputs drvPath outPath type
  outputName` per output and `deepSeq`s it. `nixpkgsArgs.config`:
  `allowAliases = false; allowUnfree = false; inHydra = true;
  recursionMode = "hydra"; permittedInsecurePackages = [...]`.
  `recursiveMapPackages` recurses into sets with `recurseForDerivations`
  or `recurseForRelease`; everything else becomes `[]`.
- `nixos/trunk-combined`: `nixos/release-combined.nix` with
  `supportedSystems = ["x86_64-linux" "aarch64-linux"]`,
  `limitedSupportedSystems = []`: `nixos = nixos/release.nix` (channel,
  ISOs, images, `tests = all-tests.nix` per system, closures) and
  `nixpkgs = pkgs/top-level/release.nix`, both with `maintainers`
  removed from `meta`, plus the `tested` aggregate whose `constituents`
  are job names (resolved by `--constituents`).

## The nix-eval-jobs walk (worker.cc)

The collector keeps `todo` as a `std::set` of attribute paths (JSON
arrays, so ordered lexicographically by element) starting from `[]`,
and N collector threads each drive one worker process. A worker:

1. Evaluates the root expression (file, `--expr`, or `--flake`) and
   `autoCallFunction`s it with the `--arg`/`--argstr` set.
2. For each path it is handed: `findAlongAttrPath` from the root
   (auto-calling at every step; an all-digit component is a list index,
   so `foo."1"` under an attrset is the error "the expression selected by
   the selection path 'foo.1' should be a list but is a set"), then
   `autoCallFunction` on the value: a lambda with formals is called with
   the autoArgs and defaults, a formal without either is the error
   "cannot evaluate a function that has an argument without a value".
3. If the value is not an attrset: nothing is printed (strings, lists,
   plain lambdas are ignored). Lists are never recursed.
4. If `getDerivation` succeeds (`type` forces to the string
   `"derivation"`): a job. `name` (error "derivation name missing"),
   `drvPath` coerced to a store path and required to end in `.drv`
   (missing: "derivation does not contain a 'drvPath' attribute"),
   `outputs` (list of names; each `<name>.outPath` coerced; a missing
   output attr or `outPath` is skipped; without `outputs`, `outPath`
   as `out`), then the `.drv` is read back from the store for `system`
   and `requiredSystemFeatures`, then `meta` with `--meta`.
5. Else: `collectAttrsForRecursion`: recurse when `--force-recurse`, or
   the path is empty (the root always recurses), or the set's
   `recurseForDerivations` forces to true (an error there is the parent's
   error). All attribute names are queued as children, including
   `recurseForDerivations` itself, `override`, `tests`, `passthru`: each
   child is its own job and is ignored when it is not an attrset. No
   special treatment of `tests` or `passthru`; `meta.broken` is not
   looked at (nixpkgs itself throws from `drvPath` when it is set and
   `allowBroken` is false).
6. Every `EvalError` (throw, assert, abort, type, missing attr, infinite
   recursion, max-call-depth) is an `error` line for that path;
   `StackOverflowError` (the guard-page kind) is `fatal` and stops the
   run; anything else is also an error line.
7. After each job the worker checks `getrusage` `ru_maxrss` against
   `--max-memory-size` (MB, 4096 default) and, above it, sends `restart`:
   the collector spawns a fresh worker (`GC_DONT_GC=1` is set: worker
   restarts are the garbage collection). The jobs of a restarted worker
   are re-evaluated from scratch by the new one, so the restart threshold
   bounds RSS per worker at the cost of repeated evaluation of shared
   subgraphs.

Output order is the order the collector threads finish jobs: not
deterministic across runs or workers. Compare after sorting by
`attrPath`.

## Line schema (drv.cc, response.cc)

nlohmann `json` objects are `std::map`s, so keys are byte-sorted, and
`dump()` is compact. A job:

```
{"attr":"e.f","attrPath":["e","f"],"constituents":[],"drvPath":"/nix/store/...-f.drv",
 "globConstituents":false,"meta":null,"name":"f","namedConstituents":[],
 "outputs":{"out":"/nix/store/...-f"},"requiredSystemFeatures":["big-parallel","kvm"],
 "storeDir":"/nix/store","system":"x86_64-linux"}
```

- `attr`: path elements joined with `.`, an element containing a dot
  wrapped in `"` (`attrPathJoin`).
- `constituents`, `namedConstituents`, `globConstituents`: always
  present; filled only with `--constituents` on `_hydraAggregate` jobs.
- `drvPath`, `outputs` (name -> path, sorted; `null` for a CA output),
  `name`, `storeDir`.
- `system` and `requiredSystemFeatures` (sorted; from
  `requiredSystemFeatures` in the env or structured attrs, plus
  `ca-derivations` when an output path is not known statically): from the
  `.drv` file, present whenever the store is local and not
  `--no-instantiate`.
- `meta` with `--meta`: every `meta` attribute that passes `checkMeta`
  (int, bool, string, float, lists and sets of those, no set with an
  `outPath`; so `null`, functions and paths are dropped), as
  `printValueAsJSON` (strict). `null` when there is no `meta` or nothing
  passes. A non-attrset `meta` is an error.
- `inputDrvs` with `--show-input-drvs`: drv path -> sorted output names.
- `isCached`, `cacheStatus` (`local`, `cached`, `notBuilt`),
  `neededBuilds`, `neededSubstitutes` with `--check-cache-status`.
- `extraValue` with `--apply`.

An error: `{"attr":"d","attrPath":["d"],"error":"error:\n       … while
calling the 'throw' builtin\n ...","fatal":false}`. The error text is
`showErrorInfo` with ANSI stripped, and is also printed to stderr.

## ietsp eval-jobs

```
ietsp eval-jobs [--workers N] [--force-recurse] [--meta] [--show-input-drvs]
                [--chunks M --chunk N] [--max-memory-size MB] [-I prefix=path]...
                [--arg name expr] [--argstr name str] [--option k v]
                (-E EXPR | FILE | FLAKEREF#attr)
```

Same walk, same lines. Differences and gaps:

- **Order** is deterministic: lexicographic depth-first by attribute
  name (every child of a set before the next sibling). nix-eval-jobs's
  is not; sort both by `attrPath` to compare.
- **Error text** is ietsp's (`error: msg`, `at file:line:col`, `… trace`
  lines), not Nix's rendering. The set of attrs that error is the same
  in the measurements below; the text is not compared.
- **`--max-memory-size`** is accepted and ignored: one arena, no GC, no
  worker restart. Use `--chunks M --chunk N` to split a run across
  processes: a leaf (derivation) is printed only when the FNV-1a hash of
  its attribute path is `N mod M`; inner sets are still walked and every
  leaf's `type` is still forced, so a chunk pays the whole "reach every
  package" cost and one M-th of the `drvPath` cost. nixpkgs CI chunks
  contiguous slices of a pre-evaluated path list instead; the hash keeps
  chunking deterministic without a pre-evaluation.
- **`--check-cache-status`**, `--constituents`, `--apply`, `--select`
  exit with an error. `--gc-roots-dir`, `--max-jobs`, `--flake`,
  `--show-trace`, `--no-instantiate`, `--option` are accepted and ignored
  (ietsp never writes to a store, so nothing is instantiated and no root
  is needed; `--option allow-import-from-derivation false` is the only
  behaviour: IFD is unsupported anyway).
- **`system`** comes from the in-memory `.drv` (the derivation table
  filled by `derivationStrict`), or from `/nix/store` for an imported
  `.drv`.
- **Parallelism**: the main fiber walks and prints; before walking a set
  it submits its children (in windows of 1024, 4 cells per task) as
  `T_JOB` tasks. A helper forces the child, and if it is a derivation
  forces `name`, `drvPath` (the build graph), each output's `outPath`
  and, with `--meta`, `meta` deeply; if it is a set to recurse into it
  fans out its children and walks them the same way. Helpers skip cells
  another fiber holds and continue past errors; errors are never cached,
  so the main walk re-evaluates a failing attr and reports it under its
  own boundary.

The per-line stderr summary at the end reports jobs, errors, wall time
and `VmHWM`.

## Measurements

Box: 16 vCPU, 30 GB RAM, `/nix/store` behind a local daemon
(`NIX_REMOTE=daemon`). nix-eval-jobs 2.35.2 from the flake registry
nixpkgs (`nix build nixpkgs#nix-eval-jobs -o ~/tmp/nej`); ietsp at
commit `dd45092`. `NP=/nix/store/ah3pxz4xa52hd6vhbx17vcsci0b8bmbr-source`.

How measured: every run went through a small wrapper
(`scratchpad/memrun.sh`) that runs the command under `/usr/bin/env time
-f "wall=%e user=%U sys=%S maxrss_largest=%MKB"` and, in parallel,
sums the RSS of the whole process group from `ps -eo pgid=,rss=` once a
second, keeping the maximum (and killing the group above 23 GB). For
nix-eval-jobs, `time`'s user, sys and `%M` cover the waited-for worker
processes: `%M` is the largest single process, the sampled sum is the
memory the run needed. For ietsp the two agree. Runs are single-shot,
sequential, on an otherwise idle box.

### (a) `ci/eval/outpaths.nix`, x86_64-linux, full set

```
# outpaths-x86.nix
import $NP/ci/eval/outpaths.nix { path = $NP; systems = [ "x86_64-linux" ]; includeBroken = false; }

nix-eval-jobs --meta --workers 4 --max-memory-size 4096 \
  --option allow-import-from-derivation false -I nixpkgs=$NP outpaths-x86.nix > nej.jsonl
for c in 0 1 2 3 4 5 6 7; do
  ietsp eval-jobs --meta --workers 4 --chunks 8 --chunk $c -I nixpkgs=$NP outpaths-x86.nix
done > ietsp.jsonl
scripts/evaljobs-cmp.sh nej.jsonl ietsp.jsonl [--meta]
```

No `--force-recurse`: `outpaths.nix` sets `recurseForDerivations` on
every inner set (the `nix-env` walk needs it), so the walks agree. The
full set does not fit one ietsp process (one eighth needs 11-14 GB, see
below), so ietsp ran eight chunks in sequence and the table sums their
times; peak RSS is the largest chunk.

| run | wall | user | sys | peak RSS | jobs | errors | drvPath diffs |
|---|---|---|---|---|---|---|---|
| nix-eval-jobs `--workers 4 --max-memory-size 4096` | 789.8 s | 751.0 s | 192.6 s | 16.8 GB (sum of 4 workers; largest 4.8 GB) | 80245 | 1851 | – |
| ietsp `--workers 4`, 8 chunks, summed | 110.6 s (12.4-16.0 s per chunk) | 254.2 s | 55.9 s | 14.3 GB (chunk 2; 11.3-14.3) | 80245 | 1851 | 0 |
| ietsp `--workers 8`, 8 chunks, summed | 139.6 s (15.4-19.2 s per chunk) | 411.2 s | 79.0 s | 14.5 GB | 80245 | 1851 | 0 |
| ietsp `--workers 1`, chunk 0 only | 33.1 s | 26.7 s | 6.0 s | 13.8 GB | 9244 | 167 | 0 |

Comparison (`scripts/evaljobs-cmp.sh`, keyed on `attrPath`): the same
82096 attrs on both sides, the same 80245 jobs and 1851 errors, no attr
that is a job on one side and an error on the other, zero `drvPath`
differences, and with `--meta` zero differences in the whole job line
(`jq -S`), so `outputs`, `system`, `requiredSystemFeatures` and `meta`
match byte for byte after key sorting. The one list-index quirk in the
set, `gnomeExtensions.1440`, is an error on both sides. The 1851 errors
are nixpkgs's own: 1346 `broken`, 45 `unsupported`, `tensorflow-bin:
unsupported configuration: x86_64-linux_314`, `... not supported for
interpreter python3.14`, NVIDIA and other licence throws. Error texts
differ (Nix's rendering with source excerpts versus ietsp's `error:` +
`at` + `…` lines) and are not compared.

Notes on the numbers:

- nix-eval-jobs instantiates: every `.drv` is written through the daemon
  and read back. Its workers ran at ~30 % CPU each for most of the run
  (`ps`), so a good part of its 790 s is store I/O, not evaluation.
  ietsp never writes a store. A `--no-instantiate` row is below when
  measured.
- nix-eval-jobs restarted workers throughout (each lived about a
  minute at the 4 GB threshold), which is what keeps its sum at 16.8 GB
  while it re-evaluates the shared base (stdenv, `lib`, the package
  sets) after every restart. ietsp has no ceiling inside a process: one
  chunk of one eighth of the leaves already sits at 11-14 GB because
  reaching the `hydraJob`-wrapped leaves forces their build graphs and
  nothing is ever freed. Chunking is the only memory control, and every
  chunk re-evaluates the shared base too; eight chunks cost roughly
  8 x 5 s of that.
- 8 workers are slower than 4 on this workload, as on nixos-desktop
  (`docs/parallel.md`): the extra workers spin and steal without a
  longer critical chain to shorten, and system time grows with them.
- Memory is the same at 1 and 4 workers (13.8 GB vs 13.7 GB for chunk
  0): it is the arena, not fiber stacks (300 fibers, 1.09 M tasks in the
  4-worker run per `IETSP_PAR_STATS=1`).

Two more nix-eval-jobs rows for the same set, same flags:

| run | wall | user | sys | peak RSS | jobs | errors |
|---|---|---|---|---|---|---|
| nix-eval-jobs, second run (every `.drv` already in the store) | 247.7 s | 571.2 s | 152.2 s | 17.2 GB | 80245 | 1851 |
| nix-eval-jobs `--no-instantiate` | 126.4 s | 421.4 s | 69.1 s | 17.1 GB | 80245 | 1851 |

So of the 790 s, ~540 s were first-time `.drv` writes through the
daemon, ~120 s are the store round trips that remain when the paths
are valid, and ~126 s is evaluation. Against the evaluation-only row,
ietsp's eight sequential 4-worker chunks (110.6 s wall, 254 s user) do
the same work in 0.9x the wall and 0.6x the CPU, while re-evaluating
the shared base eight times; a single process would need more than the
30 GB of this box.

### (b) `pkgs/top-level/release.nix`, x86_64-linux

```
# release-x86.nix
import $NP/pkgs/top-level/release.nix { supportedSystems = [ "x86_64-linux" ]; }

nix-eval-jobs --meta --force-recurse --workers 4 --max-memory-size 4096 \
  --option allow-import-from-derivation false -I nixpkgs=$NP release-x86.nix
for c in 0..7: ietsp eval-jobs --meta --force-recurse --workers 4 --chunks 8 --chunk $c -I nixpkgs=$NP release-x86.nix
```

`--force-recurse` as Hydra passes it: `release.nix` leaves are
`<pkg>.<system>` sets without `recurseForDerivations`, so without it
both tools print only the handful of top-level derivations (`tarball`,
`manual`, `metrics`, `release-checks`, `unstable`) and the sets that
carry the flag: 586 lines from ietsp in 1-3 s per chunk, 23 s for
nix-eval-jobs. `release-lib.nix` has no `limitedSupportedSystems`
argument (that is `release-combined.nix`); `supportedSystems` alone
restricts the jobs. `nixpkgs` is the default `cleanSource ../..`.

| run | wall | user | sys | peak RSS | jobs | errors | diffs |
|---|---|---|---|---|---|---|---|
| nix-eval-jobs `--workers 4 --max-memory-size 4096 --force-recurse` (`.drv`s mostly in the store from (a)) | 234.0 s | 531.4 s | 144.7 s | 17.0 GB (largest 4.7 GB) | 77708 | 4402 | – |
| ietsp `--workers 4`, 8 chunks, summed | 93.7 s (10.2-14.1 s per chunk) | 237.4 s | 41.4 s | 13.9 GB (10.7-13.9) | 77708 | 4402 | 0 |

Same 82110 attrs, same 77708 jobs and 4402 errors (`allowUnfree =
false` here, so every unfree package is an error), zero `drvPath` and
zero whole-line (`--meta`) differences. The full set includes
`tarball`, `manual`, `metrics`, `release-checks`, `unstable` (the
aggregate; `constituents` are not resolved by either side without
`--constituents`), `stdenvBootstrapTools.*`, `tests.*`, `lib-tests`,
`pkgsLLVM/pkgsArocc/pkgsZig/pkgsMusl/pkgsStatic.stdenv`, `pkgsRocm`,
`haskell.packages.ghc*.haskell-language-server`.

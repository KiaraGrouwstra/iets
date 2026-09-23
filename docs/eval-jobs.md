# eval-jobs: whole-nixpkgs evaluation

`ietsp eval-jobs` prints the JSON lines `nix-eval-jobs` 2.35 prints, from
worker processes of fibers (`docs/parallel.md`), restarted at
`--max-memory-size` as nix-eval-jobs restarts its own. This document
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
   the selection path 'foo.1' should be a list but is a set". ietsp
   departs here: under an attrset the component names the attribute, so
   a `listToAttrs` jobset keyed by numbers evaluates), then
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
ietsp eval-jobs [--workers N] [--force-recurse] [--meta] [--provenance]
                [--show-input-drvs]
                [--no-instantiate] [--eval-store URL] [--gc-roots-dir DIR]
                [--check-cache-status] [--constituents] [--apply EXPR]
                [--select EXPR] [--max-memory-size MB]
                [--chunks M --chunk N [--chunk-depth D]]
                [-I prefix=path]... [--arg name expr] [--argstr name str]
                [--arg-from-file name path] [--arg-from-stdin name]
                [--option k v]... (-E EXPR | FILE | --flake FLAKEREF[#attr])
```

Same walk, same lines. `scripts/evaljobs-flags.nu` runs nix-eval-jobs
2.35.2 and ietsp with every flag combination below on the jobsets in
`tests/evaljobs/` and diffs the sorted output (`jq -S` per line, error
text replaced by `true`, GC root directories listed): 41 cases, all
identical, exit codes included.

### Flags

| flag | status | notes |
|---|---|---|
| `FILE`, `-E`/`--expr EXPR` | identical | The argument is a file unless `--flake`; the top level is auto-called with the `--arg`s like `releaseExprTopLevelValue`. |
| `--arg`, `--argstr`, `--arg-from-file`, `--arg-from-stdin` | identical | |
| `-I`, `--include` | identical | |
| `--flake REF[#attr]` | identical for locked local flakes | `REF` is a directory (`.`, `./x`, `path:/x`); the fragment is looked up in `outputs` verbatim. Without a fragment the root is the `callFlake` result (`_type`, `inputs`, `outputs`, `sourceInfo`), to be `--select`ed like nix-eval-jobs. Registries and unlocked inputs are out of scope (`docs/flakes.md`); evaluation is pure unless `--impure` (`docs/pure-eval.md`). |
| `--force-recurse`, `--meta`, `--show-input-drvs` | identical | |
| instantiation (the default) | identical result | nix-eval-jobs writes every `.drv` inside `derivationStrict`; ietsp registers the job drvs and their closures in batches on a thread beside the walk (below). |
| `--no-instantiate` | identical | `system` from the `system` attribute (`unknown` without one), no `requiredSystemFeatures`, no `inputDrvs`, no roots; refused together with `--show-input-drvs`, `--check-cache-status` or `--constituents`, same message. |
| `--eval-store URL` | supported | `daemon`, `unix:///path`, `ssh-ng://host`. Cache status and constituents read the `.drv`s back from `/nix/store`, so they need the local daemon, as nix-eval-jobs needs a `LocalFSStore`. |
| `--gc-roots-dir DIR` | identical | `DIR/<drv basename>` symlink plus `AddIndirectRoot` for every job (and every rewritten aggregate), skipped when the link exists; nothing with `--no-instantiate`. ietsp creates `DIR`; nix-eval-jobs errors per job when it is missing. |
| `--check-cache-status` | identical | `isCached`, `cacheStatus`, `neededBuilds`, `neededSubstitutes` from the cache-status-resolver.cc algorithm (below). |
| `--constituents` | identical | Named, derivation, glob (`_hydraGlobConstituents`) constituents, dependency cycles, missing and failed constituents, rewritten `drvPath`/`outputs`, `namedConstituents` erased, roots. Not across `--chunks`: a name in another chunk is "does not exist". |
| `--apply EXPR` | identical | `extraValue`; a non-attrset result is the job's error. |
| `--select EXPR` | identical | Applied to the auto-called root; a non-attrset result is fatal. |
| `--workers N` | different meaning | Worker processes (default 4; nix-eval-jobs: 1), or fibers of one process with `--max-memory-size 0`. |
| `--max-memory-size MB` | identical default | Worker processes restarted above `MB`, 4096 like nix-eval-jobs (below). `0` is ietsp's own: one process with no ceiling, which no jobset larger than the box can finish. |
| `--chunks M --chunk N`, `--chunk-depth D` | ietsp only | See above. |
| `--impure` | identical | Turns off the pure evaluation `--flake` implies (`docs/pure-eval.md`). |
| `--show-trace` | accepted | The trace is always in the error text; with `--log-format internal-json` it also goes into the error records, as in Nix. |
| `--option k v` | accepted, ignored | `restrict-eval`, `allow-import-from-derivation` (IFD is unsupported anyway), `eval-cache`, substituters: none takes effect. |
| `--quiet`, `--verbose`, `--debug` | accepted | `--quiet` drops the stderr summary. |
| `--log-format F` | accepted | `internal-json`: stderr is Nix's `@nix {...}` records, with a `code` (and a `hint`) on each error (`docs/cli.md`). `raw`, `raw-with-logs`, `bar`, `bar-with-logs`: plain lines. |
| `--max-jobs N` | accepted, ignored | |
| `--provenance` | ietsp only | Adds a `pos` key per job: `file:line:col` of the derivation, mapped back to the local tree (`docs/provenance.md`). nix-eval-jobs 2.35 has no such key, so the flag is off by default and the 41 cases stay identical. |
| `--override-input`, `--override-flake`, `--reference-lock-file` | error | ietsp evaluates a flake from its own complete `flake.lock` only. |
| `--repair` | error | ietsp never repairs store paths. |

Differences that stay:

- **Order** is completion order, like nix-eval-jobs, and lexicographic
  depth-first with `--max-memory-size 0`. Sort by `attrPath` to compare.
- **Error text** is ietsp's (`error: msg`, `at file:line:col`, `… trace`
  lines), not Nix's rendering. The set of attrs that error is the same
  in every measurement; the text is not compared. A fatal line
  (`StackOverflow`) ends the run with exit 1 on both sides.
- **`system`** comes from the in-memory `.drv` (the derivation table
  filled by `derivationStrict`), or from `/nix/store` for an imported
  `.drv`; nix-eval-jobs reads it back from the store after writing.

### Instantiation

`src/jobs/instantiate.rs`. The walk hands every job or error line to a
thread that holds one daemon connection. Every 256 jobs (or at the end)
it computes the closure of the batch's `drvPath`s from the evaluator's
derivation table (ATerm and references; an imported `.drv` is read from
`/nix/store`) and the sources they reference (origins in `ev.known`: local trees
and `toFile` texts, hashed on the way), stops at paths already known
valid, asks `QueryValidPaths` once, sends what is missing in one
`AddMultipleToStore` (sources first, drvs in post-order, the same
`ValidPathInfo` + NAR framing as `ietsp build`), registers the GC roots,
and only then passes the batch's lines on. So a printed `drvPath` is
always valid in the store, as after nix-eval-jobs, and the daemon round
trips overlap the evaluation. The stderr summary gets an `instantiate:`
line with batches, paths queried and added, bytes, roots and thread
time.

Cost, same box and method as the measurements below (`--workers 4`):

| workload | `--no-instantiate` | instantiate, warm store | instantiate, cold store (scratch `nix-daemon --store local?root=DIR`) |
|---|---|---|---|
| nixos-desktop toplevel (1 job, 4184-path closure, 3533 drvs) | 1.04 s | 1.17 s (1 batch, 4184 queried, 0.13 s on the thread) | 3.09 s (4184 added, 38 MB, 2.09 s on the thread) |
| outpaths x86_64-linux chunk 0 of 8 (9244 jobs, 167 errors) | 13.3 s | 13.6 s (37 batches, 81721 queried, 2.7 s on the thread) | 54.7 s (81721 added, 632 MB, 51.0 s on the thread; 79074 drvs in the scratch store) |

A warm store costs 0.3 s of wall on a 13 s chunk: the queries overlap
the walk. A cold store costs 0.6 ms per path in the daemon
(`AddMultipleToStore` registers one path per SQLite transaction) and
the walk finishes long before the thread does, so the run is bound by
the daemon's write rate: 41 s for 82k paths, against nix-eval-jobs's
~540 s of first-time writes for the whole set (790 s cold vs 248 s
warm, one `writeDerivation` round trip per drv from four workers). The
same nixos-desktop job takes nix-eval-jobs 5.3 s warm (one worker).

### Cache status

`src/jobs/cache.rs` mirrors cache-status-resolver.cc (2.35 no longer
calls `queryMissing`; it probes with `queryValidPaths` and
`querySubstitutablePathInfos` and walks the input drvs itself): every
output of the job is probed for validity; all valid is `local`; the
missing ones all substitutable is `cached` with them as
`neededSubstitutes`; otherwise the input graph is walked from the job's
`.drv` (read back from `/nix/store`, cached), a drv whose wanted outputs
are missing and not all substitutable is a needed build (post-order,
`inputDrvs` in store-path order) and its inputs are visited, one whose
missing outputs are substitutable contributes them to
`neededSubstitutes` and is not descended. Probes are batched: one
`QueryValidPaths` and one `QuerySubstitutablePathInfos` (worker op 30,
added to `src/remote/worker/mod.rs`) per pass over the batch of jobs
that is still unresolved; a failing substituter query counts as a miss.
`neededSubstitutes` is sorted by name then path, like the C++. In one
process the resolver runs on the instantiation thread after each batch;
with `--max-memory-size` on its own thread fed by the collectors.

### Constituents

`src/jobs/constituents.rs` mirrors constituents.cc. At job time (with
the flag) `_hydraAggregate = true` jobs yield `constituents` (the
`.drv`s in the string context of `constituents`, sorted) and
`namedConstituents` (its strings); an aggregate with names is withheld
by the emitter. After the walk every withheld aggregate is resolved
against the job table: a missing name is `does not exist` (or, with
`_hydraGlobConstituents`, an fnmatch pattern over every other job,
`constituent glob pattern had no matches` when nothing matches), an
error line is `<its error text>`; aggregates are topologically sorted
on the names that are aggregates themselves, and a cycle prints the two
jobs with `Dependency cycle: a <-> b` and the rest with `Skipping
aggregate because of a dependency cycle`. An aggregate with no broken
constituent gets each constituent's `.drv` as an `inputDrvs` entry with
the child's first output, its input-addressed outputs reset to
deferred and recomputed (`finalize_input_addressed_outputs`, hashes
read from `/nix/store`), the new `.drv` written through the daemon and
rooted, `drvPath` and `outputs` replaced; `namedConstituents` is erased
and a broken one becomes `error: "name: why\n..."` on the job line.
`tests/evaljobs/aggregates.nix` has all of nix-eval-jobs's cases
(direct, indirect, mixed, nested, missing, failed, two glob shapes, a
cycle with a bystander, `_hydraAggregate` without constituents,
`_hydraAggregate = false`): 23 lines, identical to nix-eval-jobs with
worker restarts, with `--max-memory-size 0` and with
`--check-cache-status`.

### Memory limit

`src/jobs/collector.rs`. ietsp has one arena and no GC, so a process
cannot shed what it has evaluated; the only ceiling is a process
boundary. `--max-memory-size MB` is therefore 4096 by default, like
nix-eval-jobs (`eval-args.hh:18`), and ietsp does what nix-eval-jobs
does: the main process runs `--workers` collector threads, each with a
worker process (`ietsp eval-jobs --worker` and the same arguments, one
fiber thread, its own instantiation thread) and the shared `todo` set
of attribute paths starting at `[]`; a worker gets `do ["a","b"]`,
finds the value along the path (auto-calling at every step), and
answers `{"attrs":[...]}` for a set to recurse into, `ok` for a skipped
node or a job (the job line follows once its instantiation batch is
registered, so lines arrive at any time), or an error line; after every
answer it compares `VmHWM` with the limit and, above it, flushes its
batch, says `restart` and exits, and the collector spawns a new one. A
worker that dies is reported like nix-eval-jobs's
`handleBrokenWorkerPipe`. Cache status and constituents run in the main
process from the lines (the `.drv`s are in the store by then). Output
order is completion order. The chunk rule still applies inside a
worker, so `--chunks` and `--max-memory-size` combine.

The default is what `nixos/release-small.nix --force-recurse --workers 4`
needs: 60 NixOS VM tests in one arena pass 30 GB and the kernel kills
the process, which is what ietsp did while `--max-memory-size` was off
by default. With the restarts it prints the same 123 lines as
nix-eval-jobs (120 jobs, 3 errors, same `drvPath`s and outputs) in
47.9 s against 132.8 s, over 16 worker processes. `bench/eval-jobs.nu`
compares that pair on every run.

Full outpaths x86_64-linux set, `--workers 4 --max-memory-size 4096`,
against the nix-eval-jobs rows of section (a), same box and method
(`--meta`, `-I nixpkgs=$NP`, one run each unless noted):

| run | wall | user | sys | peak RSS (group; largest process) | worker processes | jobs | errors | diffs |
|---|---|---|---|---|---|---|---|---|
| ietsp `--max-memory-size 4096`, warm store (two runs) | 72.4 s, 83.1 s | 243 s, 251 s | 53 s, 81 s | 16.1 GB; 4.9 GB | 28 (4 + 24 restarts) | 80245 | 1851 | 0 |
| ietsp `--max-memory-size 4096 --no-instantiate` | 70.3 s | 229 s | 49 s | 15.9 GB; 4.7 GB | – | 80245 | 1851 | 0 |
| ietsp `--max-memory-size 4096`, cold scratch store | 206.5 s | 228 s | 76 s | 17.0 GB; 4.7 GB | 27 | 80245 | 1851 | 0 (243545 drvs written, 2.0 GB) |
| nix-eval-jobs `--workers 4 --max-memory-size 4096`, warm store, same day | 245.1 s | 564 s | 152 s | 17.1 GB; 4.6 GB | – | 80245 | 1851 | – |
| nix-eval-jobs, section (a): cold, warm, `--no-instantiate` | 789.8 s, 247.7 s, 126.4 s | | | | | 80245 | 1851 | – |

Diffs are `scripts/evaljobs-cmp.nu --meta` against the same-day
nix-eval-jobs run: the same 82096 attrs, the same jobs and errors, zero
whole-line differences after key sorting, for all three ietsp runs.
Restart mode is 3.4x faster than nix-eval-jobs on a warm store and 3.8x
on a cold one, with the same memory envelope (four workers under 4 GB
each plus the collector), and the same fixed cost per restart: every
new worker re-evaluates the shared base before its first job, which is
what the 28 processes buy. The cold run is bound by the daemon's write
rate (one path per SQLite transaction on each worker's connection):
+134 s over warm, against nix-eval-jobs's +542 s. The single-process
mode of section (a) (eight `--chunks` in sequence, 110.6 s summed, 14 GB)
remains the option without a memory ceiling.

## Measurements

Box: 16 vCPU, 30 GB RAM, `/nix/store` behind a local daemon
(`NIX_REMOTE=daemon`). nix-eval-jobs 2.35.2 from the flake registry
nixpkgs (`nix build nixpkgs#nix-eval-jobs -o ~/tmp/nej`); ietsp at
commit `dd45092`. `NP=/nix/store/ah3pxz4xa52hd6vhbx17vcsci0b8bmbr-source`. The ietsp
rows of (a)-(c) predate instantiation: they are `--no-instantiate` runs
in one process.

How measured: every run went through a small wrapper
(not in the repository) that runs the command under `/usr/bin/env time
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
scripts/evaljobs-cmp.nu nej.jsonl ietsp.jsonl [--meta]
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

Comparison (`scripts/evaljobs-cmp.nu`, keyed on `attrPath`): the same
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

### (c) `nixos/release-combined.nix`: the `tested` subset

The whole `nixos.tests` set (1376 names, more with nested groups) does
not fit: one NixOS test costs ietsp about 1 GB (`tests.login`: 0.9 s,
967 MB at 4 workers) and a chunk cannot skip a test above its chunk
depth without forcing it, so a probe with `--chunks 32 --chunk-depth 4`
evaluated every depth-3 test in every chunk and was killed at 23 GB
after 9 lines. What was run instead is the set of tests the `tested`
aggregate names on x86_64-linux (116 tests) plus `tested` itself,
flattened to one depth-2 unit per test:

```
# nixos-tested-flat-x86.nix
let np = $NP; lib = import (np + "/lib");
    rc = import (np + "/nixos/release-combined.nix") { supportedSystems = [ "x86_64-linux" ]; limitedSupportedSystems = [ ]; };
    names = lib.filter (lib.hasPrefix "nixos.tests.") rc.tested.constituents;
    paths = map (n: lib.drop 2 (lib.splitString "." n)) names;
    flat = lib.listToAttrs (map (p: lib.nameValuePair (lib.concatStringsSep "-" (lib.init p))
             { ${lib.last p} = lib.getAttrFromPath p rc.nixos.tests; }) paths);
in { inherit (rc) tested; tests = flat; }

nix-eval-jobs --meta --force-recurse --workers 2 --max-memory-size 4096 \
  --option allow-import-from-derivation false -I nixpkgs=$NP nixos-tested-flat-x86.nix
for c in 0..15: ietsp eval-jobs --meta --force-recurse --workers 4 --chunks 16 --chunk $c -I nixpkgs=$NP nixos-tested-flat-x86.nix
```

| run | wall | user | sys | peak RSS | jobs | errors | diffs |
|---|---|---|---|---|---|---|---|
| nix-eval-jobs `--workers 4 --max-memory-size 4096 --force-recurse` | killed at 23 GB after 270 s | – | – | > 23 GB | 116 of 117 | 0 | – |
| nix-eval-jobs `--workers 2`, same | killed at 23 GB after 474 s | – | – | > 23 GB | 116 of 117 | 0 | – |
| ietsp `--workers 4`, 16 chunks | 15 chunks: 79.4 s (3.2-10.1 s each); chunk 2 killed at 24 GB after 13 s | 230 s | 43 s | 14.1 GB | 116 of 117 | 0 | 0 |
| ietsp, chunk 2 re-split into 4 of 64 | 3 finish in 0.9-2.0 s; the one holding `switchTest` killed at 23 GB after 12 s | | | > 23 GB | | | |

The 116 jobs both sides finished are the same attrs with zero
`drvPath` and zero whole-line differences. The 117th,
`tests.switchTest.x86_64-linux`, is the missing one on both sides: it
alone needs more than 23 GB in ietsp (its chunk held nothing else once
re-split) and pushed nix-eval-jobs's process group over 23 GB with
either worker count, since its workers run with `GC_DONT_GC=1` and a
single job is not bounded by `--max-memory-size`. It is the NixOS
`switch-to-configuration` test with many system generations; a box
with more memory would finish it on both. The `tested` aggregate itself
is a plain job on both sides (`namedConstituents: []` without
`--constituents`).

## Summary

| workload | nix-eval-jobs wall | ietsp wall (4 workers, chunks summed) | ietsp peak RSS | jobs | errors | diffs |
|---|---|---|---|---|---|---|
| (a) outpaths x86_64-linux | 789.8 s cold store, 247.7 s warm, 126.4 s `--no-instantiate` | 110.6 s (`--no-instantiate`, 8 chunks) | 14.3 GB | 80245 | 1851 | 0 |
| (a) with `--max-memory-size 4096`, 4 worker processes | same | 72.4 s warm, 206.5 s cold, 70.3 s `--no-instantiate` | 16-17 GB group, 4.9 GB largest | 80245 | 1851 | 0 |
| (b) release.nix x86_64-linux | 234.0 s (warm) | 93.7 s | 13.9 GB | 77708 | 4402 | 0 |
| (c) nixos `tested` subset | killed (switchTest) | 79.4 s + switchTest killed | 14.1 GB | 116 | 0 | 0 |

## What is missing for Hydra or nixpkgs CI

- **Memory ceiling** (`--max-memory-size`) costs a re-evaluation of the
  shared base per restart, as in nix-eval-jobs, and has no in-process
  form: a single job that needs more than the box (`nixos.tests.switchTest`)
  kills both. A GC, or a per-unit arena the shared thunk graph does not
  allow, is the real fix.
- **Chunk depth.** A chunk decides by the hash of the path prefix of
  length `--chunk-depth`; a derivation above that depth has to be forced
  (`hydraJob` deep-forces it) before it can be skipped, so the depth
  must be at or below the shallowest leaf: 2 for `release.nix` and
  `outpaths.nix`, 3 for `release-combined.nix`. nixpkgs CI's
  pre-evaluated path list plus `chunk.nix` pruning is the general
  answer; ietsp could take such a list (`--attr-paths FILE`) and skip
  everything else without forcing. `--constituents` cannot resolve a
  name in another chunk.
- **Error text** differs; the set of failing attrs does not. Hydra
  shows the text in its UI, so Nix's rendering (source excerpts, the
  `… while` chain) would have to be copied for parity.
- **Flakes**: locked local flakes only. `--override-input`,
  `--override-flake`, `--reference-lock-file` error; registries and
  `github:` references are not fetched. `--flake` switches to pure
  evaluation unless `--impure` is given, as in nix-eval-jobs
  (`docs/pure-eval.md`). `--option restrict-eval true` (Hydra) is ignored:
  ietsp reads any path.
- **`--eval-store`** other than the local daemon: instantiation works
  over `ssh-ng://`, but `--check-cache-status` and `--constituents` read
  `.drv`s from `/nix/store` and need the local one, like nix-eval-jobs's
  `LocalFSStore` requirement.
- **`--log-format internal-json`** gives ietsp's plain text in `msg`
  records. The activity records a daemon sends (`start`, `stop`,
  `result`) are forwarded (`docs/cli.md`).

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

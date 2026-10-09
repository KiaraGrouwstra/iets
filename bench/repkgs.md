# repkgs: iets eval-jobs against nix-eval-jobs

Measured on 2026-10-09. This page records one comparison and its
conditions. It is not a standing claim: run it again before you quote it.

## Result

On the repkgs CI jobset, iets evaluates the same 1071 jobs as
nix-eval-jobs, with identical `drvPath` and `outputs`. It uses 2.2× to
4.1× fewer user instructions and is 2.3× to 4.1× faster in wall time. The
advantage does not disappear at one worker: there it is 2.2× in
instructions and 2.5× in wall time with a cold IR cache. It is smallest
on the nixpkgs part of the jobset (2.0× in instructions), and the
absolute gain is small: 0.9 s to 1.5 s of wall time per CI
evaluation. On the bare package set it is 0.15 s.

## Pins

| what | value |
|---|---|
| machine | bld1.ztm.io, AMD Ryzen 9 7950X3D (16 cores, 32 threads), 125 GiB, Linux 6.18.45, idle |
| repkgs | github:Mic92/repkgs `fab7121` (upstream `main`) |
| nixpkgs, from the repkgs `flake.lock` | `02f5696b0e6097e589076d886b317b83ff0437d7` |
| iets | `dcc33a0b`, `/nix/store/vjxyxb2vc3zfji5ymx8k9lid49z7vqjk-iets-0.1.0` |
| nix-eval-jobs | 2.35.2 from the iets flake's nixpkgs (`ef34387d`), `/nix/store/zh4g79k1kr2k12wada86rzz347naxf4q-nix-eval-jobs-2.35.2`, links libnixexpr 2.35.2 |
| nix-daemon | bld1's system nix 2.35.2+1, `experimental-features` includes `ca-derivations dynamic-derivations` |
| perf | 7.2.5 |

nix-eval-jobs is pinned to 2.35.2 on purpose. 2.35.4 changes the JSON
lines, so a diff against it shows differences that are not real.

The daemon's deriver is not in the store, so this page cannot say which
source built it. Neither evaluator needs Mic92's fork of nix to
*evaluate* repkgs: the stock 2.35.2 libraries evaluate every job. The fork
and `builder-rpc-v0` matter only to build the set.

## Workloads

- **ci**: `repkgs/nix/checks.nix`, what CI evaluates. 1071 jobs: the set
  for six platforms, `tests-*`, `builder-*`, `treefmt` and `seed`.
- **set**: `repkgs/default.nix`, the package set for x86_64-linux. 199
  jobs. It does not import nixpkgs.

One job in each fails in both tools with the same `throw`:
`x86_64-windows-gnu-openblas` (ci) and `libiconv` (set). Every run of the
tables below exited 0 with these counts.

## Commands

```
NIX_REMOTE=daemon nix-eval-jobs --workers W [--no-instantiate] --impure repkgs/nix/checks.nix
IETS_CACHE_DIR=C iets eval-jobs --workers W --max-memory-size M [--no-instantiate] --impure repkgs/nix/checks.nix
```

Each run is wrapped in `perf stat -x, -e instructions:u,instructions:k,task-clock`.
Wall time is `$EPOCHREALTIME` around it. 20 rounds; each round runs every
row once, in a fixed order, so drift spreads over all rows.

- **W**: 1, 4, 16. Both tools use one worker process per worker.
- **M**: 4096, the default of both tools. `M = 0` (row `iets, 1 proc`) is
  iets's own mode: one process, W fibers, so nixpkgs is imported once. Nix
  has no such mode.
- **cold**: `rm -rf C/ir` before the run. **warm**: the next run, same
  cache. `eval-jobs` uses the IR cache only, not the tracing cache.
  nix-eval-jobs has no evaluation cache for a file argument, so it has one
  row; the page cache is warm for both tools.
- `NIX_REMOTE=daemon` makes nix-eval-jobs write through the daemon as iets
  does. As root it would otherwise open the store directly, and count the
  store work as its own.

## ci, instantiating

Instructions are user space, median of 20, in G, with the coefficient of
variation. Wall is the median, with the min and max of 20.

| tool | W | cache | instructions | wall (s) |
|---|---:|---|---:|---:|
| nix-eval-jobs | 1 | — | 17.80 (0.00 %) | 2.240 (2.157–2.396) |
| iets | 1 | cold | 8.10 (0.02 %) | 0.892 (0.876–0.924) |
| iets | 1 | warm | 6.58 (0.01 %) | 0.722 (0.714–0.744) |
| iets, 1 proc | 1 | cold | 8.02 (0.02 %) | 0.850 (0.837–0.879) |
| iets, 1 proc | 1 | warm | 6.50 (0.01 %) | 0.690 (0.670–0.746) |
| nix-eval-jobs | 4 | — | 29.16 (0.50 %) | 1.598 (1.569–1.668) |
| iets | 4 | cold | 11.25 (0.57 %) | 0.669 (0.658–0.689) |
| iets | 4 | warm | 9.66 (0.23 %) | 0.507 (0.499–0.524) |
| iets, 1 proc | 4 | cold | 8.62 (0.16 %) | 0.466 (0.451–0.479) |
| iets, 1 proc | 4 | warm | 7.13 (0.17 %) | 0.394 (0.372–0.412) |
| nix-eval-jobs | 16 | — | 72.05 (0.94 %) | 1.709 (1.658–1.755) |
| iets | 16 | cold | 21.71 (1.04 %) | 0.742 (0.708–0.760) |
| iets | 16 | warm | 20.10 (1.40 %) | 0.585 (0.575–0.600) |
| iets, 1 proc | 16 | cold | 9.54 (0.56 %) | 0.501 (0.484–0.518) |
| iets, 1 proc | 16 | warm | 7.87 (0.37 %) | 0.427 (0.401–0.464) |

Ratios, nix-eval-jobs over iets at the same W:

| W | iets row | instructions | wall |
|---:|---|---:|---:|
| 1 | cold | 2.20× | 2.51× |
| 1 | warm | 2.70× | 3.10× |
| 4 | cold | 2.59× | 2.39× |
| 4 | 1 proc, cold | 3.38× | 3.43× |
| 4 | 1 proc, warm | 4.09× | 4.06× |
| 16 | cold | 3.32× | 2.30× |

nix-eval-jobs is fastest at 4 workers and slower at 16: every worker
imports nixpkgs and evaluates the root again, and the two nixpkgs jobs are
one long chain each. iets with worker processes has the same cost (21.7 G
at 16). One process removes it.

## ci, not instantiating

With `--no-instantiate` the two tools do not complete the same work. On a
store that did not hold the repkgs `.drv` files (the first run on bld1,
before any instantiating run):

- nix-eval-jobs fails at the root of `checks.nix`, with no job:
  `path '/nix/store/1mx8nm4n6kxlxjrqhzxpppwvq75h1f2f-7zip.drv' is not valid`.
- On `default.nix` it completes 165 of 199 jobs. 32 fail with
  `path '…-cargo-vendor.drv.drv' is not a valid store path` and the like:
  each reads back, through `builtins.outputOf`, a producer `.drv` that
  `--no-instantiate` did not write. `windows-sdk` fails on `7zip.drv`,
  which it reads through `sevenzip.drvPath`. The 165 match iets exactly.
- iets completes all 1071 and all 199. It answers `outputOf` from the
  derivation it holds in memory (`docs/reference/divergences.md`).

After one instantiating run the `.drv` files are in the store, and
nix-eval-jobs `--no-instantiate` then completes all 1071 jobs. So its
result depends on the store state, and the rows below are measured on the
store that holds them. iets's limit is the other way round: it fails on a
producer it did not make in this run and that is absent from
`/nix/store`. repkgs has no such producer.

| tool | W | cache | instructions | wall (s) |
|---|---:|---|---:|---:|
| nix-eval-jobs | 1 | — | 16.78 (0.00 %) | 1.687 (1.666–1.784) |
| iets | 1 | cold | 7.64 (0.02 %) | 0.824 (0.819–0.863) |
| nix-eval-jobs | 4 | — | 27.25 (0.10 %) | 1.339 (1.308–1.377) |
| iets | 4 | cold | 10.60 (0.53 %) | 0.631 (0.620–0.643) |
| iets, 1 proc | 4 | cold | 8.13 (0.17 %) | 0.436 (0.421–0.451) |

To instantiate costs nix-eval-jobs 0.55 s of wall at one worker and iets
0.07 s, with every `.drv` already valid. The daemon's instructions are not
in either count.

## set, instantiating

| tool | W | cache | instructions | wall (s) |
|---|---:|---|---:|---:|
| nix-eval-jobs | 1 | — | 1.000 (0.00 %) | 0.226 (0.217–0.249) |
| iets | 1 | cold | 0.337 (0.01 %) | 0.072 (0.069–0.077) |
| nix-eval-jobs | 4 | — | 1.916 (5.14 %) | 0.225 (0.218–0.294) |
| iets | 4 | cold | 0.675 (1.11 %) | 0.058 (0.056–0.062) |
| iets, 1 proc | 4 | cold | 0.351 (0.40 %) | 0.050 (0.047–0.053) |

2.8× to 5.5× fewer instructions and 3.1× to 4.5× faster. The whole set
takes less than a quarter of a second in both tools, so process start and
the daemon are a large part of these numbers.

## How much of ci is repkgs

`--select` evaluates part of the jobset at one worker, in one process for
iets, with a cold IR cache. Each part includes the root, so the root is
taken out of the parts. 10 runs each; the coefficient of variation is
under 0.05 %.

| part | nix-eval-jobs | iets | ratio |
|---|---:|---:|---:|
| root: `lib` from nixpkgs, the set's attribute names for six platforms | 3.22 G (18 %) | 0.80 G (10 %) | 4.0× |
| `treefmt` and `seed`, from nixpkgs | 10.76 G (60 %) | 5.37 G (67 %) | 2.0× |
| the 1069 repkgs jobs | 3.82 G (21 %) | 1.85 G (23 %) | 2.1× |
| all | 17.80 G | 8.02 G | 2.2× |

`treefmt` alone is 6.40 G in nix-eval-jobs, 36 % of the run. It pulls
`pkgs.nixfmt` (1.5.0, built with GHC 9.10.3) and with it the Haskell
package set. So about three fifths of "repkgs CI evaluation" is nixpkgs,
and the CI ratio is mostly a nixpkgs ratio. The repkgs jobs alone give
almost the same ratio, 2.1×.

## Caveats

- Instructions are those of the client processes. The daemon's work, which
  is in the wall time of the instantiating rows, is not counted.
- The instantiating rows are steady state: every `.drv` was already valid.
  A first evaluation that writes new `.drv` files was not timed.
- The nixpkgs tarball was already fetched for both tools. No row includes
  a download.
- A warm IR cache is an iets feature with no Nix equivalent. The cold rows
  are the evaluator comparison; the warm rows are what a second CI run on
  the same machine gets.
- One machine, one revision of each input. The jobs are short, so a fixed
  cost such as process start weighs more than on a NixOS evaluation.

# The static build

`nix build .#static` builds a musl binary with no runtime dependencies: no
`/nix/store`, no daemon, no shared libraries, no CA bundle. It can
evaluate, fetch `github:`, `gitlab:`, `sourcehut:` and `tarball` inputs,
and build on a `grpc://` builder. Stripped and compressed with `xz -9`, it
is 3.1 MB.

It still runs `git` for a project that is a git checkout, and `nix` for a
locked `git` or `hg` input.

With no CA bundle on the machine, it trusts the Mozilla roots compiled in
(`NIX_SSL_CERT_FILE` in [reference/settings.md](../reference/settings.md)).

## The allocator

Outside the evaluator's arena, heap memory comes from the global
allocator. musl's malloc made the static build 24–30% slower than the
glibc build, and 79% slower on desktop w4.

Each allocator was timed with hyperfine (1 warmup, 5 runs, median) on the
`bench/ab.nu` workloads, on an idle Ryzen 9 7950X3D pinned to cores 0–7.
The baseline is glibc's malloc.

| variant | desktop w1 | desktop w4 | minimal w1 | outpaths 0/8 w1 | outpaths 0/8 w4 |
|---|---:|---:|---:|---:|---:|
| glibc, system | 0.97 s | 0.47 s | 0.74 s | 24.4 s | 9.05 s |
| glibc, mimalloc | −7% | −7% | −8% | −8% | −7% |
| glibc, jemalloc | −4% | −4% | −5% | −7% | −7% |
| glibc, tcmalloc | −3% | −4% | −4% | −4% | −5% |
| glibc, rpmalloc | −3% | −2% | −3% | −6% | −7% |
| glibc, snmalloc | −2% | −1% | −2% | −2% | −6% |
| glibc, scudo | +5% | +3% | +4% | +4% | −1% |
| glibc, dlmalloc | +3% | +83% | +3% | +2% | +1% |
| glibc, rlsf | +4% | +93% | +4% | +3% | +1% |
| musl, mimalloc | +3% | +5% | 0% | +3% | +3% |
| musl, jemalloc | +7% | +10% | +5% | +4% | +4% |
| musl, dlmalloc | +12% | +70% | +10% | +12% | +12% |
| musl, rlsf | +12% | +78% | +10% | +11% | +10% |
| musl, system | +26% | +79% | +24% | +30% | +29% |

On musl, rpmalloc crashes at start and snmalloc does not link. tcmalloc
and scudo link only as non-PIE, and were not faster than mimalloc.

mimalloc is the fastest on both libcs, so iets uses it on every target.

The static build is still 9–13% slower than the glibc build. Most of the
difference is in musl's `memcpy`, `memmove`, `memcmp` and `memset`. They
take 13.8% of the cycles on desktop w1; glibc's take 2.9%.

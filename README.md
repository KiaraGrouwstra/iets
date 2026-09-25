# ietsp

A Nix evaluator written from scratch in Rust, and a build client that ships
what it evaluates to a Nix daemon.

It is a drop-in: it answers to the classic command names, computes the same
store paths as Nix 2.34.8, and hands anything it does not implement to the
real `nix`. It keeps no store of its own.

```
nix develop -c cargo build --release
./target/release/ietsp eval --strict -E '1 + 1'
./target/release/ietsp --help
./target/release/ietsp help            # the commands, and the topics below
./target/release/ietsp help settings   # a documentation page, out of the binary
```

The binary carries the reference documentation, so `ietsp help TOPIC` works
from anywhere, with no `docs/` directory in sight.

## Why

**It is about 2.5× cheaper and 2 to 4× faster.** Measured on an idle 32-core
machine against Nix 2.34.8, same nixpkgs, identical drvPaths:

| workload | ietsp | nix 2.34.8 |
|---|---:|---:|
| nixos-minimal, instructions | 7.7 G | 19.4 G |
| nixos-desktop, instructions | 10.0 G | 25.8 G |
| nixos-desktop, wall, 1 worker | 0.98 s | 2.20 s |
| nixos-desktop, wall, 4 workers | **0.59 s** | 2.20 s |

**Runs are cached.** A second `eval` or `build` of the same command validates
the files, directories and environment variables the first run read, then
replays the result — nixos-desktop in 40 ms instead of 1 s. Unlike Nix's eval
cache this works for `--expr` and `--file`, not only for flakes.
See `docs/tracing-cache.md`.

**It evaluates in parallel** without changing a single result
(`docs/parallel.md`), and **it builds what it evaluated from memory** —
derivations never touch the disk on the way to the daemon
(`docs/remote-build.md`).

## One binary, many tools

Each of these replaces a separate tool, sharing one evaluator and one cache:

```sh
ietsp build -A hello nixpkgs/default.nix        # nix-build, plus --builder ssh-ng:// or grpc://
ietsp fast-build --flake .#checks --skip-cached # nix-fast-build, one process
ietsp eval-jobs --flake .#hydraJobs             # nix-eval-jobs
ietsp search -I nixpkgs=/path ripgrep           # nix search, byte-identical, index cached
ietsp diff -A hello -A figlet nixpkgs/default.nix   # nix-diff; --closure is nvd
ietsp prefetch --unpack https://example.org/x.tar.gz # nurl, nix-prefetch
ietsp print-dev-env --gc-root .direnv/env .     # nix-direnv's `use flake`
```

Under the classic names — `nix-instantiate`, `nix-build`, `nix-shell`,
`nix-eval-jobs`, `nix-fast-build`, `nix` — the same binary behaves as that
command and execs the real `nix` for the rest (`docs/classic.md`).

A wrong fixed-output hash can be rewritten in place, with a prompt on a
terminal and silently in CI (`fix-hashes` in `docs/settings.md`). NixOS hosts
import `nixosModules.default` and set `ietsp.classic.enable` to put those
names ahead of `nix` in the system path (`docs/classic.md`).

## For programs and agents

Every command speaks JSON and returns stable exit codes, and errors carry a
code, a position and often the fix (`docs/cli.md`). `--log-format
internal-json` emits Nix's own activity records, so `nix-output-monitor` works
unchanged.

`ietsp plumbing` is the low-level surface underneath the user-facing commands:
one operation per command, JSON in and out, composable
(`ietsp plumbing resolve --dump-drvs d x.nix | ietsp plumbing plan --roots -`).
There is one implementation of each capability; `tests/layering.rs` enforces
it. See `docs/plumbing.md`.

The crate is also a library. `api::value::{Eval, Value}` is a safe,
lifetime-checked handle on the evaluator, modelled on Nix's own C API, and
nothing process-global sits in the way of embedding two of them
(`docs/library.md`).

## Status

Private and under active development. The hard invariant is drvPath parity
with Nix 2.34.8 on NixOS systems from nixpkgs; `nix flake check` runs the unit
tests, clippy, fmt, the `tests/lang` suite and the layering rule, and the
scripts under `scripts/` compare against a real `nix` byte for byte. Where
behaviour differs on purpose, `docs/divergences.md` records it.

`ARCHITECTURE.md` is the map. `docs/` holds one file per subsystem.

## Licence

LGPL-2.1-or-later, like Nix (see COPYING). Some files are copied from Nix
(corepkgs, `call-flake.nix`, `get-env.sh`, the `tests/lang` suite) under the
same licence. `proto/nix_remote.proto` comes from Mic92/nix-grpc-store, which
carries no licence yet.

# ietsp

A Nix evaluator written from scratch in Rust, and a build client that ships
what it evaluates to a Nix daemon.

It is a drop-in: it answers to the classic command names, computes the same
store paths as Nix 2.34.8, and hands anything it does not implement to the
real `nix`. It keeps no store of its own.

The aim is to compress the Nix stack. A working Nix setup today is a dozen
helper tools, and most of them exist because Nix lacks one capability or
one good default. ietsp supplies that capability or default, so Nix feels
good to use with no configuration. Speed is the first result, not the
last. The means are good abstractions and good defaults, not a jumbo
binary: a feature belongs when it falls out of what the evaluator already
owns.

```
nix develop -c cargo build --release
./target/release/ietsp eval --strict -E '1 + 1'
./target/release/ietsp --help
./target/release/ietsp help            # the commands, and the topics below
./target/release/ietsp help settings   # a documentation page, out of the binary
```

New here? [docs/tutorial/getting-started.md](docs/tutorial/getting-started.md)
walks from that build to a cache hit in ten minutes.

The binary carries the reference documentation, so `ietsp help TOPIC` works
from anywhere, with no `docs/` directory in sight.

## Why

**It is about 2.5× cheaper and 2 to 4.7× faster.** Measured on an idle 32-core
machine against Nix 2.34.8, same nixpkgs, identical drvPaths:

| workload | ietsp | nix 2.34.8 |
|---|---:|---:|
| nixos-minimal, instructions | 7.7 G | 19.4 G |
| nixos-desktop, instructions | 10.0 G | 25.8 G |
| nixos-desktop, wall, 1 worker | 0.98 s | 2.20 s |
| nixos-desktop, wall, 4 workers | **0.47 s** | 2.20 s |

These are warm compiled-file cache runs, which is what you get after the first
evaluation of a tree. With `--no-ir-cache`, so every file is parsed and
compiled again, the instruction ratio is 1.96× on minimal and 2.11× on
desktop. Nix has no equivalent cache to turn off.

**Runs are cached.** A second `eval` or `build` of the same command validates
the files, directories and environment variables the first run read, then
replays the result — nixos-desktop in 40 ms instead of 1 s. Unlike Nix's eval
cache this works for `--expr` and `--file`, not only for flakes.
See [docs/explanation/caching.md](docs/explanation/caching.md).

**It evaluates in parallel** without changing a single result
([docs/explanation/evaluation.md](docs/explanation/evaluation.md)), and
**it builds what it evaluated from memory** — derivations never touch the
disk on the way to the daemon
([docs/how-to/build-remotely.md](docs/how-to/build-remotely.md)).

## Fewer tools

Each of these replaces a separate tool. None is a bundled copy of that tool:
each one is a short path over the evaluator, the cache and the build client
that ietsp has anyway.

```sh
ietsp build -A hello nixpkgs/default.nix        # nix-build, plus --builder ssh-ng:// or grpc://
ietsp build .#checks                            # every check of this system, one verdict each
ietsp eval-jobs --flake .#hydraJobs             # nix-eval-jobs
ietsp search -I nixpkgs=/path ripgrep           # nix search, byte-identical, index cached
ietsp diff -A hello -A figlet nixpkgs/default.nix   # nix-diff; --closure is nvd
ietsp prefetch --unpack https://example.org/x.tar.gz # nurl, nix-prefetch
ietsp print-dev-env --gc-root .direnv/env .     # nix-direnv's `use flake`
```

One worked example of each swap:
[docs/how-to/replace-nix-tools.md](docs/how-to/replace-nix-tools.md).

Under the classic names — `nix-instantiate`, `nix-build`, `nix-shell`,
`nix-eval-jobs`, `nix` — the same binary behaves as that
command and execs the real `nix` for the rest
([docs/reference/classic.md](docs/reference/classic.md)).

A wrong fixed-output hash can be rewritten in place, with a prompt on a
terminal and silently in CI
([docs/how-to/fix-hashes.md](docs/how-to/fix-hashes.md)). NixOS hosts
import `nixosModules.default` and set `ietsp.classic.enable` to put those
names ahead of `nix` in the system path
([docs/reference/classic.md](docs/reference/classic.md)).

## For programs and agents

Every command speaks JSON and returns stable exit codes, and errors carry a
code, a position and often the fix
([docs/reference/errors.md](docs/reference/errors.md)). `--log-format
internal-json` emits Nix's own activity records, so `nix-output-monitor` works
unchanged.

`ietsp plumbing` is the low-level surface underneath the user-facing commands:
one operation per command, JSON in and out, composable
(`ietsp plumbing resolve --dump-drvs d x.nix | ietsp plumbing plan --roots -`).
There is one implementation of each capability; `tests/layering.rs` enforces
it. The contracts are in
[docs/reference/plumbing.md](docs/reference/plumbing.md) and the rule behind
them in
[docs/explanation/plumbing-and-porcelain.md](docs/explanation/plumbing-and-porcelain.md).

The crate also builds as a library, with a safe value handle
(`api::value::{Eval, Value}`) and no process-global state. It is not ready
to embed in a long-lived program: the evaluator does not free memory until
the process exits. See
[docs/how-to/embed-the-evaluator.md](docs/how-to/embed-the-evaluator.md).

Driving ietsp from a script or an agent:
[docs/how-to/script-ietsp.md](docs/how-to/script-ietsp.md).

## Status

Private and under active development. The hard invariant is drvPath parity
with Nix 2.34.8 on NixOS systems from nixpkgs; `nix flake check` runs the unit
tests, clippy, fmt, the `tests/lang` suite and the layering rule, and the
scripts under `scripts/` compare against a real `nix` byte for byte. Where
behaviour differs on purpose,
[docs/reference/divergences.md](docs/reference/divergences.md) records it.

## Documentation

[ARCHITECTURE.md](ARCHITECTURE.md) is the map of the crate. The rest is in
`docs/`, in four parts.

**Learn it.** [tutorial/getting-started.md](docs/tutorial/getting-started.md)
— one path from a build to a cache hit.

**Do a thing.**

| | |
|---|---|
| [how-to/build-remotely.md](docs/how-to/build-remotely.md) | a daemon, `unix://`, `ssh-ng://` or `grpc://` |
| [how-to/use-with-direnv.md](docs/how-to/use-with-direnv.md) | `print-dev-env` in an `.envrc` |
| [how-to/replace-nix-tools.md](docs/how-to/replace-nix-tools.md) | `nix search`, nix-diff, nvd, nurl, nix-eval-jobs, building a flake's checks |
| [how-to/fix-hashes.md](docs/how-to/fix-hashes.md) | rewrite a wrong fixed-output hash |
| [how-to/script-ietsp.md](docs/how-to/script-ietsp.md) | JSON, exit codes, log records, the plumbing pipeline |
| [how-to/embed-the-evaluator.md](docs/how-to/embed-the-evaluator.md) | the crate as a library |

**Look it up.**

| | |
|---|---|
| [reference/commands.md](docs/reference/commands.md) | every command and flag |
| [reference/plumbing.md](docs/reference/plumbing.md) | the plumbing operations and their JSON |
| [reference/classic.md](docs/reference/classic.md) | what each classic name handles |
| [reference/settings.md](docs/reference/settings.md) | `ietsp.toml`, `nix.conf` and every `IETSP_*` variable |
| [reference/errors.md](docs/reference/errors.md) | error codes, exit codes, log records |
| [reference/lints.md](docs/reference/lints.md) | every lint |
| [reference/syntax-tree.md](docs/reference/syntax-tree.md) | the JSON form of the syntax tree |
| [reference/builtins.md](docs/reference/builtins.md) | the primop surface |
| [reference/divergences.md](docs/reference/divergences.md) | where ietsp differs from Nix on purpose |
| [reference/library.md](docs/reference/library.md) | the library entry points |

**Understand it.**

| | |
|---|---|
| [explanation/evaluation.md](docs/explanation/evaluation.md) | laziness, the value model, the fiber scheduler |
| [explanation/caching.md](docs/explanation/caching.md) | the three stores, and why the input store is safe |
| [explanation/flakes-and-sources.md](docs/explanation/flakes-and-sources.md) | a reference becomes a tree |
| [explanation/plumbing-and-porcelain.md](docs/explanation/plumbing-and-porcelain.md) | the layer rule, and what it cost |
| [explanation/provenance.md](docs/explanation/provenance.md) | a store path back to the file that made it |
| [explanation/lint-scope.md](docs/explanation/lint-scope.md) | what ietsp lints, and how a project makes its own rules |
| [explanation/history.md](docs/explanation/history.md) | what was tried and removed |

## Licence

LGPL-2.1-or-later, like Nix (see COPYING). Some files are copied from Nix
(corepkgs, `call-flake.nix`, `get-env.sh`, the `tests/lang` suite) under the
same licence. `proto/nix_remote.proto` comes from Mic92/nix-grpc-store, which
carries no licence yet.

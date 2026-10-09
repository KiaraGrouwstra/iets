# Builders and the wire

Where a build goes, what iets sends there, and the two protocols it speaks:
the Nix worker protocol and `grpc://`. The NAR format both use is at the
end.

`iets build` evaluates in memory, works out the closure of the root
derivations, and sends only what the store lacks. iets has no local store
of its own: it reads `/nix/store` only for an input derivation that came
from there. For why it works this way, and for the measured numbers, see
`docs/explanation/transports.md`. For the steps, see
`docs/how-to/build-remotely.md`.

## The builder URL

```
--builder daemon
--builder unix:///path
--builder ssh-ng://[user@]host[?remote-program=CMD]
--builder grpc://host:port[?k=v&…]
```

Without the flag, the URL is the `builder` setting in your user
`iets.toml`, or nix.conf's `store` when that is unset
(`docs/reference/settings.md`). A classic `--store URL` or
`--option store URL` wins over both.

- `daemon`, `auto` and an empty value mean
  `/nix/var/nix/daemon-socket/socket`.
- `ssh-ng://` runs `ssh host nix-daemon --stdio` and talks over its stdio,
  as Nix's `ssh-ng` store does. It reads `NIX_SSHOPTS`.
- `grpc://` is a protocol of its own, described below.

Any other store URL is the usage error `unsupported builder 'URL':
expected daemon, unix:///path, ssh-ng://[user@]host or grpc://host:port`.
Under a classic name, iets falls back to the real nix.

## What iets sends

- **Every derivation the evaluation made**, as the exact ATerm it hashed.
  An input derivation the evaluation did not make came from a
  `/nix/store/*.drv`, reached through string context or `import`. iets
  reads that file, and `--timings` counts it as `read from /nix/store`.
- **Every source the evaluation added**, dumped again from where the
  evaluation read it: a local tree with the entries a `filterSource`
  filter accepted, a fetched input, or a `builtins.toFile` text held in
  memory. After hashing, iets checks that the NAR still gives the planned
  store path. If the tree changed since evaluation, the build fails.
- **Sources the evaluation did not add** (under a derivation read from the
  store, or a path pinned with `builtins.storePath`) go to one
  `nix path-info --json` call. A path the local store has as a recursive
  sha256 source or a text file is read from `/nix/store`. Anything else,
  such as an input-addressed build output or a path that is not valid
  locally, is left to the daemon. Nix treats such an `inputSrcs` entry the
  same way: it must be valid or substitutable on the builder.

iets walks the closure once and reads nothing during the walk. It asks
the daemon which paths it has, then finds and hashes only the sources the
daemon lacks. Each path goes after every path it references: sources
first, a `toFile` text after the sources it names, each derivation after
its inputs.

`iets plumbing plan` prints the result of that walk
(`docs/reference/plumbing.md`).

### The legacy `DRV` form

```
iets build [--source STOREPATH=LOCAL]... DRV
```

`DRV` is a `.drv` store path, or a copy of one named `<hash>-<name>.drv`.
The root comes from that file, and its closure from `/nix/store`.
`--source STOREPATH=LOCAL` names the original local tree of an input
source. Everything else works as above.

## The two transports compared

`iets build`, classic `nix-build` and `nix build` build the same way over
every transport. `nix-instantiate`, `nix-shell` and `print-dev-env` upload
the same way. `iets eval-jobs` uploads in batches while the walk runs.

| | worker protocol (`daemon`, `unix://`, `ssh-ng://`) | `grpc://` |
|---|---|---|
| a ready set | one `BuildPathsWithResults` | a `Want` on the `Schedule` stream and one `BuildDerivation` per derivation of the set's closure, `max-builds` in flight |
| build parallelism | the daemon's, `-j` through `SetOptions` | the client's, in flight, placed by the scheduler |
| log lines | attributed by the daemon's activity ids | by the call, which knows its derivation |
| output paths | `QueryDerivationOutputMap` | statically, from the derivations |
| out-links | the local daemon, the closure copied back first | the local daemon substitutes from its caches; the rest by `FetchNars` |

The stdout of the classic names is byte for byte the real nix's
(`scripts/classic-e2e.nu`). All progress goes to stderr.

## The worker protocol

iets uses no Nix library and no local store. Its client is written from
the C++ source of Nix 2.34.8.

### Handshake

The same as Nix's `initConnection`: `WORKER_MAGIC_1`, our version 1.38,
the daemon's version, a feature-set exchange at >= 1.38 (iets offers
none), the obsolete CPU-affinity and reserve-space words, then the
daemon's `nix_version` and its trust flag, then one `STDERR_LAST`.
**iets refuses daemons below protocol 1.35.** No features are negotiated.

### `SetOptions`

iets sends the fields Nix's `RemoteStore::setOptions` sends, with these
values:

| Option | Value |
|:---|:---|
| keep-failed | 0 |
| keep-going | 1 with `--keep-going` or more than one root, else 0 |
| try-fallback | 0 |
| verbosity | `lvlInfo` |
| max-jobs | the CPU count (`max-jobs = auto`; 0 would forbid local builds) |
| max-silent-time | 0 |
| build verbosity | `lvlVomit`, so builder output arrives as `resBuildLogLine` results attributed to their build activity, not as plain log lines |
| cores | 0 |
| substitutes | on |
| the override map | empty |

Under a classic name, these come from nix.conf instead: `keep-failed`,
`keep-going`, `fallback`, `max-jobs`, `max-silent-time`, `cores` and
`substitute`. The override map then holds every other store setting you
set outside the system nix.conf (`docs/reference/settings.md`).

### One request per step

1. **`QueryValidPaths`** on every planned path (sources, text paths,
   derivations) in one call, with the substitute flag off. Then, off the
   wire, iets finds and hashes the missing sources: what the evaluation
   read and the `--source` overrides first, then one
   `nix path-info --json` batch for the rest. The flake fetcher uses the
   same request with the substitute flag on, for a locked input missing
   from the eval store (`docs/reference/flakes.md`).
2. **`BuildPathsWithResults`** on the missing sources iets cannot produce
   (plain store paths, which the daemon substitutes), before any upload.
   The daemon rejects a path whose reference is not valid, so it must
   hold these before the derivations that name them. Skipped when there
   are none. A failed substitution ends the run with the daemon's message.
   `nix build` does the same for an `inputSrcs` entry it does not own.
3. **`AddMultipleToStore`** of the missing paths, in one framed stream, in
   plan order. Each entry is a `ValidPathInfo` in the 1.16 layout the
   daemon expects for this request, followed by the NAR: no deriver, the
   NAR hash and size of iets's own dump, registration time 0,
   `ultimate = false`, no signatures, and the content address, which the
   daemon computes again and checks:

   | Path | Content address |
   |:---|:---|
   | a source tree | `fixed:r:sha256:<nar hash>` |
   | a `builtins.toFile` path | `text:sha256:<flat hash>` plus references |
   | a `.drv` | `text:sha256:<sha256(aterm)>`, references = inputSrcs + inputDrvs |

   A derivation is a text path, as Nix registers it. The daemon accepts a
   content-addressed path from an untrusted client, because it needs no
   signature.
4. **`BuildPathsWithResults`** in normal mode on `<root drv>!<out1,out2>`
   (`!*` without `--out`), the `DerivedPath` string that protocol >= 1.30
   reads, one entry per root in the same request. A failed status is that
   root's verdict. `--no-build` and `--dry-run` stop before this step. The
   substitution of step 2 still runs, or the derivations could not
   register.
5. **`QueryDerivationOutputMap`** for each root that built: its output
   paths.
6. **`NarFromPath`** for each wanted output with `--fetch-to DIR`,
   restored into `DIR`.

### Events

| Wire | Event |
|:---|:---|
| `STDERR_NEXT` | an unattributed `Log` |
| `STDERR_START_ACTIVITY` of type `actBuild` (105) | `Started`, and the activity id maps to its derivation |
| `STDERR_RESULT` of type `resBuildLogLine` (101) or `resPostBuildLogLine` (107) | a `Log` attributed through that map |
| `STDERR_ERROR` | decoded as Nix's `readError`: message plus traces |

Under `internal-json`, or with a `json-log-path`, every activity message
also goes to the log (`docs/reference/errors.md`).

### `--timings`

`--timings` prints one line per step on stderr:

```
plan: 0 sources, 0 drvs, 0.009s
connect: protocol 1.38, trusted, 0.001s
query: 9 paths, 8 missing, 0.002s
resolve: 7 sources, 6 not from this eval, read from /nix/store, 0.003s
substitute: 5 paths left to the daemon, 0.004s
add: 4 paths, 3 bytes, 0.005s
build: 0.006s
outputs: 0.007s
fetch: 2 outputs, 0.008s
worker: 1 ops, 2 round trips, 3 bytes sent
```

`resolve` is the finding and hashing of the missing sources. `substitute`
shows up only when paths were left to the daemon. The round trips are the
requests plus the three handshake exchanges.

## `grpc://`

[nix-grpc-store](https://github.com/Mic92/nix-grpc-store) is a gRPC
service in front of a Nix store. iets speaks it directly, so
`--builder grpc://…` and `--store grpc://…` need no nix, no plugin and no
local store.

### URL parameters

`grpc://host:port?k=v&…` takes nix-grpc-store's own parameter names and
defaults:

| parameter | default | what it does |
|---|---|---|
| `system` | the `system` setting | `x-nix-system` on every call: which worker cluster the balancer routes to |
| `token-file` | none | file with a Keycloak/OIDC bearer token, re-read for every call. Needs TLS |
| `client-cert`, `client-key` | none | PEM client identity for mTLS. Both or neither |
| `ca-cert` | none | PEM CA trusted in addition to the usual roots (`NIX_SSL_CERT_FILE` in reference/settings.md) |
| `insecure` | `false` | plaintext instead of TLS. Local testing only |
| `connect-timeout` | 30 | seconds the first call waits for the service |
| `reschedule-retries` | 8 | how often a build a worker bounced goes back to the scheduler |
| `restart-grace` | 120 | seconds to wait for a scheduler that went away, or for a worker that can take an `Unplaceable` derivation |
| `max-builds` | 64 | concurrent `BuildDerivation` calls; nix.conf `max-jobs` does not apply |
| `nar-connections` | 4 | accepted for URL compatibility; this client fetches over its one channel |

For example:

```
--builder 'grpc://farm.example.com:443?token-file=/run/secrets/token&max-builds=128'
```

### Auth and headers

| Header | Value |
|:---|:---|
| `authorization` | `Bearer <token>` from `token-file`, read fresh for each call, so a rotating projected volume or a CI refresher works |
| `x-nix-system` | on every call, so an upload and the build that follows land on the same cluster. A `BuildDerivation` overrides it with the derivation's own `platform` |
| `x-nix-worker` | on a `BuildDerivation`: the address the scheduler assigned, which the balancer routes to |
| `x-nix-features` | on a `BuildDerivation`: the derivation's `requiredSystemFeatures`, comma separated |

### Flow

First, one `StoreInfo` call, for the trust flag. Then:

1. `QueryValidPaths` with the base name of every planned path, in one
   call. Then iets finds and hashes the missing sources, as on the worker
   protocol.
2. The sources iets cannot produce stay with the store. A worker
   substitutes an input when the build asks for it, so iets only checks
   that some substituter has them, which `QueryValidPaths` with
   `substitute` answers.
3. `AddMultipleToStore`: one client-streamed zstd stream. Decompressed,
   it is exactly the worker-protocol `AddMultipleToStore` framing that the
   `unix://` path writes: the count first, then each `ValidPathInfo` at
   1.16 followed by its NAR. The flags travel alone in the first message.
   The data follows in 256 KiB chunks, and the stream is ended, not
   flushed, once at the end.
4. For each derivation, with at most `max-builds` in flight, and only once
   every input derivation of it is done: a `Want` on the batch's one
   `Scheduler.Schedule` stream, with the output keys, the inputs, the
   features and the platform. The scheduler answers with one of:
   - `Cached`: the verdict is `AlreadyValid`;
   - `Assigned`, with a worker address and an `assign_id`;
   - `Unplaceable`, which is only advice: the `Want` stays queued, and
     fails after `restart-grace`.

   On `Assigned`, a `BuildDerivation` to that worker carries the
   `BasicDerivation` wire form at `protocol = 1 << 8 | 37`,
   `build_mode = 0` and the `assign_id`. `log_line` and `phase` become
   `Log` events. `BuildDerivationDone.result` is a `BuildResult` at 1.37,
   and becomes the derivation's verdict, with a `Failed` event on failure.
   A derivation whose outputs `QueryValidPaths` already reports valid is
   skipped, so it costs no `Want`.
5. `--fetch-to DIR`: the farm never copies an output into a local store,
   so `FetchNars` streams the NARs back. It is one zstd stream across all
   paths, flushed per frame. Each frame is tagged with its `path_index`,
   and the last one of a path is marked `eof`. Each NAR is written next to
   its destination first, then restored there.

iets never opens the worker-protocol tunnel (`Connect`). A farm endpoint
refuses it with `UNIMPLEMENTED`, which is why `nix` needs
`--eval-store auto` there. iets always evaluates in its own process, so it
needs nothing.

### What a `write` token can and cannot do

`write`, the role an OIDC token usually gets, may request builds and
import **signed** paths. nix-grpc-store forces `CheckSigs` on for that
role, however trusted the proxy's uid is. Everything iets uploads is
content-addressed, so the daemon computes the address again and accepts it
without a signature. To send an *unsigned, input-addressed* path (an
output built locally), you need the `trusted` role, which is what a CI
host's client certificate gets. iets never needs it: it uploads only
sources and derivations.

Outputs land in the farm's binary cache, not in your local store.
`--fetch-to` restores them into a directory. An out-link first asks the
local daemon to substitute the outputs (`QueryValidPaths` with
`substitute`), which fetches them from the farm's cache when `nix.conf`
lists it. A worker's own path infos carry only the worker's key, not the
cache key. What the local daemon cannot substitute is read back with
`QueryPathInfos`, fetched with `FetchNars`, and registered with one
worker-protocol `AddMultipleToStore` that carries each path's signatures.
The daemon checks them, as `nix copy --from` does. So your local
`nix.conf` must list and trust the cache (`substituters`,
`trusted-public-keys`), or you must be a trusted user.

### Retries

gRPC reports every connect failure as `UNAVAILABLE`.

| What | Policy |
|:---|:---|
| a unary call | 500 ms doubling to 4 s, until `connect-timeout` has passed |
| the `Schedule` stream | reconnects when the scheduler says `Restarting`, or goes away with `UNAVAILABLE`, `UNKNOWN` or `INTERNAL`, for up to `restart-grace`, and sends every unanswered `Want` again |
| a bounced `BuildDerivation` (`ABORTED`, `FAILED_PRECONDITION`, `UNAVAILABLE`, `UNKNOWN`, `INTERNAL`) | back to the scheduler, up to `reschedule-retries` times. Unlike nix-grpc-store's own client, each ask after the first waits: 1 s doubling to 30 s |
| `NOT_FOUND` | the worker has not got the `.drv` closure: iets uploads it (`QueryValidPaths` then `AddMultipleToStore`, with the same routing headers) and asks once more |
| an `Assigned` naming an address that bounced twice in a row | refused without a call: the derivation goes back to the scheduler once the address's cooldown has passed, bounded by `restart-grace`, not by `reschedule-retries` |

All builds of one run share what they learn about an address. Two
bounces in a row close it, for the same 1 s to 30 s steps, and a build
that returns a result on it, failed or not, opens it again
(`docs/explanation/transports.md` says why). The error names the last
address:
`'<drv>': waited 120s, and every worker the scheduler assigned turns
builds away (last 'worker:50051')`.

## NAR

The NAR iets writes is byte-identical to `nix-store --dump`, not only
hash-identical. The tracing cache keeps the NAR hash of each copied tree,
and dumps the tree again only when its metadata changed
(`docs/explanation/caching.md`). How this is tested, and how fast it is:
`docs/explanation/transports.md`.

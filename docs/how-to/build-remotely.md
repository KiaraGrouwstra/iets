# How to build on a remote builder

`iets build` evaluates in memory, then sends the builder only what it is
missing, in one go. iets keeps no local store, so nothing needs to go into
`/nix/store` first. This page shows you how to use each kind of builder.

Every flag is in [reference/commands.md](../reference/commands.md). To see
why the upload works the way it does, read
[explanation/caching.md](../explanation/caching.md).

## Pick a builder

`--builder URL` takes four forms:

```sh
iets build --builder daemon greeting.nix
iets build --builder unix:///nix/var/nix/daemon-socket/socket greeting.nix
iets build --builder ssh-ng://builder.example.org greeting.nix
iets build --builder grpc://farm.example.org:50051 greeting.nix
```

- `daemon` is this machine's socket, `/nix/var/nix/daemon-socket/socket`.
- `unix:///path` is any other socket that speaks the Nix worker protocol.
- `ssh-ng://[user@]host` runs `ssh host nix-daemon --stdio` and talks to it
  over its standard input and output. It reads `NIX_SSHOPTS`.
- `grpc://host:port` is a [nix-grpc-store](https://github.com/Mic92/nix-grpc-store)
  service. You need no `nix`, no plugin and no local store.

If you leave out the flag, iets uses the `builder` setting in your user
`iets.toml`. If that is unset too, it uses `store` from `nix.conf`. To set
it for one run without the flag:

```sh
iets --iets-option builder ssh-ng://builder.example.org build greeting.nix
```

iets refuses a daemon older than protocol 1.35.

## Set the parameters of a gRPC builder

A `grpc://` URL takes nix-grpc-store's own query parameters. These are the
ones you will most likely need:

```sh
iets build --builder 'grpc://farm.example.org:50051?token-file=/run/secrets/farm-token' greeting.nix
```

| parameter | default | what it does |
|---|---|---|
| `token-file` | none | a file holding an OIDC bearer token, re-read for every call. Needs TLS |
| `client-cert`, `client-key` | none | a PEM client identity for mTLS. Both or neither |
| `system` | the `system` setting | which worker cluster the balancer routes to |
| `max-builds` | 64 | concurrent builds in flight |
| `insecure` | `false` | plaintext instead of TLS, for local testing only |

Without credentials, the first call fails and tells you why:

```
error: gRPC StoreInfo on 'farm.example.org:50051' failed (Unauthenticated):
       server requires a TLS client certificate or bearer token
hint: TLS handshake failed and no client certificate was presented.
      Set 'client-cert'/'client-key' or 'token-file'.
```

A token with the usual `write` role is enough. Everything iets uploads is
content-addressed: a source tree, a `builtins.toFile` text, a `.drv`. The
daemon computes the address again and accepts it without a signature. You
never need the `trusted` role, because iets never sends an unsigned,
input-addressed path.

## Build all the checks of a flake

Name the whole set, not one member. `iets build` builds every derivation
in it, and picks the set for the current system by itself:

```sh
iets build --builder 'grpc://farm.example.org:50051?max-builds=32' .#checks
```

Every check gets its own result, and one failure does not stop the others.
Anything the builder already has is not built again. stderr ends with one
line, such as `12 built, 1 failed: checks.x86_64-linux.clippy`. The exit
code is Nix's: 100 for a failed build. For a program, `--json` puts
`{"outcomes":[…]}` on stdout, one object per check, with the end of the
log of each failure.

On a gRPC builder, `max-builds` in the URL sets how many builds run at
once.

## See what gets uploaded

iets writes nothing to a local store on the way out. It takes the closure
from what the evaluation still holds:

- **Derivations** come from memory. An input derivation that this
  evaluation did not make (one reached through `builtins.storePath`, string
  context or `import`) is read from `/nix/store`. `--timings` counts those
  as `read from /nix/store`.
- **Sources** come from where the evaluation found them. For a `./path` or
  a `builtins.path` copy, iets reads the original local tree again, with
  the same filter. A fetched flake input comes from the input store. A
  `builtins.toFile` text is in memory.
- **Anything else** goes to one batch lookup. The daemon substitutes what
  is still missing.

Before it sends a source, iets checks that it still gives the planned store
path. If the tree changed since the evaluation, the build fails.

Watch it with `--timings`:

```sh
iets build --builder daemon --no-build --timings greeting.nix
```

```
plan: 0 sources, 1 drvs, 0.000s
connect: protocol 1.38, trusted, 0.055s
query: 1 paths, 0 missing, 0.000s
resolve: 0 sources, 0.001s
add: 0 paths, 0 bytes, 0.000s
build: 0.000s
outputs: 0.000s
worker: 2 ops, 5 round trips, 240 bytes sent
```

`query` is one `QueryValidPaths` over every planned path. `add` is one
`AddMultipleToStore` of what that answer said was missing. That is the
whole upload: two operations, not one per path.

## Upload without building

`--no-build` plans, asks and uploads, then stops before the build:

```sh
iets build --builder daemon --no-build greeting.nix
```

It prints nothing when it succeeds. Add `--timings` to see the phases. Use
it to warm up a builder, or to check that a closure uploads cleanly before
you start the build.

`--dry-run` stops even earlier, and tells you what *would* happen:

```sh
iets build --builder daemon --dry-run --json greeting.nix
```

```json
{"downloadSize":0,"narSize":0,"unknown":[],"willBuild":[],"willSubstitute":[]}
```

Without `--json`, you get the same information as text on stderr. The JSON
is byte for byte what `iets plumbing missing` prints (see
[reference/plumbing.md](../reference/plumbing.md)).

With `--dry-run` and `--no-build`, the daemon can still substitute a source
it needs, because it cannot register the derivations without it.

## Get the outputs back

A remote builder keeps its outputs where it built them. `--fetch-to DIR`
streams each wanted output back into `DIR`:

```sh
iets build --builder daemon --fetch-to out greeting.nix
ls out/
```

```
vwp4kv8mzqx77xsf8x4dff531r5bcxkz-greeting
```

Each output lands under the base name of its store path. Nothing is
registered with a local daemon, so you do not need to be a trusted user.

To build only one output, use `--out NAME`. Repeat it for a derivation
with several outputs:

```sh
iets build --builder daemon --out out -I nixpkgs=$nixpkgs -E '(import <nixpkgs> {}).hello'
```

```
/nix/store/xl1h9i29pgq2q5cszjhm5wpfxfbbqwyi-hello-2.12.3
```

## Build a derivation that is already in the store

`iets build` also takes a `.drv` path. The closure then comes from
`/nix/store`, not from an evaluation:

```sh
iets build --builder daemon /nix/store/zr5qd58hj1cxwncj6z0ra1qlp09g8h07-greeting.drv
```

If an input source came from a local tree that the store no longer has,
tell iets where it is:

```sh
iets build --builder daemon --source /nix/store/…-src=./src /nix/store/…-thing.drv
```

## Known limits

- `ssh-ng://` has been tested only through a local `ssh` shim, never
  against a real host. There is no `--ssh-option` passthrough.
- `iets build` sends `max-jobs` equal to the CPU count. It has no flag for
  that or for substituters. A `grpc://` builder always keeps going, so `-k`
  changes nothing there.
- iets asks for the output paths of the roots only. A dependency that built
  gets no line of its own beyond its result.
- During a large upload, an error shows only after the whole stream is
  sent, because iets does not read the daemon's stderr while it streams.
- Over `grpc://`, ^C does not cancel the build on the server.
  `nar-connections` is read and ignored: all fetches use one channel.
- A flat fixed-output source (`builtins.path { recursive = false; }`) has
  no recorded origin, so the daemon must substitute it.

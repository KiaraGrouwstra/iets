# How to build on a remote builder

`iets build` evaluates in memory and ships the closure the builder lacks
in one operation. It keeps no local store, so nothing has to be written to
`/nix/store` first. This page shows how to point it at each kind of
builder.

The full flag list is in [reference/commands.md](../reference/commands.md).
Why the upload has the shape it has is in
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
- `ssh-ng://[user@]host` runs `ssh host nix-daemon --stdio` and uses its
  standard input and output. It reads `NIX_SSHOPTS`.
- `grpc://host:port` is a [nix-grpc-store](https://github.com/Mic92/nix-grpc-store)
  service. It needs no `nix`, no plugin and no local store.

Without the flag, the URL is the `builder` setting in the user `iets.toml`, and
`nix.conf`'s `store` when that is unset. Set it once instead of typing the
flag:

```sh
iets --iets-option builder ssh-ng://builder.example.org build greeting.nix
```

A daemon older than protocol 1.35 is refused.

## Set the parameters of a gRPC builder

A `grpc://` URL takes nix-grpc-store's own query parameters. The ones you
are most likely to need:

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

Without credentials the first call fails and says so:

```
error: gRPC StoreInfo on 'farm.example.org:50051' failed (Unauthenticated):
       server requires a TLS client certificate or bearer token
hint: TLS handshake failed and no client certificate was presented.
      Set 'client-cert'/'client-key' or 'token-file'.
```

A token with the usual `write` role is enough. Everything iets uploads is
content-addressed — a source tree, a `builtins.toFile` text, a `.drv` — so
the daemon recomputes the address and accepts it without a signature. You
never need the `trusted` role, because iets never sends an unsigned,
input-addressed path.

## Build all the checks of a flake

Name the set, not one member. `iets build` builds every derivation in
it, and enters the set of the current system by itself:

```sh
iets build --builder 'grpc://farm.example.org:50051?max-builds=32' .#checks
```

Every check gets its verdict: a failure does not stop the others. What
the builder already has is not built again. stderr ends with one line,
`12 built, 1 failed: checks.x86_64-linux.clippy`, and the exit code is
Nix's: 100 for a failed build. For a machine, `--json` puts
`{"outcomes":[…]}` on stdout, one object per check, with the log tail of
each failure.

`max-builds` in the URL is how many builds run at once on a gRPC
builder.

## Know what gets uploaded

Nothing is written to a local store on the way out. The closure comes from
what the evaluation is still holding:

- **Derivations** come from the evaluator's own table. `derivationStrict`
  kept the ATerm it hashed, so nothing is serialised twice. An input
  derivation that was not created by this evaluation — one reached through
  `builtins.storePath`, string context or `import` — is read from
  `/nix/store`, and `--timings` counts those as `read from /nix/store`.
- **Sources** come from where the evaluation found them. A `./path` or a
  `builtins.path` copy records the original local tree plus the entries a
  filter accepted, so the client re-dumps exactly the same NAR. A fetched
  flake input is streamed out of the input store. A `builtins.toFile` text
  is in memory.
- **Anything with no recorded origin** goes to one batch lookup, and what
  is still unaccounted for is left to the daemon to substitute.

After hashing, the client checks that each NAR still yields the store path
that was planned, and fails if the tree changed since evaluation.

Watch it happen with `--timings`:

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
`AddMultipleToStore` of what the answer said was missing. That is the whole
upload: two operations, not one per path.

## Upload without building

`--no-build` runs the plan, the query and the upload, and stops before the
build:

```sh
iets build --builder daemon --no-build greeting.nix
```

It prints nothing on success; add `--timings` to see the phases. Use it to
warm a builder, or to check that a closure uploads cleanly before you
commit to the build.

`--dry-run` stops earlier still and reports what *would* happen:

```sh
iets build --builder daemon --dry-run --json greeting.nix
```

```json
{"downloadSize":0,"narSize":0,"unknown":[],"willBuild":[],"willSubstitute":[]}
```

Without `--json` the same information is printed as text on stderr. The
JSON is byte-for-byte what `iets plumbing missing` prints; see
[reference/plumbing.md](../reference/plumbing.md).

`--dry-run` and `--no-build` still let the daemon substitute a
source it needs, because the derivations cannot register otherwise.

## Get the outputs back

A remote builder leaves its outputs where they were built. `--fetch-to DIR`
streams each wanted output back and restores it into `DIR`:

```sh
iets build --builder daemon --fetch-to out greeting.nix
ls out/
```

```
vwp4kv8mzqx77xsf8x4dff531r5bcxkz-greeting
```

Each output lands under its store path's base name. Nothing is registered
with a local daemon, so the copy needs no trusted-user status.

Build one output only with `--out NAME`, repeatable for a derivation that
has several:

```sh
iets build --builder daemon --out out -I nixpkgs=$nixpkgs -E '(import <nixpkgs> {}).hello'
```

```
/nix/store/xl1h9i29pgq2q5cszjhm5wpfxfbbqwyi-hello-2.12.3
```

## Build a derivation that is already in the store

`iets build` also takes a `.drv` path directly. Its closure then comes
from `/nix/store` instead of from an evaluation:

```sh
iets build --builder daemon /nix/store/zr5qd58hj1cxwncj6z0ra1qlp09g8h07-greeting.drv
```

If an input source of that derivation came from a local tree that the store
no longer holds, name it:

```sh
iets build --builder daemon --source /nix/store/…-src=./src /nix/store/…-thing.drv
```

## Known limits

- `ssh-ng://` has been exercised only through a local `ssh` shim, never
  against a real host. There is no `--ssh-option` passthrough.
- `iets build` sends `max-jobs` equal to the CPU count and has no flag for
  it or for substituters. A `grpc://` builder always keeps going: `-k`
  changes nothing there.
- Output paths are asked for the roots only. A dependency that built has no
  line of its own beyond the renderer's verdict.
- Errors during a large upload surface after the whole stream is sent,
  because the client does not poll the daemon's stderr while streaming.
- Over `grpc://`, a build cancelled with ^C is not cancelled on the server,
  and `nar-connections` is parsed and ignored: fetches run over one channel.
- Flat fixed-output sources (`builtins.path { recursive = false; }`) have
  no recorded origin, so the daemon must substitute them.

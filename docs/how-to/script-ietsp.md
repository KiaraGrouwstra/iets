# How to drive ietsp from a script or an agent

Every command speaks JSON, returns a stable exit code, and can put its
whole stderr into machine-readable records. This page shows the four things
a caller needs: structured output, exit codes, a log stream, and the
plumbing operations.

The record shapes are in [reference/errors.md](../reference/errors.md), and
every plumbing operation's contract is in
[reference/plumbing.md](../reference/plumbing.md).

## Get JSON instead of text

`--json` is on `eval`, `build --dry-run`, `diff`, `search`, `prefetch` and
`print-dev-env`:

```sh
ietsp eval --strict --json -I nixpkgs=$nixpkgs hello.nix
```

```json
{"drv":"/nix/store/2bki…-hello-2.12.3.drv","name":"hello-2.12.3"}
```

```sh
ietsp diff --json -A a -A b demo.nix
```

```json
{"a":"/nix/store/623k…-demo.drv","b":"/nix/store/sx7z…-demo.drv",
 "changes":[{"a":"hello","b":"goodbye","key":"GREETING","what":"env"}],
 "equal":false}
```

`ietsp eval-jobs` writes JSON lines by default, one object per job.

## Branch on the exit code

```sh
ietsp build --builder daemon fail.nix; echo $?
```

| code | meaning |
|---|---|
| 0 | success |
| 1 | any error, and a build failure with no code of its own |
| 2 | a usage error of a native command. Under a classic name it is 1, as in Nix |
| 100 | a build failed |
| 101 | a build timed out |
| 102 | a fixed-output build had a hash mismatch |
| 104 | a build was not deterministic |

The build codes follow Nix's own `failingExitStatus`: `0b1100000` plus one
bit per kind of failure, so several failures combine their bits and a
timeout beside a failed build gives 101. Real Nix computes these only when
it builds in its own store; as a client it exits 1, because the worker
protocol does not carry the status. ietsp is always a client and reads the
status of each build result, so you get the code.

`ietsp diff` is the one exception, and it follows `diff(1)`: 0 equal, 1
different, 2 a usage error or any other failure.

The classic names exit 1 as Nix does. `ietsp fast-build` exits 1 on any
failure, as `nix-fast-build` does. `ietsp eval-jobs` exits 0 when only jobs
fail; their errors are in the job lines.

## Read the log as records

`--log-format internal-json` turns every stderr line into `@nix ` followed
by one JSON object — the format of Nix's own `JSONLogger`, so
`nix-output-monitor` works unchanged:

```sh
ietsp eval --log-format internal-json -E '{a=1;}.b'
```

```
@nix {"action":"msg","code":"missing-attr","column":1,"file":"«string»:1:1","level":0,"line":1,"msg":"error: attribute 'b' missing\n       at «string»:1:1","raw_msg":"attribute 'b' missing","sid":"01a0d7d6-f649-78fc-b0a9-70261702a3f8"}
```

Three keys are ietsp's own and are the ones a caller wants:

- `code` — a stable error code. Branch on this, not on the message text.
- `hint` — what to do about it, present only when ietsp is certain. A hash
  mismatch names the hash the build got; a `flake.lock` entry without a
  `narHash` names the input and the `nix flake update` that locks it.
- `sid` — one session id on every record of one run. Set it yourself with
  `IETSP_SESSION_ID` to correlate a run with your own logs.

`raw_msg` is the message alone and `msg` is the whole text ietsp would have
printed. `level` is 0 for an error and 1 for a warning. With `--show-trace`
a `trace` array is added, innermost first.

## Send the records somewhere else

`json-log-path` writes every record to a file or a unix socket, without the
`@nix ` prefix, whatever `--log-format` says. Your stderr is unchanged, so
a human can still watch the run:

```sh
ietsp eval --json-log-path ./log.json -E '{a=1;}.b'
cat log.json
```

```json
{"action":"msg","code":"missing-attr","column":1,"file":"«string»:1:1","level":0,"line":1,"msg":"error: attribute 'b' missing\n       at «string»:1:1","raw_msg":"attribute 'b' missing","sid":"01a0d7d7-0a1b-764c-9fa8-1deb8983bdb7"}
```

`--option json-log-path PATH` and the `nix.conf` key of the same name do
the same thing. A file is opened for append and created with mode 0644. A
path that will not open gives one ignored-error line and the run goes on.
Two processes writing one file can interleave their records, as in Nix.

## Turn off the interactive behaviour

Two things change when a terminal is not there, and you should set them
explicitly rather than rely on the detection:

```sh
ietsp --ietsp-option fix-hashes always build --builder daemon src.nix
```

`fix-hashes` defaults to `ask`, which only prompts on a terminal and
otherwise prints a hint. `always` rewrites without asking. See
[fix-hashes.md](fix-hashes.md).

The other is colour in `ietsp search`: Nix writes its escape sequences to
stdout whether or not stdout is a terminal, and ietsp matches it for
byte-parity. Use `--json` when a program reads the output.

## Compose the plumbing operations

`ietsp plumbing` is the layer underneath the user-facing commands: one
operation per command, JSON in and JSON out, nothing else on stdout. There
are five of them — `resolve`, `plan`, `missing`, `build` and `fetch`.

Every operation that takes roots accepts `--roots -` and reads the previous
one's stdout, so they pipe:

```sh
ietsp plumbing resolve greeting.nix
```

```json
{"attrPaths":[],"roots":[{"attr":null,"drv":"/nix/store/zr5q…-greeting.drv","output":null}]}
```

```sh
ietsp plumbing resolve greeting.nix | ietsp plumbing plan --roots -
```

```json
{"drvs":["/nix/store/zr5q…-greeting.drv"],"sources":[],"storeDrvs":1}
```

```sh
ietsp plumbing resolve greeting.nix \
  | ietsp plumbing missing --builder daemon --check-cache-status --roots -
```

```json
{"downloadSize":0,"narSize":0,
 "roots":[{"cacheStatus":"local","drv":"/nix/store/zr5q…-greeting.drv",
           "neededBuilds":[],"neededSubstitutes":[]}],
 "unknown":[],"willBuild":[],"willSubstitute":[]}
```

```sh
ietsp plumbing resolve greeting.nix | ietsp plumbing build --builder daemon --roots -
```

```json
{"outcomes":[{"attr":"greeting","drv":"/nix/store/zr5q…-greeting.drv",
              "logTail":[],
              "outputs":{"out":"/nix/store/vwp4…-greeting"},
              "reason":null,"success":true}]}
```

`missing`'s object is byte-for-byte what `ietsp build --dry-run --json`
prints, and that is checked by a test.

One thing to know when you pipe across processes: by default the
downstream operation reads the closure from `/nix/store`, which only works
for a derivation that is already there. To hand over a closure that is not,
dump it:

```sh
ietsp plumbing resolve --dump-drvs dd demo.nix -A a
```

```json
{"attrPaths":[],"drvDir":"dd","roots":[{"attr":"a","drv":"/nix/store/623k…-demo.drv","output":null}]}
```

The `drvDir` key travels with the roots, so the next operation in the pipe
reads the derivations out of that directory instead. `--drvs DIR` sets it
by hand.

`ietsp plumbing` is deliberately absent from the top-level help: that help
is for people. `ietsp plumbing --help` lists every operation.

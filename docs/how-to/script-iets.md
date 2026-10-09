# How to drive iets from a script or an agent

Every command speaks JSON, returns a stable exit code, and can write all of
its stderr as records a program can read. This page shows you the four
things a script needs: JSON output, exit codes, a log stream, and the
plumbing operations.

The record shapes are in [reference/errors.md](../reference/errors.md), and
every plumbing operation's contract is in
[reference/plumbing.md](../reference/plumbing.md).

## Get JSON instead of text

`--json` is on `eval`, `build --dry-run`, `diff`, `search`, `prefetch` and
`print-dev-env`:

```sh
iets eval --strict --json -I nixpkgs=$nixpkgs hello.nix
```

```json
{"drv":"/nix/store/2bki…-hello-2.12.3.drv","name":"hello-2.12.3"}
```

```sh
iets diff --json -A a -A b demo.nix
```

```json
{"a":"/nix/store/623k…-demo.drv","b":"/nix/store/sx7z…-demo.drv",
 "changes":[{"a":"hello","b":"goodbye","key":"GREETING","what":"env"}],
 "equal":false}
```

`iets eval-jobs` writes JSON lines by default, one object per job.

## Branch on the exit code

```sh
iets build --builder daemon fail.nix; echo $?
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

The build codes are Nix's own: 100 plus one bit per kind of failure. So
several failures combine their bits, and a timeout next to a failed build
gives 101.
Real Nix gives these codes only when it builds in its own store; as a
client of a daemon it exits 1. iets reads the status of each build, so you
always get the code.

`iets diff` is the one exception. It follows `diff(1)`: 0 equal, 1
different, 2 a usage error or any other failure.

The classic names exit 1, as Nix does. `iets eval-jobs` exits 0 when only
jobs fail; you find their errors in the job lines.

## Read the log as records

`--log-format internal-json` turns every stderr line into `@nix ` and one
JSON object. That is the format of Nix's own JSON logger, so
`nix-output-monitor` works as it is:

```sh
iets eval --log-format internal-json -E '{a=1;}.b'
```

```
@nix {"action":"msg","code":"missing-attr","column":1,"file":"«string»:1:1","level":0,"line":1,"msg":"error: attribute 'b' missing\n       at «string»:1:1","raw_msg":"attribute 'b' missing","sid":"01a0d7d6-f649-78fc-b0a9-70261702a3f8"}
```

Three keys are iets's own, and they are the ones you want:

- `code`: a stable error code. Branch on this, not on the message text.
- `hint`: what to do about it, there only when iets is sure. A hash
  mismatch names the hash the build got. A `flake.lock` entry without a
  `narHash` names the input and the `nix flake update` that locks it.
- `sid`: one session id on every record of one run. Set it yourself with
  `IETS_SESSION_ID` to match a run with your own logs.

`raw_msg` is the message alone, and `msg` is the whole text iets would have
printed. `level` is 0 for an error and 1 for a warning. With `--show-trace`
you also get a `trace` array, innermost first.

## Send the records somewhere else

`json-log-path` writes every record to a file or a unix socket, without the
`@nix ` prefix, whatever `--log-format` says. Your stderr stays the same,
so a person can still watch the run:

```sh
iets eval --json-log-path ./log.json -E '{a=1;}.b'
cat log.json
```

```json
{"action":"msg","code":"missing-attr","column":1,"file":"«string»:1:1","level":0,"line":1,"msg":"error: attribute 'b' missing\n       at «string»:1:1","raw_msg":"attribute 'b' missing","sid":"01a0d7d7-0a1b-764c-9fa8-1deb8983bdb7"}
```

`--option json-log-path PATH` and the `nix.conf` key of the same name do
the same. iets appends to a file, and creates it with mode 0644. If the
path will not open, you get one ignored-error line and the run goes on.
Two processes that write one file can mix their records, as in Nix.

## Turn off the interactive behaviour

Two things change when there is no terminal. Set them yourself instead of
relying on the detection:

```sh
iets --iets-option fix-hashes always build --builder daemon src.nix
```

`fix-hashes` defaults to `ask`, which asks only on a terminal and otherwise
prints a hint. `always` fixes without asking. See
[fix-hashes.md](fix-hashes.md).

The other is colour in `iets search`. Nix writes its escape sequences to
stdout even when stdout is not a terminal, and iets does the same, byte for
byte. Use `--json` when a program reads the output.

## Compose the plumbing operations

`iets plumbing` is the layer under the everyday commands. Each operation
takes JSON and gives JSON, with nothing else on stdout. There are five:
`resolve`, `plan`, `missing`, `build` and `fetch`.

Every operation that takes roots accepts `--roots -` and reads the stdout
of the one before it, so you can pipe them:

```sh
iets plumbing resolve greeting.nix
```

```json
{"attrPaths":[],"roots":[{"attr":null,"drv":"/nix/store/zr5q…-greeting.drv","output":null}]}
```

```sh
iets plumbing resolve greeting.nix | iets plumbing plan --roots -
```

```json
{"drvs":["/nix/store/zr5q…-greeting.drv"],"sources":[],"storeDrvs":1}
```

```sh
iets plumbing resolve greeting.nix \
  | iets plumbing missing --builder daemon --check-cache-status --roots -
```

```json
{"downloadSize":0,"narSize":0,
 "roots":[{"cacheStatus":"local","drv":"/nix/store/zr5q…-greeting.drv",
           "neededBuilds":[],"neededSubstitutes":[]}],
 "unknown":[],"willBuild":[],"willSubstitute":[]}
```

```sh
iets plumbing resolve greeting.nix | iets plumbing build --builder daemon --roots -
```

```json
{"outcomes":[{"attr":"greeting","drv":"/nix/store/zr5q…-greeting.drv",
              "logTail":[],
              "outputs":{"out":"/nix/store/vwp4…-greeting"},
              "reason":null,"success":true}]}
```

`missing` prints byte for byte what `iets build --dry-run --json` prints. A
test checks that.

One thing to know when you pipe: by default, the next operation reads the
closure from `/nix/store`. That only works for a derivation that is
already there. To hand over one that is not, dump it:

```sh
iets plumbing resolve --dump-drvs dd demo.nix -A a
```

```json
{"attrPaths":[],"drvDir":"dd","roots":[{"attr":"a","drv":"/nix/store/623k…-demo.drv","output":null}]}
```

The `drvDir` key travels with the roots, so the next operation reads the
derivations from that directory. `--drvs DIR` sets it by hand.

`iets plumbing` is left out of the top-level help on purpose, because that
help is for people. `iets plumbing --help` lists every operation.

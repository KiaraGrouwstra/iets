# Errors, exit codes and log records

The stable error codes, the exit codes, the `--log-format internal-json`
record shape, the activity records and the `json-log-path` setting.

Every error goes through one reporter (`src/log.rs`). It prints Nix's
plain text, or with `--log-format internal-json` one `@nix` JSON record.
It also sets the exit code. The `ietsp` command sends it to stderr; a
library host sets its own sink, or none, on its `Session`
(docs/reference/library.md). Humans read the text. Programs read the
record and the exit code. Agents read the `hint`.

## Error codes

Each error has a stable kebab-case `code`. The code comes from the error
kind: `ErrorKind` in `src/eval/error.rs` for evaluation errors, `Error`
in `src/error.rs` for the rest, and the daemon's build status for builds.

| Code | Kind | What went wrong |
| --- | --- | --- |
| `eval` | `ErrorKind::Eval` | an evaluation error with no more specific kind |
| `type` | `ErrorKind::Type` | a value of the wrong type |
| `throw` | `ErrorKind::Throw` | `builtins.throw` |
| `assert` | `ErrorKind::Assert` | a failed `assert` |
| `abort` | `ErrorKind::Abort` | `builtins.abort` |
| `undefined-var` | `ErrorKind::UndefinedVar` | a variable that is not in scope |
| `missing-attr` | `ErrorKind::MissingAttr` | an attribute that is not there |
| `infinite-recursion` | `ErrorKind::InfiniteRecursion` | a value that depends on itself |
| `stack-overflow` | `ErrorKind::StackOverflow` | the evaluator ran out of stack |
| `parse` | `ErrorKind::Parse` | a syntax error |
| `lint` | `ErrorKind::Lint` | a lint at level `error`; at `warn`, a level-1 record |
| `hash-mismatch` | `ErrorKind::HashMismatch`, build status | a fetched tree or a fixed-output build whose hash is not the one given |
| `hash-fixed` | `fix-hashes` | a level-3 record: the hash `fix-hashes` wrote, at the file, line and column it changed |
| `determinate-setting` | `src/config.rs` | a level-1 record: a Determinate Nix setting that ietsp ignores (docs/reference/settings.md) |
| `io` | `ErrorKind::Io`, `Error::Io` | a file that cannot be read or written |
| `unsupported` | `ErrorKind::Unsupported`, `Error::Unsupported` | a feature ietsp does not have; classic mode falls back to Nix |
| `cancelled` | `ErrorKind::Cancelled` | a parallel helper was stopped; never reaches the user |
| `usage` | `Error::Usage` | a bad command line |
| `config` | `Error::Config` | a bad `nix.conf` or ietsp setting |
| `store` | `Error::Store` | a bad store path, derivation or hash |
| `flake` | `Error::Flake` | a bad flake reference or lock file |
| `protocol` | `Error::Protocol` | bad data from another process (daemon, worker) |
| `cache` | `Error::Cache` | the tracing cache |
| `plan` | `Error::Plan` | a derivation closure that cannot be built |
| `no-results` | `ietsp search` | no package matched every regex (docs/reference/commands.md) |
| `build-failed` | build status | a build that failed |

## Exit codes

| Code | Meaning |
| --- | --- |
| 0 | Success. |
| 1 | Any error. Also a build failure that has no code of its own. |
| 2 | A usage error of a native command (`ietsp eval --bogus`). Under a classic name the code is 1, as in Nix. |
| 1, 2 | `ietsp diff` alone follows `diff(1)`: 0 the two are equal, 1 they differ, 2 a usage error or any other failure (docs/reference/commands.md). |
| 100 | A build failed (`PermanentFailure`, `InputRejected`). |
| 101 | A build timed out. |
| 102 | A fixed-output build had a hash mismatch. |
| 104 | A build was not deterministic. |

The build codes follow `ExitStatusFlags::failingExitStatus` in Nix's
`src/libstore/build-result.cc`. The value is `0b1100000` plus one bit
per kind of failure: 4 build failed, 1 timeout, 2 hash mismatch, 8 not
deterministic. Several failures combine their bits: a timeout and a
failed build give 101. `ietsp build` uses them. Nix computes them only
when it builds in its own store; as a client of a daemon it exits 1,
because the worker protocol does not carry the exit status. ietsp is
always a client, but it reads the status of each `BuildResult`. The
classic names (`nix-build`, `nix build`, `nix-shell`) exit 1, as Nix
does through a daemon. `ietsp fast-build` exits 1 on any failure, as
`nix-fast-build` does. `ietsp eval-jobs` exits 0 when only jobs fail;
their errors are in the job lines.

A NAR hash mismatch of a fetched tree has code `hash-mismatch` but exits
1, as the same error does in Nix.

## `--log-format internal-json`

The native commands (`eval`, `build`, `diff`, `eval-jobs`, `fast-build`,
`print-dev-env`) and the classic names take `--log-format FORMAT` and
`--show-trace`. `internal-json` is the format; `raw`, `raw-with-logs`,
`bar` and `bar-with-logs` give the plain text. Any other value is a usage
error with Nix's message.

Every stderr line is then `@nix ` and one JSON object, the format of
`JSONLogger` in Nix's `src/libutil/logging.cc`. The keys are sorted, as
Nix sorts them. An error is `logEI`'s record plus `code` and, when ietsp
knows the fix, `hint`:

```json
{"action":"msg","code":"missing-attr","column":12,"file":"«string»:1:12",
 "level":0,"line":1,"msg":"error: attribute 'b' missing\n       at «string»:1:12",
 "raw_msg":"attribute 'b' missing"}
```

| Key | Value |
| --- | --- |
| `action` | `"msg"` |
| `level` | 0 for an error, 1 for a warning |
| `msg` | the whole text ietsp prints without `--log-format` |
| `raw_msg` | the message alone |
| `line`, `column` | where the error is, or `null` |
| `file` | `FILE:LINE:COL`, as Nix fills it, or `null` |
| `code` | the error code (ietsp only) |
| `hint` | what to do about it (ietsp only, only when known) |
| `sid` | the session: one id for every record of one run (ietsp only) |
| `trace` | with `--show-trace` only: `[{raw_msg, line, column, file}]`, innermost first |

A lint warning is a level-1 record with code `lint`; its `raw_msg` starts
with `[NAME]`. A failed build is a record with code `build-failed` or
`hash-mismatch`: `raw_msg` is the daemon's message, and the position is
where the derivation was defined.

Any other line is `{"action":"msg","level":N,"msg":TEXT}`: level 0 for
`error:` and `trace:` lines, 1 for warnings, 3 for the rest (build
progress, summaries, classic fallback notes). The lines that `--timings`
and `IETSP_STATS` add stay plain text.

### Activity records

A command that talks to a store also puts out Nix's activity records,
the ones `nix-output-monitor` (`nom --json`) reads. These are `startActivity`,
`stopActivity` and `result` of `JSONLogger` in `src/libutil/logging.cc`:

```json
{"action":"start","fields":["/nix/store/…-hello.drv","",1,1],"id":…,"level":3,"parent":0,"text":"building '/nix/store/…-hello.drv'","type":105}
{"action":"result","fields":["hello"],"id":…,"type":101}
{"action":"stop","id":…}
```

The type numbers are `ActivityType` and `ResultType` of
`src/libutil/include/nix/util/logging.hh`: activities 100 copy path, 101
file transfer, 102 realise, 103 copy paths, 104 builds, 105 build, 108
substitute, 109 query path info; results 101 build log line, 104 set
phase, 105 progress, 106 set expected.

- **From the daemon.** A worker-protocol daemon (`daemon`, `unix://`,
  `ssh-ng://`) sends its activities during a build or a substitution.
  ietsp forwards each one, as `worker-protocol-connection.cc:processStderr`
  does. Each forwarded activity gets an id of this process, and its
  `parent` follows.
- **ietsp's own uploads.** The derivations and sources ietsp sends with
  `AddMultipleToStore`, and a remote builder's outputs copied back to the
  local daemon, are one `copying N paths` activity (type 103) with
  progress results and one `copying path` activity (type 100) per path,
  as `nix copy` shows them (`store-api.cc:copyPaths`).
- **`grpc://`.** There is no daemon stream, so ietsp makes the records of
  `build/worker.cc` itself. They are the realise, builds and copy-paths
  activities around a batch, and one `building 'DRV' on 'URL'` activity
  per derivation. The build's log lines and phases are its results. The
  builds activity gets progress results `[done, expected, running,
  failed]`.

An id is Nix's form: the pid in the high 32 bits and a counter below.
Records go out only under `internal-json` or with a `json-log-path`.
Under `internal-json` the build renderer leaves the builds to these
records: it puts out no start, phase, log or verdict lines. The failed
build's error record, notes and daemon messages outside a build still go
out. The text output does not change.

## The build plan: `ietsp build --dry-run`

`ietsp build --dry-run` registers the closure, as `--no-build` does, and
asks the daemon what a build would do (`QueryMissing`,
`remote-store.cc:queryMissing`). It builds nothing. It prints what
`nix build --dry-run` prints (`printMissing` in `src/libmain/shared.cc`),
at level 0:

```
these 2 derivations will be built:
  /nix/store/…-dep.drv
  /nix/store/…-top.drv
this path will be fetched (1.5 MiB download, 5.9 MiB unpacked):
  /nix/store/…-figlet-2.2.5
don't know how to build these paths:
  /nix/store/…
```

The derivations come references first. The fetched paths are sorted by
name. The sizes are Nix's `renderSize`: at least KiB, one decimal. A
`grpc://` builder has no `QueryMissing`, so `--dry-run` is a usage error
there.

With `--json`, stdout also gets the plan as one JSON object. This is an
ietsp extension: `nix build --dry-run --json` prints the installables'
derived paths, not the plan.

```json
{"downloadSize":1553696,"narSize":6180152,"unknown":[],
 "willBuild":["/nix/store/…-dep.drv","/nix/store/…-top.drv"],
 "willSubstitute":["/nix/store/…-figlet-2.2.5"]}
```

| Key | Value |
| --- | --- |
| `willBuild` | the derivations to build, in the order of the text |
| `willSubstitute` | the paths to fetch from a substituter, in the order of the text |
| `unknown` | the paths nothing can build or fetch |
| `downloadSize` | bytes to download for `willSubstitute` |
| `narSize` | bytes of those paths unpacked |

The names are the fields of Nix's `MissingPaths`.

`ietsp plumbing missing` prints the same object, and
`ietsp plumbing build` the outcomes of the same dispatch, from derivation
paths instead of an installable (docs/reference/plumbing.md). Both, and
`ietsp build`, go through one call, `api::build::Dispatch`.

### `sid`: the session

Every record carries `sid`, activity records included, on stderr and in the
`json-log-path` file. It lets a program that reads several streams group
the records of one run.
This is an ietsp extension: Nix 2.34.8 has no such field. The key and the
default value have the shape Determinate Nix uses (`sid`, a UUIDv7).

- By default each session makes a new UUIDv7, for example
  `019e037b-f603-7443-8071-cfd8585ec8c6`. It starts with the time in
  milliseconds, so ids sort by start time.
- `IETSP_SESSION_ID=ID` sets it. Use this to give several invocations one
  session.
- `eval-jobs` starts its worker processes with `IETSP_SESSION_ID` set to its
  own id, so the workers' records have the id of the run that started them.
- The `session-id` setting of Determinate Nix does not set it; ietsp warns
  about it and ignores it (docs/reference/settings.md).

## `json-log-path`

The `json-log-path` setting (nix.conf, `--option json-log-path PATH` or
`--json-log-path PATH`) names a file or a unix socket. Every record above,
activity records too, also goes there, without the `@nix ` prefix, whatever `--log-format` says;
stderr does not change. A file is opened for append and created with mode
0644. A path that does not open gives one `error (ignored): opening log
file` line, and the run goes on. This is `applyJSONLogger` in Nix's
`src/libutil/logging.cc`. As in Nix, two processes that write the same file
can interleave their records.

### Hints

A hint is there only when it is certain:

- A hash mismatch of a fixed-output build names the hash the build got.
- A NAR hash mismatch of a fetched tree names the hash of the tree and,
  for a `fetchTree` call, the position of its `narHash` attribute.
- A usage error repeats the text's own `did you mean` and `--help` lines.
- A `flake.lock` entry without a `narHash` names the input by its path
  from the root (`a/nixpkgs`) and the `nix flake update` that locks it
  again.
- A `git` input whose tree is not the one the lock names (nix reports a
  NAR hash mismatch, fetchers.cc:`Input::checkLocks`) has code
  `hash-mismatch`, and the hint names the usual causes: a `.gitattributes`
  filter such as git-lfs, `export-subst`, or a submodule.
- A file that is in the working tree of a git flake but that git does not
  track is Nix's error (git.cc:`makeNotAllowedError`, `Path 'F' in the
  repository "R" is not tracked by Git.`), and the hint is the
  `git -C "R" add "F"` that makes it visible.

A missing attribute or an undefined variable has no hint: ietsp has no
did-you-mean suggestion for them yet.

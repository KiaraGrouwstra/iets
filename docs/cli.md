# Errors, exit codes and log records

Every error goes through one reporter (`src/log.rs`). It prints Nix's
plain text, or with `--log-format internal-json` one `@nix` JSON record.
It also sets the exit code. Humans read the text. Programs read the
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
| `build-failed` | build status | a build that failed |

## Exit codes

| Code | Meaning |
| --- | --- |
| 0 | Success. |
| 1 | Any error. Also a build failure that has no code of its own. |
| 2 | A usage error of a native command (`ietsp eval --bogus`). Under a classic name the code is 1, as in Nix. |
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

The native commands (`eval`, `build`, `eval-jobs`, `fast-build`,
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
| `trace` | with `--show-trace` only: `[{raw_msg, line, column, file}]`, innermost first |

A lint warning is a level-1 record with code `lint`; its `raw_msg` starts
with `[NAME]`. A failed build is a record with code `build-failed` or
`hash-mismatch`: `raw_msg` is the daemon's message, and the position is
where the derivation was defined.

Any other line is `{"action":"msg","level":N,"msg":TEXT}`: level 0 for
`error:` and `trace:` lines, 1 for warnings, 3 for the rest (build
progress, summaries, classic fallback notes). The lines that `--timings`
and `IETSP_STATS` add stay plain text. The activity records of Nix
(`start`, `stop`, `result`) are not produced yet; they will go out
through `log::out` and `log::tee` in `src/log.rs`.

## `json-log-path`

The `json-log-path` setting (nix.conf, `--option json-log-path PATH` or
`--json-log-path PATH`) names a file or a unix socket. Every record above
also goes there, without the `@nix ` prefix, whatever `--log-format` says;
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

A missing attribute or an undefined variable has no hint: ietsp has no
did-you-mean suggestion for them yet.

# Settings

Every `IETSP_*` environment variable, every `ietsp.conf` key, the
`--ietsp-option` flag, and how ietsp reads Nix's own `nix.conf`.

ietsp reads two settings files. `nix.conf` is Nix's, shared with the real
nix behind the classic fallback; `ietsp.conf` is ietsp's own, and no nix
ever sees it. Both use the same line syntax.

Every `IETSP_*` variable is read once at startup into one `Settings`
struct (`src/settings.rs`), together with the `ietsp.conf` files and the
`--ietsp-option` flag. A `Session` owns it, not a static: read it with
`ev.session.settings()` where an evaluator is at hand, else
`session::Session::current().settings()` (`docs/reference/library.md`).
`Session::load_settings` reads the files; until then a session has the
environment knobs alone. Nothing reads the environment at its point of
use, so a knob cannot change halfway through a run.

## Environment variables

A variable with a **File** entry is also an `ietsp.conf` key, under the
kebab-case name given. The variable wins over the file: the file is the
configuration, the environment is the override. The perf-tuning knobs
(`IETSP_SPIN`, `IETSP_SPINNERS`, `IETSP_HANDOFF`, `IETSP_FANOUT`) and the
statistics knobs stay environment-only.

A level knob (`IETSP_STATS`, `IETSP_PAR_STATS`) is off when unset, `2`
selects level 2, and any other value selects level 1.

| Variable | File | Type | Default | Effect |
|:---|:---|:---|:---|:---|
| `IETSP_WORKERS` | `workers` | usize ≥ 1 | cores, capped at 4 | Worker threads. `--workers N` overrides it. |
| `IETSP_FIBER_STACK_MB` | `fiber-stack-mb` | usize (MiB) | 256 | Stack of every fiber. The reservation is `MAP_NORESERVE`, and workers × stack may not pass 64 GiB. |
| `IETSP_SPIN` | | u32 | 2000 | Scans an idle searcher makes before it parks (one scan is 64 polls). |
| `IETSP_SPINNERS` | | usize | workers / 4, at least 1 | Idle workers that may search for work at once. |
| `IETSP_HANDOFF` | | usize | 8 | Queued tasks per awake worker that justify waking a sleeper. |
| `IETSP_FANOUT` | | u32 bitmask | 7 | Strict-demand fan-out: 1 deep walks, 2 coercions, 4 list builtins. |
| `IETSP_PAR_STATS` | | level | 0 | 1 prints the scheduler summary, 2 adds the sample timeline and the file table. |
| `IETSP_STATS` | | level | 0 | 1 prints the evaluator counters, 2 adds the memory census (`stats2` feature). |
| `IETSP_CACHE` | `cache` | `0` turns off | on | The tracing cache and the IR cache. Same as `--no-cache --no-ir-cache`. |
| `IETSP_CACHE_DIR` | `cache-dir` | path | `$XDG_CACHE_HOME/ietsp` | Cache root, for the tracing cache, the IR cache and the input store. `--cache-dir DIR` overrides it. |
| `IETSP_IR_STATS` | | set = on | off | One line per compiled file on stderr. |
| `IETSP_IR_MIN` | | usize (bytes) | 1024 | Sources smaller than this compile faster than their cache entry loads, so they are not cached. |
| `IETSP_PROVENANCE` | `provenance` | `0` turns off | on | Rewrites store paths and positions back to local trees (`docs/explanation/provenance.md`). |
| `IETSP_NIX_BIN_DIR` | `nix-bin-dir` | path | unset | Where the real Nix binaries live. Unset searches `PATH`, skipping every ietsp. |
| `IETSP_NO_FALLBACK` | `no-fallback` | `1` turns on | off | A classic command fails instead of running the real Nix (`docs/reference/classic.md`). |
| `IETSP_FETCHING` | | set = on | off | Set on the Nix subprocess that fetches a flake input, so a recursive fetch is an error, not a loop. |
| `IETSP_OFFLINE` | | set = on | off | Any download is an error. A flake whose inputs the input store already holds still evaluates. |
| `IETSP_SESSION_ID` | | string | a new UUIDv7 | The `sid` of every JSON log record (`docs/reference/errors.md`). `eval-jobs` sets it on its workers. `src/log.rs` reads it, not `Settings`: a settings error is itself a record. |
| | `fix-hashes` | `ask`, `always`, `never` | `ask` | See **fix-hashes** below. |

`IETSP_BUILD_ID` is not a setting: the build script bakes it in
(`src/trace/key.rs`), and it keys the caches to the binary.

### fix-hashes

`ask` asks `[y/n/a]` for each fix when stdin and stderr are terminals and
the log format is not `internal-json`; `a` takes this fix and the rest of
the run. Elsewhere `ask` only prints the hint. `always` fixes without
asking, for CI and agents. `never` never fixes.

With a fix, `ietsp build` rewrites the hash of a fixed-output build that
failed on it and builds again. The old hash must appear once, in any
spelling, in the local files evaluation parsed; store paths are never
edited. A derivation with inputs is built as a resolved copy of itself,
whose mismatch the daemon reports as a line of its own; that one is fixed
too. It stops when a hash it wrote mismatches again (a build that is not
reproducible). Each fix is a `hash-fixed` record
(`docs/reference/errors.md`). After a mismatch, one more evaluation
without the tracing cache lists the files. Files read with `readFile` or
`importJSON` are not searched.

## `--ietsp-option`

`--ietsp-option NAME VALUE` sets any `ietsp.conf` key for one run. Every
ietsp command and every classic name accepts it, and it never reaches the
real nix: it leaves the argv before a classic fallback execs
(`src/main.rs`).

# `ietsp.conf`

`src/settings.rs` reads ietsp's own settings. The line syntax is
nix.conf's, parsed by the same code: `name = value`, `include` and
`!include`, `#` comments, and `extra-<name>` appending to a list setting.

Nothing but ietsp reads this file, so a host can send ietsp's builds
somewhere without touching `nix.conf`, which the real nix behind the
classic fallback also reads.

## Files and order

Later wins.

1. `$IETSP_CONF_DIR/ietsp.conf`, default `/etc/ietsp/ietsp.conf`.
2. `$XDG_CONFIG_HOME/ietsp/ietsp.conf`, default
   `~/.config/ietsp/ietsp.conf`.
3. `$IETSP_CONFIG`, treated as one more file's text.
4. The `--ietsp-option name value` pairs from the command line.

A file that is missing or unreadable is skipped. An **unknown key is an
error** naming the file and the line: this file is ietsp's, so a name it
does not know is a mistake, not a setting for somebody else.

## Keys

Every key in the **File** column above, with the type and default given
there, plus three keys that have no environment variable:

| Key | Type | Default |
|:---|:---|:---|
| `builder` | URL: `daemon`, `unix://PATH`, `ssh-ng://[user@]host`, `grpc://host:port`; not used by the shell commands (`docs/reference/classic.md`) | the nix.conf `store` setting (itself `auto`) |
| `source-map` | list of `STOREPATH=LOCALDIR` | empty |
| `fetch-max-size` | bytes | 17179869184 (16 GiB) |

`extra-source-map` appends; no other key is a list. `fetch-max-size` caps
the decompressed bytes of one flake input download; a stream that
decompresses over 1000 times its size past its first 16 MiB is refused
too.

ietsp has no local store of its own, so `auto` and an empty value both
mean the local daemon.

`builder` is where derivations are realised. Every command takes it:
`ietsp build --builder`, `ietsp print-dev-env --builder` and `ietsp
fast-build --store` win over it, and so does a classic `--store` or
`--option store`. `nix-build`, `nix build`, `nix-shell` and `nix develop`
have no flag of their own and use it as it stands. Registration is not
affected: drvs and sources for `nix-instantiate --add-root`, its GC roots
and the `nix-build`/`nix build` out-links always go to the local daemon,
and a remote `builder`'s outputs are copied back first
(`docs/reference/classic.md`).

## The NixOS module

`ietsp.settings` is an attribute set of key to string, bool, int or list
of strings, rendered to `/etc/ietsp/ietsp.conf`:

```nix
{
  imports = [ ietsp.nixosModules.default ];
  ietsp.settings.builder = "ssh-ng://bld1";
}
```

# `nix.conf`

`src/config.rs` reads Nix's settings files. It copies Nix 2.34.8:
`src/libstore/globals.cc:loadConfFile` for discovery,
`src/libutil/configuration.cc:parseConfigFiles` for the line syntax.

## Files and order

`Config::load(overrides)` applies these in order. Later wins.

1. `$NIX_CONF_DIR/nix.conf`, default `/etc/nix/nix.conf`.
2. The user files, lowest precedence first. `loadConfFile` walks the list
   backwards (`globals.cc:143`), so the **first** entry of the list below
   has the highest precedence:
   - `$NIX_USER_CONF_FILES`, colon-separated, replaces the whole list;
     else
   - `$NIX_CONFIG_HOME/nix.conf` or `$XDG_CONFIG_HOME/nix/nix.conf`
     (default `~/.config/nix/nix.conf`), then
   - `<dir>/nix/nix.conf` for each `$XDG_CONFIG_DIRS` entry (default
     `/etc/xdg`).
3. `$NIX_CONFIG`, treated as one more file's text.
4. The `--option name value` pairs from the command line.

A file that is missing or unreadable is skipped, not an error.

## Line syntax

- `#` starts a comment; the rest of the line is dropped.
- `name = value`. The value is the rest of the line, with runs of
  whitespace collapsed to one space.
- `include path` and `!include path`, relative to the including file. A
  missing `include` is an error (`file '...' included from '...' not
  found`); a missing `!include` is silent.
- `extra-<name> = value` appends to a list setting. For a setting that is
  not a list, the pair is kept under its literal name, as Nix keeps it in
  `unknownSettings`.
- An unknown setting is kept and readable with `Config::str`. Nix warns
  about it; this reader is silent, except for the Determinate Nix
  settings below.

## Values

Raw strings, with typed accessors: `str`, `int`, `bool` (`true`/`yes`/`1`
and `false`/`no`/`0`) and `list` (whitespace-separated). Typed getters
carry Nix's defaults: `store` (`auto`), `system` (the build target),
`max_jobs` (`auto` = CPU count), `cores` (`0` = all), `keep_going` and
`keep_failed`.

`$NIX_PATH` replaces the `nix-path` setting, as `eval-gc.cc:129` does.

## What is honoured

`worker::Options` carries `max-jobs`, `cores`, `keep-going`,
`keep-failed` and `substitute` into the daemon's `SetOptions`
(`remote-store.cc:setOptions`), built with `Options::try_from(&config)`.
`crate::cli::classic` uses it, and takes `store` as the last fallback for
`builder` and `system` for `builtins.currentSystem`
(`docs/reference/classic.md`). The other subcommands read `system` and
`store` only and send `Options::default()`. The flake input fetcher reads
`access-tokens`, `connect-timeout` (default 15 s) and
`stalled-download-timeout` (default 300 s); 0 is no limit, as in
`filetransfer.hh`. Every other setting is read and kept, and nothing in
ietsp consults it.

## Determinate Nix settings

A host with Determinate Nix often has settings in its `nix.conf` that Nix
2.34.8 does not know: `lazy-trees`, `eval-cores`, `session-id` and the
rest of `DETERMINATE` in `src/config.rs` (the `Setting`s of
DeterminateSystems/nix-src that upstream Nix does not have). Nix 2.34.8
prints `warning: unknown setting 'NAME'` for each
(configuration.cc:`warnUnknownSettings`). ietsp prints one warning per
key and process instead, and ignores the value:

```
warning: 'lazy-trees' is a Determinate Nix setting; ietsp ignores it
```

Under `--log-format internal-json` it is a level-1 record with code
`determinate-setting` (`docs/reference/errors.md`). The same holds when
the setting comes from `$NIX_CONFIG`, from `--option NAME VALUE`, or from
its flag form: `--lazy-trees` and `--no-lazy-trees` for a Boolean,
`--eval-cores N` for the others (configuration.cc:`convertToArg`). The
warning goes out once every flag is read, so a later `--log-format`
applies to it. `eval-jobs` workers do not repeat their parent's warnings.

Nothing in ietsp reads these settings, and none of them reaches a daemon:
`SetOptions` carries the fixed set above. So they cannot change a result.
`eval-cores` does not set `workers`, and `session-id` does not set the
`sid` of the log records (`IETSP_SESSION_ID` does).

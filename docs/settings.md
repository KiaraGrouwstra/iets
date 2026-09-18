# Environment settings

Every `IETSP_*` variable is read once at startup into one `Settings`
struct (`src/settings.rs`), together with the `ietsp.conf` files and the
`--ietsp-option` flag (`docs/config.md`). Read it with
`crate::settings::get()`. Nothing reads the environment at its point of use, so
a knob cannot change halfway through a run.

A variable in the **File** column below is also an `ietsp.conf` key, under the
kebab-case name given. The variable wins over the file: the file is the
configuration, the environment is the override. The perf-tuning knobs
(`IETSP_SPIN`, `IETSP_SPINNERS`, `IETSP_HANDOFF`, `IETSP_FANOUT`, `IETSP_SPEC*`)
and the statistics knobs stay environment-only.

`--ietsp-option name value` sets any `ietsp.conf` key for one run. Every ietsp
command and every classic name accepts it, and it never reaches the real nix: it
leaves the argv before a classic fallback execs (`src/main.rs`).

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
| `IETSP_SPEC` | | u32 bitmask | 0 (off) | Creation-time speculation: 1 thunk bodies, 2 `map`/`mapAttrs` applications. |
| `IETSP_SPEC_MIN` | | u32 | 24 | IR nodes a body needs before speculation is worth a fiber. |
| `IETSP_SPEC_BUDGET` | | u64 | 4000 | Thunk creations a speculative task may make before it bails. |
| `IETSP_STATS` | | level | 0 | 1 prints the evaluator counters, 2 adds the memory census (`stats2` feature). |
| `IETSP_CACHE` | `cache` | `0` turns off | on | The tracing cache and the IR cache. Same as `--no-cache --no-ir-cache`. |
| `IETSP_CACHE_DIR` | `cache-dir` | path | `$XDG_CACHE_HOME/ietsp` | Cache root. `--cache-dir DIR` overrides it. |
| `IETSP_IR_STATS` | | set = on | off | One line per compiled file on stderr. |
| `IETSP_IR_MIN` | | usize (bytes) | 1024 | Sources smaller than this compile faster than their cache entry loads, so they are not cached. |
| `IETSP_PROVENANCE` | `provenance` | `0` turns off | on | Rewrites store paths and positions back to local trees (docs/provenance.md). |
| `IETSP_NIX_BIN_DIR` | `nix-bin-dir` | path | unset | Where the real Nix binaries live. Unset searches `PATH`, skipping this binary. |
| `IETSP_NO_FALLBACK` | `no-fallback` | `1` turns on | off | A classic command fails instead of running the real Nix (docs/classic.md). |
| `IETSP_FETCHING` | | set = on | off | Set on the Nix subprocess that fetches a flake input, so a recursive fetch is an error, not a loop. |

Two keys are file-only: `builder` and `source-map` (`docs/config.md`).

`IETSP_BUILD_ID` is not a setting: the build script bakes it in
(`src/trace/key.rs`), and it keys the caches to the binary.

//! Every `IETSP_*` environment knob, read once into one struct. Each
//! variable, its type, its default and its effect: docs/settings.md.

use std::path::PathBuf;
use std::str::FromStr;
use std::sync::OnceLock;

/// The environment as this process started. Defaults match the behaviour
/// each knob had when it was read at its point of use.
pub struct Settings {
    /// `IETSP_WORKERS`: worker threads. `None` asks the machine.
    pub workers: Option<usize>,
    /// `IETSP_FIBER_STACK_MB`: fiber stack size in MiB.
    pub fiber_stack_mb: Option<usize>,
    /// `IETSP_SPIN`: scans an idle searcher makes before it parks.
    pub spin: u32,
    /// `IETSP_SPINNERS`: idle workers that may search at once. `None` is
    /// a quarter of the workers.
    pub spinners: Option<usize>,
    /// `IETSP_HANDOFF`: queued tasks per awake worker that wake a sleeper.
    pub handoff: usize,
    /// `IETSP_FANOUT` bitmask: 1 deep, 2 coerce, 4 list builtins.
    pub fanout: u32,
    /// `IETSP_PAR_STATS`: 1 the summary line, 2 adds the timeline.
    pub par_stats: u8,
    /// `IETSP_SPEC` bitmask: 1 thunk bodies, 2 map/mapAttrs applications.
    pub spec: u32,
    /// `IETSP_SPEC_MIN`: IR nodes a body needs to be worth speculating.
    pub spec_min: u32,
    /// `IETSP_SPEC_BUDGET`: thunk creations a speculative task may make.
    pub spec_budget: u64,
    /// `IETSP_STATS`: 1 the evaluator counters, 2 adds the memory census.
    pub stats: u8,
    /// `IETSP_CACHE`: `0` turns the tracing and IR caches off.
    pub cache: bool,
    /// `IETSP_CACHE_DIR`: cache root, below `--cache-dir`.
    pub cache_dir: Option<PathBuf>,
    /// `IETSP_IR_STATS`: one line per compiled file on stderr.
    pub ir_stats: bool,
    /// `IETSP_IR_MIN`: sources below this size are not cached.
    pub ir_min: usize,
    /// `IETSP_PROVENANCE`: `0` prints plain store paths.
    pub provenance: bool,
    /// `IETSP_NIX_BIN_DIR`: where the real Nix binaries live.
    pub nix_bin_dir: Option<PathBuf>,
    /// `IETSP_NO_FALLBACK`: `1` fails instead of running the real Nix.
    pub no_fallback: bool,
    /// `IETSP_FETCHING`: set on the Nix we call to fetch a flake input, so
    /// a recursive fetch is an error instead of a loop.
    pub fetching: bool,
}

/// The settings of this process.
pub fn get() -> &'static Settings {
    static S: OnceLock<Settings> = OnceLock::new();
    S.get_or_init(Settings::from_env)
}

impl Settings {
    pub fn from_env() -> Settings {
        Settings {
            workers: num("IETSP_WORKERS").filter(|n| *n >= 1),
            fiber_stack_mb: num("IETSP_FIBER_STACK_MB"),
            spin: num("IETSP_SPIN").unwrap_or(2000),
            spinners: num("IETSP_SPINNERS"),
            handoff: num("IETSP_HANDOFF").unwrap_or(8),
            fanout: num("IETSP_FANOUT").unwrap_or(7),
            par_stats: level("IETSP_PAR_STATS"),
            spec: num("IETSP_SPEC").unwrap_or(0),
            spec_min: num("IETSP_SPEC_MIN").unwrap_or(24),
            spec_budget: num("IETSP_SPEC_BUDGET").unwrap_or(4000),
            stats: level("IETSP_STATS"),
            cache: !is("IETSP_CACHE", "0"),
            cache_dir: dir("IETSP_CACHE_DIR"),
            ir_stats: set("IETSP_IR_STATS"),
            ir_min: num("IETSP_IR_MIN").unwrap_or(1024),
            provenance: !is("IETSP_PROVENANCE", "0"),
            nix_bin_dir: dir("IETSP_NIX_BIN_DIR"),
            no_fallback: is("IETSP_NO_FALLBACK", "1"),
            fetching: set("IETSP_FETCHING"),
        }
    }
}

fn num<T: FromStr>(name: &str) -> Option<T> {
    std::env::var(name).ok().and_then(|v| v.parse().ok())
}

/// A level knob: unset is 0, `2` is 2, any other value is 1. Two levels
/// grew out of one `is_some` check, so anything set means at least 1.
fn level(name: &str) -> u8 {
    match std::env::var_os(name) {
        None => 0,
        Some(v) if v == "2" => 2,
        Some(_) => 1,
    }
}

fn set(name: &str) -> bool {
    std::env::var_os(name).is_some()
}

fn is(name: &str, want: &str) -> bool {
    std::env::var_os(name).is_some_and(|v| v == want)
}

fn dir(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).map(PathBuf::from)
}

//! ietsp's own settings: the `ietsp.conf` files, `--ietsp-option` and every
//! `IETSP_*` environment knob, read once into one struct. Each key, its type,
//! its default and its effect: docs/settings.md.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::OnceLock;

use crate::error::{Error, Result};

/// Keys an `ietsp.conf` may set. Nothing else: the file is ours, so a name
/// that is not here is a mistake, not a setting for someone else.
const KEYS: &[&str] = &[
    "builder",
    "cache",
    "cache-dir",
    "fetch-max-size",
    "fiber-stack-mb",
    "nix-bin-dir",
    "no-fallback",
    "provenance",
    "source-map",
    "workers",
];

/// Keys whose value is a whitespace-separated list, so `extra-<name>` appends.
const APPENDABLE: &[&str] = &["source-map"];

/// The settings of this process: the files, then the environment for the knobs
/// that have one. Defaults match the behaviour each knob had when it was read
/// at its point of use.
pub struct Settings {
    /// `builder`: where derivations are realised. `None` falls back to the
    /// nix.conf `store` setting; resolve it with [`builder`].
    pub builder: Option<String>,
    /// `source-map`: `STOREPATH=LOCALDIR` pairs (docs/provenance.md).
    pub source_map: Vec<(String, String)>,
    /// `workers`, `IETSP_WORKERS`: worker threads. `None` asks the machine.
    pub workers: Option<usize>,
    /// `fiber-stack-mb`, `IETSP_FIBER_STACK_MB`: fiber stack size in MiB.
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
    /// `IETSP_STATS`: 1 the evaluator counters, 2 adds the memory census.
    pub stats: u8,
    /// `cache`, `IETSP_CACHE`: the tracing and IR caches.
    pub cache: bool,
    /// `cache-dir`, `IETSP_CACHE_DIR`: cache root, below `--cache-dir`.
    pub cache_dir: Option<PathBuf>,
    /// `IETSP_IR_STATS`: one line per compiled file on stderr.
    pub ir_stats: bool,
    /// `IETSP_IR_MIN`: sources below this size are not cached.
    pub ir_min: usize,
    /// `provenance`, `IETSP_PROVENANCE`: `false` prints plain store paths.
    pub provenance: bool,
    /// `nix-bin-dir`, `IETSP_NIX_BIN_DIR`: where the real Nix binaries live.
    pub nix_bin_dir: Option<PathBuf>,
    /// `no-fallback`, `IETSP_NO_FALLBACK`: fail instead of running the real Nix.
    pub no_fallback: bool,
    /// `IETSP_FETCHING`: set on the Nix we call to fetch a flake input, so
    /// a recursive fetch is an error instead of a loop.
    pub fetching: bool,
    /// `IETSP_OFFLINE`: any download is an error. A flake whose inputs the
    /// input store already holds evaluates; one that would fetch fails and says
    /// which URL it wanted.
    pub offline: bool,
    /// `fetch-max-size`: the most bytes one download may decompress to.
    pub fetch_max_size: u64,
}

/// The `--ietsp-option` pairs of this command line, recorded by
/// [`take_options`] before anything reads a setting.
static CLI: OnceLock<Vec<(String, String)>> = OnceLock::new();

/// The flag every ietsp command and every classic name accepts. It is ours, so
/// it never reaches the real nix: [`take_options`] removes it from the argv a
/// fallback execs.
pub const FLAG: &str = "--ietsp-option";

/// `argv` without its `--ietsp-option name value` triples, whose pairs are
/// recorded for [`get`]. Call it before any other setting is read.
pub fn take_options(argv: &[String]) -> Result<Vec<String>> {
    let mut rest = Vec::with_capacity(argv.len());
    let mut opts = Vec::new();
    let mut i = 0;
    while i < argv.len() {
        if argv[i] != FLAG {
            rest.push(argv[i].clone());
            i += 1;
            continue;
        }
        let (Some(name), Some(value)) = (argv.get(i + 1), argv.get(i + 2)) else {
            return Err(Error::Usage(format!("'{FLAG}' wants a name and a value")));
        };
        opts.push((name.clone(), value.clone()));
        i += 3;
    }
    let _ = CLI.set(opts);
    Ok(rest)
}

/// The settings of this process. A broken settings file is fatal here: every
/// caller is past the point where it could report the error itself.
pub fn get() -> &'static Settings {
    static S: OnceLock<Settings> = OnceLock::new();
    S.get_or_init(|| {
        Settings::load(CLI.get().map_or(&[], Vec::as_slice))
            .unwrap_or_else(|e| std::process::exit(e.report()))
    })
}

/// Where a command builds: its own flag, else the `builder` setting, else the
/// nix.conf `store` setting.
pub fn builder(flag: Option<&str>, cfg: &crate::config::Config) -> String {
    flag.map(str::to_string)
        .or_else(|| get().builder.clone())
        .unwrap_or_else(|| cfg.store())
}

/// `$IETSP_CONF_DIR` or the compiled-in `/etc/ietsp`.
pub fn conf_dir() -> PathBuf {
    env("IETSP_CONF_DIR").map_or_else(|| PathBuf::from("/etc/ietsp"), PathBuf::from)
}

/// The settings files, lowest precedence first.
pub fn conf_files() -> Vec<PathBuf> {
    vec![
        conf_dir().join("ietsp.conf"),
        crate::config::config_home().join("ietsp/ietsp.conf"),
    ]
}

impl Settings {
    /// The files, then `$IETSP_CONFIG`, then `overrides`, then the environment
    /// for the knobs that have a variable. Later wins.
    pub fn load(overrides: &[(String, String)]) -> Result<Settings> {
        let mut c = Conf::default();
        for f in conf_files() {
            if let Ok(text) = std::fs::read_to_string(&f) {
                c.apply(&text, &f)?;
            }
        }
        if let Some(text) = env("IETSP_CONFIG") {
            c.apply(&text, Path::new("IETSP_CONFIG"))?;
        }
        for (name, value) in overrides {
            c.set(name, value, FLAG)?;
        }
        let source_map: Vec<(String, String)> = c
            .list("source-map")
            .iter()
            .filter_map(|e| e.split_once('='))
            .map(|(s, l)| (s.to_string(), l.to_string()))
            .collect();
        for (_, local) in &source_map {
            if !Path::new(local).is_dir() {
                eprintln!("warning: source-map: '{local}' is not a directory");
            }
        }
        Ok(Settings {
            builder: c.str("builder").map(str::to_string),
            source_map,
            workers: num("IETSP_WORKERS")
                .or(c.num("workers")?)
                .filter(|n| *n >= 1),
            fiber_stack_mb: num("IETSP_FIBER_STACK_MB").or(c.num("fiber-stack-mb")?),
            spin: num("IETSP_SPIN").unwrap_or(2000),
            spinners: num("IETSP_SPINNERS"),
            handoff: num("IETSP_HANDOFF").unwrap_or(8),
            fanout: num("IETSP_FANOUT").unwrap_or(7),
            par_stats: level("IETSP_PAR_STATS"),
            stats: level("IETSP_STATS"),
            cache: flag("IETSP_CACHE", "0", c.bool("cache")?, true),
            cache_dir: dir("IETSP_CACHE_DIR").or_else(|| c.str("cache-dir").map(PathBuf::from)),
            ir_stats: set("IETSP_IR_STATS"),
            ir_min: num("IETSP_IR_MIN").unwrap_or(1024),
            provenance: flag("IETSP_PROVENANCE", "0", c.bool("provenance")?, true),
            nix_bin_dir: dir("IETSP_NIX_BIN_DIR")
                .or_else(|| c.str("nix-bin-dir").map(PathBuf::from)),
            no_fallback: flag("IETSP_NO_FALLBACK", "1", c.bool("no-fallback")?, false),
            fetching: set("IETSP_FETCHING"),
            offline: set("IETSP_OFFLINE"),
            fetch_max_size: c.num("fetch-max-size")?.unwrap_or(16 << 30),
        })
    }
}

/// One setting, with the place it was set for an error message.
#[derive(Debug)]
struct Value {
    value: String,
    at: String,
}

/// What the settings files said, with the nix.conf line syntax
/// (`crate::config::parse`) and `extra-<name>` appending to a list.
#[derive(Debug, Default)]
struct Conf {
    at: BTreeMap<String, Value>,
}

impl Conf {
    fn apply(&mut self, text: &str, origin: &Path) -> Result<()> {
        for e in crate::config::parse(text, origin)? {
            self.set(&e.name, &e.value, &e.at())?;
        }
        Ok(())
    }

    fn set(&mut self, name: &str, value: &str, at: &str) -> Result<()> {
        let base = name.strip_prefix("extra-").unwrap_or(name);
        if !KEYS.contains(&base) || (base != name && !APPENDABLE.contains(&base)) {
            return Err(Error::Config(format!(
                "unknown ietsp setting '{name}' in {at}"
            )));
        }
        if base != name {
            let slot = self.at.entry(base.to_string()).or_insert_with(|| Value {
                value: String::new(),
                at: at.to_string(),
            });
            if !slot.value.is_empty() && !value.is_empty() {
                slot.value.push(' ');
            }
            slot.value.push_str(value);
            slot.at = at.to_string();
            return Ok(());
        }
        self.at.insert(
            name.to_string(),
            Value {
                value: value.to_string(),
                at: at.to_string(),
            },
        );
        Ok(())
    }

    fn str(&self, name: &str) -> Option<&str> {
        self.at.get(name).map(|v| v.value.as_str())
    }

    fn list(&self, name: &str) -> Vec<String> {
        self.str(name)
            .into_iter()
            .flat_map(str::split_whitespace)
            .map(str::to_string)
            .collect()
    }

    fn num<T: FromStr>(&self, name: &str) -> Result<Option<T>> {
        match self.at.get(name) {
            None => Ok(None),
            Some(v) => v.value.trim().parse().map(Some).map_err(|_| {
                Error::Config(format!(
                    "ietsp setting '{name}' wants a number, got '{}' in {}",
                    v.value, v.at
                ))
            }),
        }
    }

    fn bool(&self, name: &str) -> Result<Option<bool>> {
        match self.at.get(name) {
            None => Ok(None),
            Some(v) => match v.value.as_str() {
                "true" | "yes" | "1" => Ok(Some(true)),
                "false" | "no" | "0" => Ok(Some(false)),
                s => Err(Error::Config(format!(
                    "ietsp setting '{name}' wants a boolean, got '{s}' in {}",
                    v.at
                ))),
            },
        }
    }
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|s| !s.is_empty())
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

/// The environment wins over the file, which wins over `default`. `switch` is
/// the value of `name` that means "the opposite of the default".
fn flag(name: &str, switch: &str, file: Option<bool>, default: bool) -> bool {
    match std::env::var_os(name) {
        Some(v) if v == switch => !default,
        Some(_) => default,
        None => file.unwrap_or(default),
    }
}

fn dir(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load(text: &str) -> Result<Conf> {
        let mut c = Conf::default();
        c.apply(text, Path::new("t.conf"))?;
        Ok(c)
    }

    #[test]
    fn unknown_key_names_the_file_and_line() {
        let e = load("builder = daemon\nnope = 1\n")
            .unwrap_err()
            .to_string();
        assert!(e.contains("unknown ietsp setting 'nope'"), "{e}");
        assert!(e.contains("t.conf:2"), "{e}");
    }

    #[test]
    fn extra_appends_to_a_list_key_only() {
        let c = load("source-map = /nix/store/a=/a\nextra-source-map = /nix/store/b=/b\n").unwrap();
        assert_eq!(
            c.list("source-map"),
            vec!["/nix/store/a=/a", "/nix/store/b=/b"]
        );
        let e = load("extra-workers = 2\n").unwrap_err().to_string();
        assert!(e.contains("unknown ietsp setting 'extra-workers'"), "{e}");
    }

    #[test]
    fn later_files_win() {
        let mut c = load("builder = daemon\n").unwrap();
        c.apply("builder = ssh-ng://bld1\n", Path::new("u.conf"))
            .unwrap();
        assert_eq!(c.str("builder"), Some("ssh-ng://bld1"));
    }

    #[test]
    fn typed_values_report_where_they_were_set() {
        let c = load("workers = many\n").unwrap();
        let e = c.num::<usize>("workers").unwrap_err().to_string();
        assert!(e.contains("wants a number"), "{e}");
        assert!(e.contains("t.conf:1"), "{e}");
        let c = load("cache = maybe\n").unwrap();
        let e = c.bool("cache").unwrap_err().to_string();
        assert!(e.contains("wants a boolean"), "{e}");
        let c = load("cache = no\nprovenance = 1\n").unwrap();
        assert_eq!(c.bool("cache").unwrap(), Some(false));
        assert_eq!(c.bool("provenance").unwrap(), Some(true));
    }

    #[test]
    fn the_flag_leaves_the_argv() {
        let argv: Vec<String> = ["--parse", FLAG, "workers", "2", "-E", "1"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(take_options(&argv).unwrap(), vec!["--parse", "-E", "1"]);
        let bad: Vec<String> = [FLAG, "workers"].iter().map(|s| s.to_string()).collect();
        assert!(take_options(&bad).is_err());
    }

    #[test]
    fn the_environment_wins_over_the_file() {
        const UNSET: &str = "IETSP_NOT_SET_ANYWHERE";
        assert!(flag(UNSET, "0", None, true));
        assert!(!flag(UNSET, "0", Some(false), true));
        assert!(flag(UNSET, "1", Some(true), false));
        // A variable every test process has, so the file value cannot win.
        assert!(!flag(
            "PATH",
            &std::env::var("PATH").unwrap(),
            Some(true),
            true
        ));
    }

    #[test]
    fn the_file_list_is_etc_then_xdg() {
        let files = conf_files();
        assert_eq!(files.len(), 2);
        assert!(files[0].ends_with("ietsp.conf"), "{files:?}");
        assert!(files[1].ends_with("ietsp/ietsp.conf"), "{files:?}");
    }
}

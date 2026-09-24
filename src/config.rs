//! Nix's settings files, read the way Nix reads them.
//!
//! Discovery follows libstore/globals.cc:loadConfFile; the line syntax follows
//! libutil/configuration.cc:parseConfigFiles. Every value stays a raw string:
//! the typed getters below apply the same parse rules Nix's `BaseSetting`
//! specialisations do. Settings this crate does not know are kept, not dropped
//! (Nix warns about them and keeps them in `unknownSettings`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

/// Settings whose values are whitespace-separated lists, so `extra-<name>`
/// appends instead of replacing (configuration.cc:Config::set, `isAppendable`).
const APPENDABLE: &[&str] = &[
    "access-tokens",
    "allowed-uris",
    "allowed-users",
    "experimental-features",
    "extra-platforms",
    "hashed-mirrors",
    "nix-path",
    "plugin-files",
    "sandbox-paths",
    "secret-key-files",
    "substituters",
    "system-features",
    "trusted-public-keys",
    "trusted-substituters",
    "trusted-users",
];

/// The settings Determinate Nix has and Nix 2.34.8 does not (the `Setting`s
/// of DeterminateSystems/nix-src, 2026-09): the Booleans, then the rest. Each
/// is warned about once and never read (docs/config.md).
const DETERMINATE_BOOLS: &[&str] = &[
    "filetransfer-retry-jitter",
    "http3",
    "lazy-locks",
    "lazy-trees",
    "nix-219-compat",
    "otlp",
];
const DETERMINATE_VALUES: &[&str] = &[
    "build-provenance-tags",
    "eval-cores",
    "eval-max-fibers",
    "filetransfer-retry-attempts",
    "filetransfer-retry-delay",
    "filetransfer-retry-delay-rate-limited",
    "filetransfer-retry-max-delay",
    "host-name",
    "otlp-compression",
    "otlp-endpoint",
    "otlp-headers",
    "session-id",
];

/// The Determinate settings a run saw, and whether each was warned about,
/// plus `hushed`. The session owns it (`crate::session`).
#[derive(Default)]
pub struct Determinate {
    hushed: std::sync::atomic::AtomicBool,
    noted: std::sync::Mutex<Vec<(String, bool)>>,
}

/// No Determinate warnings from this run: an `eval-jobs` worker, whose
/// parent already gave them.
pub fn hush() {
    crate::session::Session::current()
        .determinate
        .hushed
        .store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Remembers `name` when it is a Determinate Nix setting. The warning waits
/// for `warn_determinate`, so a later `--log-format` still applies to it, as
/// Nix warns only once every flag is read (common-args.cc:initialFlagsProcessed).
pub fn note_determinate(name: &str) {
    let name = name.strip_prefix("extra-").unwrap_or(name);
    if DETERMINATE_BOOLS.contains(&name) || DETERMINATE_VALUES.contains(&name) {
        let s = crate::session::Session::current();
        let mut noted = s
            .determinate
            .noted
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if !noted.iter().any(|(n, _)| n == name) {
            noted.push((name.to_string(), false));
        }
    }
}

/// `warning: 'NAME' is a Determinate Nix setting; ietsp ignores it` for each
/// noted name not yet warned about, as a `determinate-setting` record.
pub fn warn_determinate() {
    let s = crate::session::Session::current();
    let hushed = s
        .determinate
        .hushed
        .load(std::sync::atomic::Ordering::Relaxed);
    let mut noted = s
        .determinate
        .noted
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    for (name, warned) in noted.iter_mut().filter(|(_, w)| !*w) {
        *warned = true;
        if !hushed {
            let raw = format!("'{name}' is a Determinate Nix setting; ietsp ignores it");
            crate::log::warning("determinate-setting", raw);
        }
    }
}

/// A Determinate Nix setting as a flag (configuration.cc, `convertToArg`):
/// `--lazy-trees` and `--no-lazy-trees` for a Boolean, `--eval-cores N` for
/// the rest. It is noted and ignored; `Ok(false)` for any other flag.
pub fn determinate_flag(
    flag: &str,
    mut next: impl FnMut() -> std::result::Result<String, String>,
) -> std::result::Result<bool, String> {
    let Some(name) = flag.strip_prefix("--") else {
        return Ok(false);
    };
    let boolean = name.strip_prefix("no-").unwrap_or(name);
    if DETERMINATE_BOOLS.contains(&boolean) {
        note_determinate(boolean);
    } else if DETERMINATE_VALUES.contains(&name) {
        next()?;
        note_determinate(name);
    } else {
        return Ok(false);
    }
    Ok(true)
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|s| !s.is_empty())
}

fn home() -> PathBuf {
    env("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from)
}

/// `$XDG_CONFIG_HOME` or `~/.config` (unix/xdg-dirs.cc:getConfigHome).
pub fn config_home() -> PathBuf {
    env("XDG_CONFIG_HOME").map_or_else(|| home().join(".config"), PathBuf::from)
}

/// `$XDG_CONFIG_DIRS` or `/etc/xdg` (unix/xdg-dirs.cc:getConfigDirs).
fn config_dirs() -> Vec<PathBuf> {
    env("XDG_CONFIG_DIRS")
        .unwrap_or_else(|| "/etc/xdg".into())
        .split(':')
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .collect()
}

/// `$NIX_CONF_DIR` or the compiled-in `/etc/nix` (globals.cc:nixConfDir).
pub fn conf_dir() -> PathBuf {
    env("NIX_CONF_DIR").map_or_else(|| PathBuf::from("/etc/nix"), PathBuf::from)
}

/// globals.cc:nixUserConfFiles. `$NIX_USER_CONF_FILES` replaces the XDG list
/// wholesale. The list is in precedence order: the *first* entry wins, because
/// loadConfFile applies it last (globals.cc:143 iterates `rbegin()..rend()`).
pub fn user_conf_files() -> Vec<PathBuf> {
    if let Some(files) = env("NIX_USER_CONF_FILES") {
        return files
            .split(':')
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .collect();
    }
    let mut files = vec![
        env("NIX_CONFIG_HOME")
            .map_or_else(|| config_home().join("nix"), PathBuf::from)
            .join("nix.conf"),
    ];
    files.extend(config_dirs().into_iter().map(|d| d.join("nix/nix.conf")));
    files
}

/// configuration.cc:BaseSetting<bool>::parse.
pub fn parse_bool(name: &str, s: &str) -> Result<bool> {
    match s {
        "true" | "yes" | "1" => Ok(true),
        "false" | "no" | "0" => Ok(false),
        _ => Err(Error::Config(format!(
            "Boolean setting '{name}' has invalid value '{s}'"
        ))),
    }
}

/// The Nix settings that apply to this process.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Config {
    settings: BTreeMap<String, String>,
}

impl Config {
    pub fn empty() -> Config {
        Config::default()
    }

    /// The whole discovery: `$NIX_CONF_DIR/nix.conf`, then the user files from
    /// lowest to highest precedence, then `$NIX_CONFIG`, then `--option`
    /// overrides last (globals.cc:loadConfFile, common-args.cc:42). A
    /// Determinate Nix setting among them is warned about, as Nix warns about
    /// an unknown one (configuration.cc:warnUnknownSettings).
    pub fn load(overrides: &[(String, String)]) -> Result<Config> {
        let c = Config::read(overrides)?;
        c.settings.keys().for_each(|k| note_determinate(k));
        warn_determinate();
        Ok(c)
    }

    /// `load` without the warnings, for a reader inside ietsp: the log sink,
    /// the fetcher.
    pub fn read(overrides: &[(String, String)]) -> Result<Config> {
        let mut c = Config::empty();
        c.apply_file_if_present(&conf_dir().join("nix.conf"))?;
        for f in user_conf_files().iter().rev() {
            c.apply_file_if_present(f)?;
        }
        if let Some(text) = env("NIX_CONFIG") {
            c.apply(&text, Path::new("NIX_CONFIG"))?;
        }
        for (name, value) in overrides {
            c.set(name, value);
        }
        Ok(c)
    }

    /// An unreadable file is skipped, as loadConfFile's `catch (SystemError &)` does.
    fn apply_file_if_present(&mut self, path: &Path) -> Result<()> {
        match std::fs::read_to_string(path) {
            Ok(text) => self.apply(&text, path),
            Err(_) => Ok(()),
        }
    }

    pub fn apply(&mut self, text: &str, origin: &Path) -> Result<()> {
        for e in parse(text, origin)? {
            // NIX_PATH must override the setting (configuration.cc:186).
            if (e.name == "nix-path" || e.name == "extra-nix-path") && env("NIX_PATH").is_some() {
                continue;
            }
            self.set(&e.name, &e.value);
        }
        Ok(())
    }

    /// configuration.cc:Config::set. `extra-<name>` appends when `<name>` is a
    /// list setting; otherwise the pair is kept under its literal name.
    pub fn set(&mut self, name: &str, value: &str) {
        if let Some(base) = name.strip_prefix("extra-")
            && APPENDABLE.contains(&base)
        {
            let slot = self.settings.entry(base.to_string()).or_default();
            if !slot.is_empty() && !value.is_empty() {
                slot.push(' ');
            }
            slot.push_str(value);
            return;
        }
        self.settings.insert(name.to_string(), value.to_string());
    }

    /// Every setting that was set, known or not.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.settings.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }

    pub fn str(&self, name: &str) -> Option<&str> {
        self.settings.get(name).map(String::as_str)
    }

    pub fn int(&self, name: &str) -> Result<Option<u64>> {
        match self.str(name) {
            None => Ok(None),
            Some(s) => s.trim().parse::<u64>().map(Some).map_err(|_| {
                Error::Config(format!("Integer setting '{name}' has invalid value '{s}'"))
            }),
        }
    }

    pub fn bool(&self, name: &str) -> Result<Option<bool>> {
        self.str(name).map(|s| parse_bool(name, s)).transpose()
    }

    /// A whitespace-separated list setting. Unset is the empty list.
    pub fn list(&self, name: &str) -> Vec<String> {
        self.str(name)
            .into_iter()
            .flat_map(str::split_whitespace)
            .map(str::to_string)
            .collect()
    }

    /// `store`, default `auto` (globals.cc:storeUri).
    pub fn store(&self) -> String {
        self.str("store").unwrap_or("auto").to_string()
    }

    /// `system`, default the build target.
    pub fn system(&self) -> String {
        self.str("system").unwrap_or(DEFAULT_SYSTEM).to_string()
    }

    /// `max-jobs`, with `auto` resolving to the CPU count. The default is the
    /// CPU count too: that is what this client has always sent.
    pub fn max_jobs(&self) -> Result<u64> {
        match self.str("max-jobs") {
            None | Some("auto") => Ok(cpus()),
            Some(_) => Ok(self.int("max-jobs")?.unwrap_or_else(cpus)),
        }
    }

    /// `cores`, where 0 means "all of them" and is the default this client sends.
    pub fn cores(&self) -> Result<u64> {
        Ok(self.int("cores")?.unwrap_or(0))
    }

    pub fn keep_going(&self) -> Result<bool> {
        Ok(self.bool("keep-going")?.unwrap_or(false))
    }

    pub fn keep_failed(&self) -> Result<bool> {
        Ok(self.bool("keep-failed")?.unwrap_or(false))
    }
}

#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
pub const DEFAULT_SYSTEM: &str = "x86_64-linux";
#[cfg(all(target_arch = "aarch64", target_os = "linux"))]
pub const DEFAULT_SYSTEM: &str = "aarch64-linux";
#[cfg(all(target_arch = "x86_64", target_os = "macos"))]
pub const DEFAULT_SYSTEM: &str = "x86_64-darwin";
#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
pub const DEFAULT_SYSTEM: &str = "aarch64-darwin";

fn cpus() -> u64 {
    std::thread::available_parallelism().map_or(1, |n| n.get() as u64)
}

/// One `name = value` line, with the file and line it came from so a reader
/// that rejects unknown names can say where the name is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub value: String,
    pub file: PathBuf,
    pub line: usize,
}

impl Entry {
    /// `file:line`, what an error message shows.
    pub fn at(&self) -> String {
        format!("{}:{}", self.file.display(), self.line)
    }
}

/// configuration.cc:parseConfigFiles. Returns the entries in file order,
/// includes expanded in place.
pub fn parse(text: &str, origin: &Path) -> Result<Vec<Entry>> {
    let mut out = Vec::new();
    parse_into(text, origin, &mut out, 0)?;
    Ok(out)
}

const MAX_INCLUDE_DEPTH: usize = 32;

fn parse_into(text: &str, origin: &Path, out: &mut Vec<Entry>, depth: usize) -> Result<()> {
    if depth > MAX_INCLUDE_DEPTH {
        return Err(Error::Config(format!(
            "too many nested includes in {}",
            origin.display()
        )));
    }
    for (nr, raw) in text.split('\n').enumerate() {
        let line = match raw.find('#') {
            Some(i) => &raw[..i],
            None => raw,
        };
        let tokens: Vec<&str> = line.split_whitespace().collect();
        if tokens.is_empty() {
            continue;
        }
        if tokens.len() < 2 {
            return Err(Error::Config(format!(
                "syntax error in configuration line '{line}' in {}",
                origin.display()
            )));
        }
        let ignore_missing = match tokens[0] {
            "include" => Some(false),
            "!include" => Some(true),
            _ => None,
        };
        if let Some(ignore_missing) = ignore_missing {
            if tokens.len() != 2 {
                return Err(Error::Config(format!(
                    "syntax error in configuration line '{line}' in {}",
                    origin.display()
                )));
            }
            let path = abs_path(tokens[1], origin.parent().unwrap_or(Path::new("")));
            if path.exists() {
                // An existing but unreadable include is ignored, as Nix's
                // `catch (SystemError &)` around the read does.
                if let Ok(included) = std::fs::read_to_string(&path) {
                    parse_into(&included, &path, out, depth + 1)?;
                }
            } else if !ignore_missing {
                return Err(Error::Config(format!(
                    "file '{}' included from '{}' not found",
                    path.display(),
                    origin.display()
                )));
            }
            continue;
        }
        if tokens[1] != "=" {
            return Err(Error::Config(format!(
                "syntax error in configuration line '{line}' in {}",
                origin.display()
            )));
        }
        out.push(Entry {
            name: tokens[0].to_string(),
            value: tokens[2..].join(" "),
            file: origin.to_path_buf(),
            line: nr + 1,
        });
    }
    Ok(())
}

fn abs_path(path: &str, base: &Path) -> PathBuf {
    let p = Path::new(path);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        base.join(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixtures() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/conf")
    }

    fn from_str(text: &str, origin: impl AsRef<Path>) -> Result<Config> {
        let mut c = Config::empty();
        c.apply(text, origin.as_ref())?;
        Ok(c)
    }

    fn load(name: &str) -> Config {
        let path = fixtures().join(name);
        let text = std::fs::read_to_string(&path).unwrap();
        from_str(&text, &path).unwrap()
    }

    #[test]
    fn basic_syntax() {
        let c = load("basic.conf");
        assert_eq!(c.str("cores"), Some("4"));
        assert_eq!(c.cores().unwrap(), 4);
        assert!(c.keep_going().unwrap());
        assert!(!c.keep_failed().unwrap());
        assert_eq!(c.str("with-comment"), Some("value"));
        assert_eq!(c.str("spaced"), Some("a b c"));
        assert_eq!(c.str("empty-value"), Some(""));
    }

    #[test]
    fn unknown_settings_are_kept() {
        let c = load("basic.conf");
        assert_eq!(c.str("not-a-real-setting"), Some("kept"));
        assert!(c.iter().any(|(k, _)| k == "not-a-real-setting"));
    }

    #[test]
    fn determinate_settings_as_flags() {
        let none = || -> std::result::Result<String, String> { panic!("no value") };
        assert_eq!(determinate_flag("--lazy-trees", none), Ok(true));
        assert_eq!(determinate_flag("--no-lazy-trees", none), Ok(true));
        let mut taken = 0;
        let value = || {
            taken += 1;
            Ok("4".to_string())
        };
        assert_eq!(determinate_flag("--eval-cores", value), Ok(true));
        assert_eq!(taken, 1);
        assert_eq!(determinate_flag("--no-eval-cores", none), Ok(false));
        assert_eq!(determinate_flag("--max-jobs", none), Ok(false));
        assert_eq!(determinate_flag("lazy-trees", none), Ok(false));
        note_determinate("extra-build-provenance-tags");
        note_determinate("max-jobs");
        let s = crate::session::Session::current();
        let noted = s.determinate.noted.lock().unwrap();
        assert!(noted.iter().any(|(n, _)| n == "build-provenance-tags"));
        assert!(!noted.iter().any(|(n, _)| n == "max-jobs"));
    }

    #[test]
    fn include_chain() {
        let c = load("outer.conf");
        assert_eq!(c.str("from-outer"), Some("1"));
        assert_eq!(c.str("from-middle"), Some("2"));
        assert_eq!(c.str("from-inner"), Some("3"));
        // The inner file is read where the include sits, so a later line wins.
        assert_eq!(c.str("overridden"), Some("outer-wins"));
    }

    #[test]
    fn missing_include_errors_and_bang_include_does_not() {
        let dir = fixtures();
        let err = from_str("include nope.conf\n", dir.join("x.conf")).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("included from"), "{msg}");
        assert!(msg.contains("not found"), "{msg}");
        let c = from_str("!include nope.conf\ncores = 1\n", dir.join("x.conf")).unwrap();
        assert_eq!(c.cores().unwrap(), 1);
    }

    #[test]
    fn syntax_errors() {
        let e = from_str("cores\n", "t").unwrap_err().to_string();
        assert!(e.contains("syntax error in configuration line"), "{e}");
        let e = from_str("cores 4\n", "t").unwrap_err().to_string();
        assert!(e.contains("syntax error in configuration line"), "{e}");
        let e = from_str("include a b\n", "t").unwrap_err().to_string();
        assert!(e.contains("syntax error in configuration line"), "{e}");
    }

    #[test]
    fn extra_appends_to_list_settings_only() {
        let c = load("extra.conf");
        assert_eq!(
            c.list("substituters"),
            vec![
                "https://cache.nixos.org/".to_string(),
                "https://one.example".to_string(),
                "https://two.example".to_string(),
            ]
        );
        assert_eq!(
            c.list("experimental-features"),
            vec!["nix-command".to_string(), "flakes".to_string()]
        );
        // `cores` is not a list, so `extra-cores` stays a setting of its own.
        assert_eq!(c.cores().unwrap(), 2);
        assert_eq!(c.str("extra-cores"), Some("8"));
    }

    #[test]
    fn typed_getters_and_defaults() {
        let c = Config::empty();
        assert_eq!(c.store(), "auto");
        assert_eq!(c.cores().unwrap(), 0);
        assert!(!c.keep_going().unwrap());
        assert!(!c.keep_failed().unwrap());
        assert_eq!(c.max_jobs().unwrap(), cpus());
        assert!(c.list("substituters").is_empty());
        assert!(c.system().ends_with("-linux") || c.system().ends_with("-darwin"));

        let c = load("typed.conf");
        assert_eq!(c.store(), "daemon");
        assert_eq!(c.system(), "aarch64-linux");
        assert_eq!(c.max_jobs().unwrap(), 3);
        assert_eq!(c.cores().unwrap(), 7);
        assert_eq!(
            c.list("trusted-public-keys"),
            vec!["k1:AAAA=".to_string(), "k2:BBBB=".to_string()]
        );
    }

    #[test]
    fn max_jobs_auto_is_the_cpu_count() {
        let c = from_str("max-jobs = auto\n", "t").unwrap();
        assert_eq!(c.max_jobs().unwrap(), cpus());
    }

    #[test]
    fn bad_typed_values_error() {
        let c = from_str("keep-going = maybe\n", "t").unwrap();
        let e = c.keep_going().unwrap_err().to_string();
        assert!(e.contains("Boolean setting 'keep-going'"), "{e}");
        let c = from_str("cores = many\n", "t").unwrap();
        assert!(c.cores().is_err());
    }

    #[test]
    fn bool_spellings() {
        for (text, want) in [
            ("true", true),
            ("yes", true),
            ("1", true),
            ("false", false),
            ("no", false),
            ("0", false),
        ] {
            let c = from_str(&format!("keep-failed = {text}\n"), "t").unwrap();
            assert_eq!(c.keep_failed().unwrap(), want, "{text}");
        }
    }

    #[test]
    fn overrides_win_over_files() {
        let mut c = load("basic.conf");
        c.set("cores", "16");
        assert_eq!(c.cores().unwrap(), 16);
    }

    #[test]
    fn user_conf_files_are_in_precedence_order() {
        let files = user_conf_files();
        assert!(!files.is_empty());
        assert!(files[0].ends_with("nix/nix.conf"), "{files:?}");
    }
}

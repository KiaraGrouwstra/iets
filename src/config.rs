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
    /// overrides last (globals.cc:loadConfFile, common-args.cc:42).
    pub fn load(overrides: &[(String, String)]) -> Result<Config> {
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

    /// Parses one settings file's text. `origin` is what include paths resolve
    /// against and what errors name.
    pub fn from_str(text: &str, origin: impl AsRef<Path>) -> Result<Config> {
        let mut c = Config::empty();
        c.apply(text, origin.as_ref())?;
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

    /// configuration.cc:BaseSetting<bool>::parse.
    pub fn bool(&self, name: &str) -> Result<Option<bool>> {
        match self.str(name) {
            None => Ok(None),
            Some("true" | "yes" | "1") => Ok(Some(true)),
            Some("false" | "no" | "0") => Ok(Some(false)),
            Some(s) => Err(Error::Config(format!(
                "Boolean setting '{name}' has invalid value '{s}'"
            ))),
        }
    }

    /// A whitespace-separated list setting. Unset is the empty list.
    pub fn list(&self, name: &str) -> Vec<String> {
        self.str(name)
            .into_iter()
            .flat_map(str::split_whitespace)
            .map(str::to_string)
            .collect()
    }

    /// `builders`-style lists: `;`- and newline-separated, `#` comments, and a
    /// leading `@` reads the rest as a file of more entries
    /// (machines.cc:expandBuilderLines). A missing `@file` is skipped.
    pub fn paths(&self, name: &str) -> Result<Vec<String>> {
        let mut out = Vec::new();
        if let Some(s) = self.str(name) {
            expand_builder_lines(s, &mut out, 0)?;
        }
        Ok(out)
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

    pub fn substituters(&self) -> Vec<String> {
        match self.str("substituters") {
            None => vec!["https://cache.nixos.org/".to_string()],
            Some(_) => self.list("substituters"),
        }
    }

    pub fn trusted_public_keys(&self) -> Vec<String> {
        match self.str("trusted-public-keys") {
            None => vec![CACHE_NIXOS_ORG_KEY.to_string()],
            Some(_) => self.list("trusted-public-keys"),
        }
    }

    pub fn builders(&self) -> Result<Vec<String>> {
        self.paths("builders")
    }

    pub fn experimental_features(&self) -> Vec<String> {
        self.list("experimental-features")
    }

    /// `nix-path`, which `$NIX_PATH` replaces wholesale (eval-gc.cc:129).
    pub fn nix_path(&self) -> Vec<String> {
        match env("NIX_PATH") {
            Some(s) => parse_nix_path(&s),
            None => self.list("nix-path"),
        }
    }

    /// `sandbox`: `true`, `false` or `relaxed`. Default `true` on Linux only.
    pub fn sandbox(&self) -> String {
        self.str("sandbox")
            .unwrap_or(if cfg!(target_os = "linux") {
                "true"
            } else {
                "false"
            })
            .to_string()
    }
}

const CACHE_NIXOS_ORG_KEY: &str = "cache.nixos.org-1:6NCHdD59X431o0gWypbMrAURkbJ16ZPMQFGspcDShjY=";

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

fn expand_builder_lines(s: &str, out: &mut Vec<String>, depth: usize) -> Result<()> {
    if depth > MAX_INCLUDE_DEPTH {
        return Err(Error::Config("too many nested machines files".into()));
    }
    for line in s.split('\n') {
        let line = match line.find('#') {
            Some(i) => &line[..i],
            None => line,
        };
        for entry in line.split(';') {
            let entry = entry.trim();
            if entry.is_empty() {
                continue;
            }
            match entry.strip_prefix('@') {
                Some(path) => match std::fs::read_to_string(path.trim()) {
                    Ok(text) => expand_builder_lines(&text, out, depth + 1)?,
                    Err(_) => continue,
                },
                None => out.push(entry.to_string()),
            }
        }
    }
    Ok(())
}

/// eval-settings.cc:EvalSettings::parseNixPath. Colon-separated, except that a
/// value which is a URL keeps the colon that follows its scheme.
pub fn parse_nix_path(s: &str) -> Vec<String> {
    let b = s.as_bytes();
    let mut res = Vec::new();
    let mut p = 0;
    while p < b.len() {
        let start = p;
        let mut start2 = p;
        while p < b.len() && b[p] != b':' {
            if b[p] == b'=' {
                start2 = p + 1;
            }
            p += 1;
        }
        if p == b.len() {
            if p != start {
                res.push(s[start..p].to_string());
            }
            break;
        }
        let prefix = &s[start2..];
        if is_pseudo_url(prefix) || prefix.starts_with("flake:") {
            p += 1;
            while p < b.len() && b[p] != b':' {
                p += 1;
            }
        }
        res.push(s[start..p].to_string());
        if p == b.len() {
            break;
        }
        p += 1;
    }
    res
}

/// eval-settings.cc:EvalSettings::isPseudoUrl.
fn is_pseudo_url(s: &str) -> bool {
    if s.starts_with("channel:") {
        return true;
    }
    match s.find("://") {
        None => false,
        Some(i) => matches!(
            &s[..i],
            "http" | "https" | "file" | "channel" | "git" | "s3" | "ssh"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixtures() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/conf")
    }

    fn load(name: &str) -> Config {
        let path = fixtures().join(name);
        let text = std::fs::read_to_string(&path).unwrap();
        Config::from_str(&text, &path).unwrap()
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
        let err = Config::from_str("include nope.conf\n", dir.join("x.conf")).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("included from"), "{msg}");
        assert!(msg.contains("not found"), "{msg}");
        let c = Config::from_str("!include nope.conf\ncores = 1\n", dir.join("x.conf")).unwrap();
        assert_eq!(c.cores().unwrap(), 1);
    }

    #[test]
    fn syntax_errors() {
        let e = Config::from_str("cores\n", "t").unwrap_err().to_string();
        assert!(e.contains("syntax error in configuration line"), "{e}");
        let e = Config::from_str("cores 4\n", "t").unwrap_err().to_string();
        assert!(e.contains("syntax error in configuration line"), "{e}");
        let e = Config::from_str("include a b\n", "t")
            .unwrap_err()
            .to_string();
        assert!(e.contains("syntax error in configuration line"), "{e}");
    }

    #[test]
    fn extra_appends_to_list_settings_only() {
        let c = load("extra.conf");
        assert_eq!(
            c.substituters(),
            vec![
                "https://cache.nixos.org/".to_string(),
                "https://one.example".to_string(),
                "https://two.example".to_string(),
            ]
        );
        assert_eq!(
            c.experimental_features(),
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
        assert_eq!(c.substituters(), vec!["https://cache.nixos.org/"]);
        assert_eq!(c.trusted_public_keys(), vec![CACHE_NIXOS_ORG_KEY]);
        assert!(c.builders().unwrap().is_empty());
        assert!(c.experimental_features().is_empty());
        assert!(c.system().ends_with("-linux") || c.system().ends_with("-darwin"));
        assert_eq!(
            c.sandbox(),
            if cfg!(target_os = "linux") {
                "true"
            } else {
                "false"
            }
        );

        let c = load("typed.conf");
        assert_eq!(c.store(), "daemon");
        assert_eq!(c.system(), "aarch64-linux");
        assert_eq!(c.max_jobs().unwrap(), 3);
        assert_eq!(c.cores().unwrap(), 7);
        assert_eq!(c.sandbox(), "relaxed");
        assert_eq!(
            c.trusted_public_keys(),
            vec!["k1:AAAA=".to_string(), "k2:BBBB=".to_string()]
        );
    }

    #[test]
    fn max_jobs_auto_is_the_cpu_count() {
        let c = Config::from_str("max-jobs = auto\n", "t").unwrap();
        assert_eq!(c.max_jobs().unwrap(), cpus());
    }

    #[test]
    fn bad_typed_values_error() {
        let c = Config::from_str("keep-going = maybe\n", "t").unwrap();
        let e = c.keep_going().unwrap_err().to_string();
        assert!(e.contains("Boolean setting 'keep-going'"), "{e}");
        let c = Config::from_str("cores = many\n", "t").unwrap();
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
            let c = Config::from_str(&format!("keep-failed = {text}\n"), "t").unwrap();
            assert_eq!(c.keep_failed().unwrap(), want, "{text}");
        }
    }

    #[test]
    fn builders_read_the_machines_file() {
        let c = load("builders.conf");
        assert_eq!(
            c.builders().unwrap(),
            vec![
                "ssh://direct x86_64-linux".to_string(),
                "ssh://other aarch64-linux".to_string(),
            ]
        );
        let text = format!(
            "builders = ssh://direct x86_64-linux ; @{}\n",
            fixtures().join("machines").display()
        );
        let c = Config::from_str(&text, "t").unwrap();
        assert_eq!(
            c.builders().unwrap(),
            vec![
                "ssh://direct x86_64-linux".to_string(),
                "ssh://mach1 x86_64-linux - 4".to_string(),
                "ssh://mach2 aarch64-linux - 2".to_string(),
            ]
        );
        // A machines file that is not there is skipped, not an error.
        let c = Config::from_str("builders = @/nonexistent/machines\n", "t").unwrap();
        assert!(c.builders().unwrap().is_empty());
    }

    #[test]
    fn overrides_win_over_files() {
        let mut c = load("basic.conf");
        c.set("cores", "16");
        assert_eq!(c.cores().unwrap(), 16);
    }

    #[test]
    fn nix_path_urls_keep_their_colon() {
        assert_eq!(
            parse_nix_path("nixpkgs=https://example.com/n.tar.gz:/etc/nixos"),
            vec![
                "nixpkgs=https://example.com/n.tar.gz".to_string(),
                "/etc/nixos".to_string()
            ]
        );
        assert_eq!(
            parse_nix_path("a=/x:b=channel:nixos-24.05"),
            vec!["a=/x".to_string(), "b=channel:nixos-24.05".to_string()]
        );
        let c = Config::from_str("nix-path = nixpkgs=/a /nix/var/nix/profiles/x\n", "t").unwrap();
        if std::env::var_os("NIX_PATH").is_none() {
            assert_eq!(
                c.nix_path(),
                vec![
                    "nixpkgs=/a".to_string(),
                    "/nix/var/nix/profiles/x".to_string()
                ]
            );
        }
    }

    #[test]
    fn user_conf_files_are_in_precedence_order() {
        let files = user_conf_files();
        assert!(!files.is_empty());
        assert!(files[0].ends_with("nix/nix.conf"), "{files:?}");
    }
}

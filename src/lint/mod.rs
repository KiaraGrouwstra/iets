//! Named lints for language corner cases, each `off`, `warn` or `error`.
//! The registry lives here, one line per lint; each family file holds
//! the trigger logic its hooks call. Levels resolve from the built-in
//! default, then `[lint]` in `ietsp.toml`, then `IETSP_LINT`, then
//! `--lint`. See docs/lints.md.

pub mod coerce;
pub mod legacy;
pub mod path;
pub mod purity;
pub mod scope;
pub mod style;

use std::collections::HashSet;
use std::hash::BuildHasherDefault;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::eval::{Error, ErrorKind, EvalResult, Evaluator, Pos};
use crate::syntax::symbol::FxHasher;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Level {
    #[default]
    Off,
    Warn,
    Error,
}

impl Level {
    /// `off`, `warn`, `error`, plus Nix's `ignore` and `fatal`.
    pub fn parse(s: &str) -> Option<Level> {
        match s {
            "off" | "ignore" => Some(Level::Off),
            "warn" => Some(Level::Warn),
            "error" | "fatal" => Some(Level::Error),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Level::Off => "off",
            Level::Warn => "warn",
            Level::Error => "error",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Family {
    Path,
    Scope,
    Coerce,
    Legacy,
    Purity,
    Style,
}

impl Family {
    pub fn name(self) -> &'static str {
        match self {
            Family::Path => "path",
            Family::Scope => "scope",
            Family::Coerce => "coerce",
            Family::Legacy => "legacy",
            Family::Purity => "purity",
            Family::Style => "style",
        }
    }

    pub fn parse(s: &str) -> Option<Family> {
        [
            Family::Path,
            Family::Scope,
            Family::Coerce,
            Family::Legacy,
            Family::Purity,
            Family::Style,
        ]
        .into_iter()
        .find(|f| f.name() == s)
    }
}

/// Where a lint fires. Parser and compiler lints run once per file and
/// travel with the IR cache entry; evaluator lints fire on demand.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Hook {
    Parser,
    Compiler,
    Evaluator,
}

impl Hook {
    pub fn name(self) -> &'static str {
        match self {
            Hook::Parser => "parser",
            Hook::Compiler => "compiler",
            Hook::Evaluator => "evaluator",
        }
    }

    /// The lint runs while a file is compiled, so its level is part of the
    /// IR cache key and its warnings are stored in the entry.
    pub fn is_compile_time(self) -> bool {
        self != Hook::Evaluator
    }
}

pub struct Def {
    pub id: LintId,
    pub name: &'static str,
    pub family: Family,
    pub hook: Hook,
    pub default: Level,
    pub doc: &'static str,
}

macro_rules! registry {
    ($($id:ident: $name:literal, $family:ident, $hook:ident, $default:ident, $doc:literal;)*) => {
        #[repr(u8)]
        #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
        pub enum LintId { $($id),* }

        pub const LINTS: &[Def] = &[$(Def {
            id: LintId::$id,
            name: $name,
            family: Family::$family,
            hook: Hook::$hook,
            default: Level::$default,
            doc: $doc,
        }),*];
    };
}

registry! {
    AbsolutePathLiteral: "absolute-path-literal", Path, Compiler, Off, "a path literal that starts with `/` or `~/` (Nix: lint-absolute-path-literals)";
    ShortPathLiteral: "short-path-literal", Path, Compiler, Off, "a relative path literal without a leading `./` (Nix: lint-short-path-literals)";
    UrlLiteral: "url-literal", Legacy, Parser, Off, "an unquoted URL (Nix: lint-url-literals)";
    With: "with", Scope, Compiler, Off, "any `with` expression";
    WithNonLiteral: "with-non-literal", Scope, Compiler, Off, "`with` whose scope is not an attrset literal or a plain variable";
    WithNested: "with-nested", Scope, Compiler, Off, "`with` inside the body of another `with`";
    ShadowBuiltin: "shadow-builtin", Scope, Compiler, Off, "a let, rec, or formal binding named like a global (`map`, `__add`, `builtins`, ...)";
    ShadowBinding: "shadow-binding", Scope, Compiler, Off, "a binding that hides an outer lexical binding of the same name (`_`-prefixed names excepted)";
    UnusedBinding: "unused-binding", Scope, Compiler, Off, "a `let` binding its scope never references (`_`-prefixed names excepted)";
    UnusedFormal: "unused-formal", Scope, Compiler, Off, "a formal the lambda never references, when no `@` binding is used either";
    RecUnused: "rec-unused", Scope, Compiler, Off, "`rec` on a set where no binding references a sibling";
    PathCopy: "path-copy", Coerce, Evaluator, Off, "a path coerced to a string with a copy to the store (interpolation, `+`, derivation attrs)";
    StorePathRecopy: "store-path-recopy", Coerce, Evaluator, Off, "a path already under the store copied again, which doubles the hash in its name";
    ReadThroughCopy: "read-through-copy", Coerce, Evaluator, Off, "a read (readFile, readDir, import, ...) of a store copy this evaluation made, when the original path would do";
    DrvBoolNull: "drv-bool-null", Coerce, Evaluator, Off, "a Boolean or null coerced into a derivation attribute (`\"1\"`, `\"\"`)";
    DrvList: "drv-list", Coerce, Evaluator, Off, "a list space-joined into a derivation attribute (`outputs` and `args` excepted)";
    EqTypeMismatch: "eq-type-mismatch", Coerce, Evaluator, Off, "`==` or `!=` on values of different types (int and float excepted)";
    EqFunction: "eq-function", Coerce, Evaluator, Off, "`==` or `!=` where a side is a function, which is never equal";
    OrNonAttrs: "or-non-attrs", Coerce, Evaluator, Off, "`x.a or d` where the subject of some element is not an attrset";
    LegacyLet: "legacy-let", Legacy, Parser, Off, "`let { body = ...; }`";
    DeprecatedBuiltin: "deprecated-builtin", Legacy, Compiler, Off, "a reference to isNull, toPath, unsafeDiscardOutputDependency, or a `__` alias such as `__add`";
    Overrides: "overrides", Legacy, Compiler, Off, "`__overrides` in a rec set";
    UnsafeBuiltin: "unsafe-builtin", Legacy, Compiler, Off, "a reference to any `unsafe*` builtin";
    ImpureBuiltin: "impure-builtin", Purity, Evaluator, Off, "a `getEnv` call, or a reference to `currentTime`";
    CurrentSystem: "current-system", Purity, Compiler, Off, "a reference to `currentSystem`";
    UnhashedFetch: "unhashed-fetch", Purity, Evaluator, Off, "fetchurl, fetchTarball, fetchGit or fetchTree called without a hash, narHash or rev";
    Ifd: "ifd", Purity, Evaluator, Off, "import of a string that carries derivation context";
    Fod: "fod", Purity, Evaluator, Off, "a fixed-output derivation; the message names the derivation and its hash mode";
    LargeSourceCopy: "large-source-copy", Purity, Evaluator, Off, "a path copy whose NAR exceeds `max-bytes` (default 8 MiB)";
    GitInSource: "git-in-source", Purity, Evaluator, Warn, "a path copy of a directory that contains a `.git` entry";
    DirCopyUnfiltered: "dir-copy-unfiltered", Purity, Evaluator, Off, "a directory coerced to a string is copied whole, untracked and ignored files included; use `builtins.path` with a filter";
    ImpurePathInDrv: "impure-path-in-drv", Purity, Evaluator, Off, "a derivation attribute holds the text of a path outside the store and outside any flake root (`toString ./x`), so the .drv depends on the checkout location";
    AbsolutePathInDrv: "absolute-path-in-drv", Purity, Evaluator, Off, "a derivation attribute holds the text of a path under a flake root, an absolute path a flake evaluation would have copied";
    IfBoolLiteral: "if-bool-literal", Style, Compiler, Off, "`if c then true else false` or its negation";
    EqBool: "eq-bool", Style, Compiler, Off, "`x == true`, `x == false`, or the same with `!=`";
    ManualInherit: "manual-inherit", Style, Compiler, Off, "`a = a;` or `a = b.a;` where `inherit` would do";
    EmptyLet: "empty-let", Style, Compiler, Warn, "`let in e`";
    EllipsisWithoutBind: "ellipsis-without-bind", Style, Compiler, Off, "`{ a, ... }:` with `...` and no `@name`";
}

pub const N: usize = LINTS.len();

/// Nix's names for the lints that predate the registry.
const ALIASES: &[(&str, LintId)] = &[
    ("lint-absolute-path-literals", LintId::AbsolutePathLiteral),
    ("lint-short-path-literals", LintId::ShortPathLiteral),
    ("lint-url-literals", LintId::UrlLiteral),
];

pub const DEFAULT_MAX_BYTES: u64 = 8 << 20;

impl LintId {
    #[inline]
    pub fn def(self) -> &'static Def {
        &LINTS[self as usize]
    }

    #[inline]
    pub fn name(self) -> &'static str {
        self.def().name
    }

    pub fn from_u8(b: u8) -> Option<LintId> {
        LINTS.get(b as usize).map(|d| d.id)
    }
}

pub fn by_name(name: &str) -> Option<LintId> {
    LINTS
        .iter()
        .find(|d| d.name == name)
        .map(|d| d.id)
        .or_else(|| ALIASES.iter().find(|(a, _)| *a == name).map(|(_, id)| *id))
}

/// The effective level of every lint plus the lint knobs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Config {
    pub levels: [Level; N],
    /// `large-source-copy`: the NAR size a copy may reach.
    pub max_bytes: u64,
}

impl Default for Config {
    fn default() -> Config {
        let mut levels = [Level::Off; N];
        for d in LINTS {
            levels[d.id as usize] = d.default;
        }
        Config {
            levels,
            max_bytes: DEFAULT_MAX_BYTES,
        }
    }
}

impl Config {
    #[inline]
    pub fn level(&self, id: LintId) -> Level {
        self.levels[id as usize]
    }

    pub fn any_on(&self) -> bool {
        self.levels.iter().any(|l| *l != Level::Off)
    }

    /// `all`, `family:NAME`, a lint name, or a Nix alias.
    pub fn set(&mut self, selector: &str, level: Level) -> Result<(), String> {
        if selector == "all" {
            self.levels = [level; N];
            return Ok(());
        }
        if let Some(f) = selector.strip_prefix("family:") {
            let Some(f) = Family::parse(f) else {
                return Err(format!("unknown lint family '{f}'"));
            };
            for d in LINTS.iter().filter(|d| d.family == f) {
                self.levels[d.id as usize] = level;
            }
            return Ok(());
        }
        match by_name(selector) {
            Some(id) => {
                self.levels[id as usize] = level;
                Ok(())
            }
            None => Err(format!("unknown lint '{selector}'")),
        }
    }

    /// `NAME=LEVEL`, as `--lint` and `IETSP_LINT` take it.
    pub fn set_pair(&mut self, pair: &str) -> Result<(), String> {
        let Some((name, level)) = pair.split_once('=') else {
            return Err(format!("lint setting '{pair}' is not NAME=LEVEL"));
        };
        let Some(level) = Level::parse(level.trim()) else {
            return Err(format!(
                "invalid lint level '{level}' for '{name}': use off, warn or error"
            ));
        };
        self.set(name.trim(), level)
    }

    /// `IETSP_LINT="name=level,name=level"`.
    pub fn apply_env(&mut self, s: &str) -> Result<(), String> {
        for pair in s.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            self.set_pair(pair)?;
        }
        Ok(())
    }

    /// The `[lint]` table of an `ietsp.toml`: `name = "level"`, or
    /// `name = { level = "warn", max-bytes = 1000 }` for a lint with knobs.
    pub fn apply_toml(&mut self, text: &str, origin: &Path) -> Result<(), String> {
        let at = |m: String| format!("{}: {m}", origin.display());
        let table: toml::Table = text.parse().map_err(|e| at(format!("{e}")))?;
        let Some(lint) = table.get("lint") else {
            return Ok(());
        };
        let Some(lint) = lint.as_table() else {
            return Err(at("'lint' is not a table".into()));
        };
        for (name, value) in lint {
            match value {
                toml::Value::String(level) => {
                    let Some(level) = Level::parse(level) else {
                        return Err(at(format!(
                            "invalid lint level '{level}' for '{name}': use off, warn or error"
                        )));
                    };
                    self.set(name, level).map_err(at)?;
                }
                toml::Value::Table(t) => {
                    if by_name(name).is_none() {
                        return Err(at(format!("unknown lint '{name}'")));
                    }
                    for (k, v) in t {
                        match (name.as_str(), k.as_str(), v) {
                            (_, "level", toml::Value::String(level)) => {
                                let Some(level) = Level::parse(level) else {
                                    return Err(at(format!(
                                        "invalid lint level '{level}' for '{name}': use off, warn or error"
                                    )));
                                };
                                self.set(name, level).map_err(at)?;
                            }
                            ("large-source-copy", "max-bytes", toml::Value::Integer(n)) => {
                                self.max_bytes = (*n).max(0) as u64;
                            }
                            _ => {
                                return Err(at(format!("unknown key '{k}' for lint '{name}'")));
                            }
                        }
                    }
                }
                _ => {
                    return Err(at(format!(
                        "lint '{name}' must be a level string or a table"
                    )));
                }
            }
        }
        Ok(())
    }

    /// Default, then `ietsp.toml` found by walking up from `start`, then
    /// `IETSP_LINT`, then `flags` in order.
    pub fn resolve(start: Option<&Path>, flags: &[(String, Level)]) -> Result<Config, String> {
        let mut c = Config::default();
        if let Some(file) = start.and_then(find_config_file) {
            let text =
                std::fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))?;
            c.apply_toml(&text, &file)?;
        }
        if let Ok(env) = std::env::var("IETSP_LINT") {
            c.apply_env(&env).map_err(|m| format!("IETSP_LINT: {m}"))?;
        }
        for (selector, level) in flags {
            c.set(selector, *level)?;
        }
        Ok(c)
    }

    /// One byte per lint plus the knobs: the cache key field.
    pub fn key(&self) -> String {
        let mut s: String = self.levels.iter().map(|l| level_char(*l)).collect();
        s.push(':');
        s.push_str(&self.max_bytes.to_string());
        s
    }

    /// The compile-time lints only: the IR cache key field. Empty when they
    /// are all off, so entries written before the registry existed stay valid.
    pub fn compile_key(&self) -> String {
        let on = LINTS
            .iter()
            .any(|d| d.hook.is_compile_time() && self.level(d.id) != Level::Off);
        if !on {
            return String::new();
        }
        LINTS
            .iter()
            .map(|d| {
                if d.hook.is_compile_time() {
                    level_char(self.level(d.id))
                } else {
                    '-'
                }
            })
            .collect()
    }
}

fn level_char(l: Level) -> char {
    match l {
        Level::Off => 'o',
        Level::Warn => 'w',
        Level::Error => 'e',
    }
}

/// The nearest `ietsp.toml` in `start` or an ancestor.
pub fn find_config_file(start: &Path) -> Option<PathBuf> {
    let mut dir = start.to_path_buf();
    loop {
        let f = dir.join("ietsp.toml");
        if f.is_file() {
            return Some(f);
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// `ietsp lint list`: every lint with its effective level.
pub fn list(c: &Config) -> String {
    let mut out = String::new();
    for d in LINTS {
        let level = c.level(d.id);
        let dflt = if level == d.default {
            String::new()
        } else {
            format!(" (default {})", d.default.name())
        };
        out.push_str(&format!(
            "{:<24} {:<5}{:<16} {:<8} {:<9} {}\n",
            d.name,
            level.name(),
            dflt,
            d.family.name(),
            d.hook.name(),
            d.doc
        ));
    }
    out
}

// ---- emission -----------------------------------------------------------

/// Warnings wait in `pending` until it holds this much, then go to stderr
/// in one write: 13k lines on nixos-desktop cost 7 % of the wall at 4
/// workers as one syscall each.
const FLUSH_AT: usize = 16 << 10;

type SiteSet = HashSet<(LintId, Pos), BuildHasherDefault<FxHasher>>;

struct Emitted {
    seen: SiteSet,
    lines: Vec<String>,
    /// Store paths this evaluation copied from a local path, for
    /// `read-through-copy`; filled only while that lint is on.
    copies: HashSet<Vec<u8>>,
    pending: Vec<u8>,
}

impl Emitted {
    fn flush(&mut self) {
        use std::io::Write;
        if !self.pending.is_empty() {
            let _ = std::io::stderr().write_all(&self.pending);
            self.pending.clear();
        }
    }
}

impl Drop for Emitted {
    fn drop(&mut self) {
        self.flush();
    }
}

/// The lint configuration and the warnings a run emitted. Clones share
/// the emitted set, so every fiber deduplicates against the same sites.
#[derive(Clone)]
pub struct Lints {
    pub config: Config,
    shared: Arc<Mutex<Emitted>>,
    /// Sites this fiber saw reported: a hit skips the shared lock, and a
    /// firing site is evaluated many times more than it is reported.
    local_seen: SiteSet,
}

impl Default for Lints {
    fn default() -> Lints {
        Lints::new(Config::default())
    }
}

impl Lints {
    pub fn new(config: Config) -> Lints {
        Lints {
            config,
            shared: Arc::new(Mutex::new(Emitted {
                seen: SiteSet::default(),
                lines: Vec::new(),
                copies: HashSet::new(),
                pending: Vec::new(),
            })),
            local_seen: SiteSet::default(),
        }
    }

    /// A store path produced by copying a local path.
    pub fn note_copy(&self, store_path: &[u8]) {
        if self.on(LintId::ReadThroughCopy) {
            self.shared
                .lock()
                .unwrap()
                .copies
                .insert(store_path.to_vec());
        }
    }

    pub fn is_copy(&self, store_path: &[u8]) -> bool {
        self.shared.lock().unwrap().copies.contains(store_path)
    }

    #[inline]
    pub fn level(&self, id: LintId) -> Level {
        self.config.level(id)
    }

    #[inline]
    pub fn on(&self, id: LintId) -> bool {
        self.config.level(id) != Level::Off
    }

    /// Every warning line emitted so far, in order.
    pub fn lines(&self) -> Vec<String> {
        self.shared.lock().unwrap().lines.clone()
    }

    /// Writes the buffered warnings to stderr. `parallel::run` flushes at
    /// the end of every command, `report` before an error, `trace` and
    /// `warn` before their own line, and the last clone on drop.
    pub fn flush(&self) {
        self.shared.lock().unwrap().flush();
    }

    fn seen(&mut self, id: LintId, pos: Pos) -> bool {
        if self.local_seen.contains(&(id, pos)) {
            return true;
        }
        if self.shared.lock().unwrap().seen.contains(&(id, pos)) {
            self.local_seen.insert((id, pos));
            return true;
        }
        false
    }

    /// Records the site and queues its line; false when another fiber
    /// recorded it first.
    fn record(&mut self, id: LintId, pos: Pos, line: String) -> bool {
        self.local_seen.insert((id, pos));
        let mut g = self.shared.lock().unwrap();
        if !g.seen.insert((id, pos)) {
            return false;
        }
        g.pending.extend_from_slice(line.as_bytes());
        g.pending.push(b'\n');
        g.lines.push(line);
        if g.pending.len() >= FLUSH_AT {
            g.flush();
        }
        true
    }
}

/// A compile-time diagnostic, carried from the compiler (or an IR cache
/// entry) to the evaluator that reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diag {
    pub id: LintId,
    pub pos: Pos,
    pub msg: String,
}

pub fn error(id: LintId, pos: Pos, msg: &str) -> Box<Error> {
    Error::boxed(ErrorKind::Lint, format!("[{}] {msg}", id.name()), pos)
}

impl Evaluator {
    #[inline]
    pub fn lint_on(&self, id: LintId) -> bool {
        self.lints.on(id)
    }

    /// Reports a lint site: nothing at `off`, one warning per (lint, pos)
    /// at `warn`, an error `tryEval` does not catch at `error`.
    #[inline]
    pub fn lint(&mut self, id: LintId, pos: Pos, msg: impl FnOnce() -> String) -> EvalResult<()> {
        match self.lints.level(id) {
            Level::Off => Ok(()),
            Level::Warn => {
                self.lint_warn(id, pos, msg);
                Ok(())
            }
            Level::Error => Err(error(id, pos, &msg())),
        }
    }

    /// Any fiber reports: the site set keeps a warning to one line, so a
    /// helper reporting a site the demand path reaches again costs
    /// nothing, and bailing like `trace` does (docs/parallel.md, Effects)
    /// cost 15 % of the wall on nixos-desktop at 4 workers.
    #[inline(never)]
    fn lint_warn(&mut self, id: LintId, pos: Pos, msg: impl FnOnce() -> String) {
        if self.lints.seen(id, pos) {
            return;
        }
        self.lint_print(id, pos, msg());
    }

    /// `file:line:col` of a lint site, the local path for a file inside a
    /// store copy this evaluation made. `provenance::describe_pos` freezes
    /// its map on first use and an error report needs the whole run's map,
    /// so this resolves the copy chain through the source table directly.
    fn lint_pos(&self, pos: Pos) -> String {
        let Some((f, off)) = self.file_of(pos) else {
            return self.describe_pos(pos);
        };
        let (l, c) = f.line_col(off);
        let path = if crate::provenance::enabled() {
            self.lint_local_path(&f.path)
        } else {
            f.path.clone()
        };
        format!("{}:{l}:{c}", String::from_utf8_lossy(&path))
    }

    fn lint_local_path(&self, path: &[u8]) -> Vec<u8> {
        let mut p = path.to_vec();
        for _ in 0..8 {
            let Some(root) = self.store_root_len(&p) else {
                break;
            };
            let Some(sp) = std::str::from_utf8(&p[..root])
                .ok()
                .and_then(|s| crate::store::path::StorePath::parse(s).ok())
            else {
                break;
            };
            let Some(crate::store::source::SourceOrigin::Tree { local, .. }) =
                self.sources.get(&sp)
            else {
                break;
            };
            let mut next = local.as_os_str().as_encoded_bytes().to_vec();
            next.extend_from_slice(&p[root..]);
            p = next;
        }
        for (store, local) in &crate::settings::get().source_map {
            if let Some(rest) = p.strip_prefix(store.as_bytes())
                && (rest.is_empty() || rest[0] == b'/')
            {
                let mut next = local.as_bytes().to_vec();
                next.extend_from_slice(rest);
                return next;
            }
        }
        p
    }

    /// The length of the `/nix/store/<base>` prefix of `p`, if any.
    fn store_root_len(&self, p: &[u8]) -> Option<usize> {
        let rest = p
            .strip_prefix(self.store_dir.as_slice())?
            .strip_prefix(b"/")?;
        let end = rest.iter().position(|&b| b == b'/').unwrap_or(rest.len());
        (end > 0).then_some(self.store_dir.len() + 1 + end)
    }

    fn lint_print(&mut self, id: LintId, pos: Pos, msg: String) {
        let line = if pos.is_none() {
            format!("warning: [{}] {msg}", id.name())
        } else {
            format!(
                "warning: [{}] {msg}\n  at {}",
                id.name(),
                self.lint_pos(pos)
            )
        };
        self.lints.record(id, pos, line);
    }

    /// Compile-time diagnostics, at the level the compiler saw. A file
    /// compiles the same in every fiber, so a helper prints them itself
    /// instead of bailing; the site set keeps them to one line each.
    pub fn report_diags(&mut self, diags: &[Diag]) {
        for d in diags {
            if !self.lints.seen(d.id, d.pos) {
                self.lint_print(d.id, d.pos, d.msg.clone());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_unique_kebab_case() {
        let mut seen = HashSet::new();
        for d in LINTS {
            assert!(seen.insert(d.name), "duplicate lint {}", d.name);
            assert!(
                d.name
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b == b'-' || b.is_ascii_digit()),
                "{} is not kebab-case",
                d.name
            );
            assert_eq!(by_name(d.name), Some(d.id));
            assert_eq!(LintId::from_u8(d.id as u8), Some(d.id));
        }
    }

    #[test]
    fn selectors_and_order() {
        let mut c = Config::default();
        c.set("all", Level::Warn).unwrap();
        assert!(c.levels.iter().all(|l| *l == Level::Warn));
        c.set("family:scope", Level::Error).unwrap();
        assert_eq!(c.level(LintId::With), Level::Error);
        assert_eq!(c.level(LintId::PathCopy), Level::Warn);
        c.set_pair("with=off").unwrap();
        assert_eq!(c.level(LintId::With), Level::Off);
        c.set_pair("lint-url-literals=fatal").unwrap();
        assert_eq!(c.level(LintId::UrlLiteral), Level::Error);
        assert!(c.set("nope", Level::Warn).is_err());
        assert!(c.set("family:nope", Level::Warn).is_err());
        assert!(c.set_pair("with=loud").is_err());
        c.apply_env("with=warn, ifd=error").unwrap();
        assert_eq!(c.level(LintId::With), Level::Warn);
        assert_eq!(c.level(LintId::Ifd), Level::Error);
    }

    #[test]
    fn toml_table() {
        let mut c = Config::default();
        c.apply_toml(
            "[other]\nx = 1\n[lint]\nwith = \"warn\"\nlarge-source-copy = { level = \"error\", max-bytes = 64 }\n",
            Path::new("t"),
        )
        .unwrap();
        assert_eq!(c.level(LintId::With), Level::Warn);
        assert_eq!(c.level(LintId::LargeSourceCopy), Level::Error);
        assert_eq!(c.max_bytes, 64);
        assert!(
            c.apply_toml("[lint]\nnope = \"warn\"\n", Path::new("t"))
                .is_err()
        );
        assert!(c.apply_toml("[lint]\nwith = 3\n", Path::new("t")).is_err());
        assert!(
            c.apply_toml("[lint]\nwith = { max-bytes = 3 }\n", Path::new("t"))
                .is_err()
        );
    }

    #[test]
    fn keys() {
        let mut c = Config::default();
        c.set("all", Level::Off).unwrap();
        assert_eq!(c.compile_key(), "");
        c.set("path-copy", Level::Warn).unwrap();
        assert_eq!(c.compile_key(), "");
        let off = c;
        c.set("with", Level::Warn).unwrap();
        assert!(c.compile_key().contains('w'));
        assert_ne!(c.key(), off.key());
        assert_eq!(off.key(), off.key());
    }
}

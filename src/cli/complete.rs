//! `iets __complete WORD...` and `iets completion SHELL`: tab completion
//! from the tables the help is made of, so a new flag completes the day it is
//! documented.
//!
//! The protocol is cobra's: one `CANDIDATE<TAB>DESCRIPTION` per line, then
//! `:N`, the directive. The shell scripts are thin because the filtering,
//! the descriptions and the choice of what comes next all happen here.

mod attrs;

use crate::api::Input;
use crate::api::installable::Prefixes;
use crate::cli::help::{self, Flag};
use crate::cli::{classic, docs};

/// The shell adds files to the candidates.
pub const DEFAULT: u8 = 0;
/// The shell does not put a space after the candidate: it is not the end.
pub const NO_SPACE: u8 = 2;
/// The shell adds nothing, not even files.
pub const NO_FILES: u8 = 4;

const BASH: &str = include_str!("complete/iets.bash");
const ZSH: &str = include_str!("complete/iets.zsh");
const FISH: &str = include_str!("complete/iets.fish");
const NUSHELL: &str = include_str!("complete/iets.nu");
const ELVISH: &str = include_str!("complete/iets.elv");
const POWERSHELL: &str = include_str!("complete/iets.ps1");

const SHELLS: &[&str] = &["bash", "zsh", "fish", "nushell", "elvish", "powershell"];

/// `iets completion SHELL`.
pub fn cmd_completion(args: &[String]) -> i32 {
    let script = match args {
        [s] if s == "bash" => BASH,
        [s] if s == "zsh" => ZSH,
        [s] if s == "fish" => FISH,
        [s] if s == "nushell" => NUSHELL,
        [s] if s == "elvish" => ELVISH,
        [s] if s == "powershell" => POWERSHELL,
        [] => help::unknown_subcommand(None, SHELLS),
        [s, ..] => help::unknown_subcommand(Some(s), SHELLS),
    };
    help::out(script);
    0
}

/// `iets __complete WORD...`: `words` are the arguments after `iets`, the
/// last one the word under the cursor (possibly empty).
pub fn cmd_complete(words: &[String]) -> i32 {
    let (cands, directive) = complete(words);
    let mut out = String::new();
    for (c, about) in cands {
        out.push_str(&c);
        if !about.is_empty() {
            out.push('\t');
            out.push_str(&about);
        }
        out.push('\n');
    }
    out.push_str(&format!(":{directive}\n"));
    help::out(&out);
    0
}

type Candidates = Vec<(String, String)>;

/// The names that start with `cur`, once each.
fn pairs<'a>(cur: &str, it: impl IntoIterator<Item = (&'a str, &'a str)>) -> Candidates {
    let mut out: Candidates = Vec::new();
    for (n, a) in it {
        if n.starts_with(cur) && !out.iter().any(|(o, _)| o == n) {
            out.push((n.to_string(), a.to_string()));
        }
    }
    out
}

/// What a placeholder says about the value: a path, or a fixed set of words
/// when it is spelled `A|B`.
enum Kind {
    Path,
    Words(Vec<&'static str>),
    Other,
}

fn kind(placeholder: &'static str) -> Kind {
    if placeholder.contains('|') {
        return Kind::Words(placeholder.split('|').collect());
    }
    match placeholder {
        "FILE" | "DIR" | "PATH" | "ENTRY" | "P=PATH" | "SP=LOCAL" => Kind::Path,
        p if p.ends_with("FILE") || p.ends_with("DIR") => Kind::Path,
        _ => Kind::Other,
    }
}

fn first_word(cur: &str) -> Candidates {
    if cur.starts_with('-') {
        return pairs(cur, [("--help", ""), ("--version", "")]);
    }
    let own = [
        ("help", "print a command's or a topic's page"),
        ("version", "print the version"),
    ];
    let classic = classic::NAMES
        .iter()
        .map(|n| (*n, "act like the Nix command"));
    pairs(
        cur,
        help::command_summaries()
            .into_iter()
            .chain(own)
            .chain(classic),
    )
}

/// The candidates for the last of `words` and the directive.
pub fn complete(words: &[String]) -> (Candidates, u8) {
    let Some((cur, before)) = words.split_last() else {
        return (vec![], NO_FILES);
    };
    let cur = cur.as_str();
    let Some(cmd) = before.first().map(String::as_str) else {
        return (first_word(cur), NO_FILES);
    };
    let rest = &before[1..];

    if cmd == "help" {
        if !rest.is_empty() {
            return (vec![], NO_FILES);
        }
        let topics = docs::names().map(|n| (n, "topic"));
        let cmds = help::command_summaries();
        return (pairs(cur, cmds.into_iter().chain(topics)), NO_FILES);
    }
    if classic::NAMES.contains(&cmd) {
        // the real command's own flags are not ours to know
        return (vec![], DEFAULT);
    }
    let cmd = if cmd == "cache-dump" { "cache" } else { cmd };
    if !help::COMMANDS.split(' ').any(|c| c == cmd) {
        return (vec![], NO_FILES);
    }

    match (cmd, rest.first()) {
        ("completion", None) => return (pairs(cur, SHELLS.iter().map(|s| (*s, ""))), NO_FILES),
        ("completion", _) => return (vec![], NO_FILES),
        ("cache", None) => {
            let subs = [
                ("gc", "remove the cache entries nobody read lately"),
                ("dump", "print the read set of an entry"),
                ("paths", "print what to watch for an entry"),
            ];
            return (pairs(cur, subs), NO_FILES);
        }
        ("lint", None) => return (pairs(cur, [("list", "print the lint registry")]), NO_FILES),
        _ => {}
    }

    let table = help::flag_table(cmd);
    if let Some((f, n)) = pending(&table, before) {
        if matches!(f.name, "-A" | "--attr") {
            return match root_input(&table, before) {
                Some(input) => (pairs_of(attrs::attr_path(input, cur)), NO_SPACE | NO_FILES),
                None => (vec![], NO_FILES),
            };
        }
        return match kind(f.values[n]) {
            Kind::Path => (vec![], DEFAULT),
            Kind::Words(ws) => (pairs(cur, ws.into_iter().map(|w| (w, ""))), NO_FILES),
            Kind::Other => (vec![], NO_FILES),
        };
    }
    if cur.contains('#')
        && !cur.starts_with('-')
        && let Some(p) = fragment_prefixes(cmd)
    {
        return (pairs_of(attrs::flake_fragment(p, cur)), NO_SPACE | NO_FILES);
    }
    if cur.starts_with('-') {
        let flags = table.iter().map(|f| (f.name, f.about));
        let help = [("--help", "print the flags of the command")];
        return (pairs(cur, flags.chain(help)), NO_FILES);
    }
    // a file, a flake reference or an expression: the shell knows files
    (vec![], DEFAULT)
}

fn pairs_of(names: Vec<String>) -> Candidates {
    names.into_iter().map(|n| (n, String::new())).collect()
}

/// What `-A` selects from: `-E EXPR`, `-f FILE` or the first positional,
/// whichever the words before the cursor name. A flake reference has its own
/// fragment, so it is not one.
fn root_input(table: &[Flag], before: &[String]) -> Option<Input> {
    let mut positional = None;
    let mut i = 1;
    while i < before.len() {
        let w = before[i].as_str();
        let flag = table.iter().find(|f| f.name == w);
        match (w, before.get(i + 1)) {
            ("-E" | "--expr", Some(e)) => return Some(Input::Expr(e.clone())),
            ("-f" | "--file", Some(f)) => return Some(Input::File(f.clone())),
            _ => {}
        }
        i += 1 + flag.map_or(0, |f| f.values.len());
        if flag.is_none() && !w.starts_with('-') && positional.is_none() {
            positional = Some(w);
        }
    }
    match positional {
        Some(p) if !p.contains('#') => Some(Input::File(p.to_string())),
        _ => None,
    }
}

/// The prefixes the fragment of a command's flake reference is tried under;
/// `None` for a command whose installable is not a flake output.
fn fragment_prefixes(cmd: &str) -> Option<Prefixes> {
    match cmd {
        "build" | "eval" => Some(Prefixes::Package),
        "print-dev-env" => Some(Prefixes::DevShell),
        "search" => Some(Prefixes::PackageSet),
        _ => None,
    }
}

/// The flag that still wants values among the `before` the cursor, and the
/// index of the value under the cursor.
fn pending<'a>(table: &'a [Flag], before: &[String]) -> Option<(&'a Flag, usize)> {
    let max = table.iter().map(|f| f.values.len()).max().unwrap_or(0);
    for back in 1..=max.min(before.len()) {
        let word = &before[before.len() - back];
        if let Some(f) = table.iter().find(|f| f.name == word) {
            return (f.values.len() >= back).then_some((f, back - 1));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(words: &[&str]) -> (Vec<String>, u8) {
        let w: Vec<String> = words.iter().map(|s| s.to_string()).collect();
        let (c, d) = complete(&w);
        (c.into_iter().map(|(n, _)| n).collect(), d)
    }

    #[test]
    fn commands_complete_with_their_summary() {
        let (c, d) = run(&["bu"]);
        assert_eq!(c, ["build"]);
        assert_eq!(d, NO_FILES);
        assert_eq!(run(&["nix-b"]).0, ["nix-build"]);
        let (c, _) = run(&[""]);
        for want in ["eval", "completion", "help", "version", "nix-shell"] {
            assert!(c.iter().any(|n| n == want), "{want}");
        }
        let w = vec!["ev".to_string()];
        let about = "evaluate an expression or a file and print the value";
        assert_eq!(complete(&w).0[0].1, about);
    }

    #[test]
    fn flags_complete_after_a_dash() {
        let (c, d) = run(&["eval", "--st"]);
        assert_eq!(c, ["--strict"]);
        assert_eq!(d, NO_FILES);
        let (c, _) = run(&["build", "--no-"]);
        assert!(c.contains(&"--no-build".to_string()));
        assert!(c.contains(&"--no-cache".to_string()));
        assert_eq!(run(&["eval", "--he"]).0, ["--help"]);
    }

    #[test]
    fn a_flag_value_completes_by_its_placeholder() {
        assert_eq!(run(&["eval", "--file", ""]), (vec![], DEFAULT));
        assert_eq!(run(&["eval", "-f", "x"]), (vec![], DEFAULT));
        assert_eq!(run(&["cache", "gc", "--cache-dir", ""]), (vec![], DEFAULT));
        // the second value of --arg is an expression, not a path
        assert_eq!(run(&["eval", "--arg", "n", ""]), (vec![], NO_FILES));
        // a value is not a flag
        assert_eq!(run(&["eval", "--workers", "-"]), (vec![], NO_FILES));
        // once the values are in, the flags are back
        assert_eq!(run(&["eval", "--arg", "n", "1", "--js"]).0, ["--json"]);
    }

    #[test]
    fn subcommands_and_topics() {
        assert_eq!(run(&["cache", ""]).0, ["gc", "dump", "paths"]);
        assert_eq!(run(&["completion", "f"]).0, ["fish"]);
        assert_eq!(run(&["completion", ""]).0, SHELLS);
        assert_eq!(run(&["completion", "fish", ""]), (vec![], NO_FILES));
        assert!(run(&["help", "bui"]).0.contains(&"build".to_string()));
        assert!(run(&["help", "build", ""]).0.is_empty());
    }

    #[test]
    fn a_positional_is_left_to_the_shell() {
        assert_eq!(run(&["eval", ""]), (vec![], DEFAULT));
        assert_eq!(run(&["nix-build", ""]), (vec![], DEFAULT));
        assert_eq!(run(&["bogus", ""]), (vec![], NO_FILES));
    }

    #[test]
    fn every_script_asks_the_binary() {
        for s in [BASH, ZSH, FISH, NUSHELL, ELVISH, POWERSHELL] {
            assert!(s.contains("__complete"));
        }
    }
}

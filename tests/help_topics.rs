//! `ietsp help TOPIC` from outside the repository: the binary carries the
//! reference pages, so no `docs/` directory has to be reachable.

use std::process::{Command, Output};

/// The binary, run from `/`, where there is no `docs/`.
fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ietsp"))
        .args(args)
        .current_dir("/")
        .env("XDG_CONFIG_HOME", "/nonexistent")
        .output()
        .expect("ietsp runs")
}

fn stdout(args: &[&str]) -> String {
    let o = run(args);
    assert!(o.status.success(), "{args:?}: {:?}", o.status);
    String::from_utf8(o.stdout).expect("utf-8")
}

#[test]
fn help_lists_the_topics() {
    let s = stdout(&["help"]);
    assert!(s.contains("usage: ietsp COMMAND"));
    assert!(s.contains("topics:"));
    for t in ["settings", "errors", "lints", "divergences", "caching"] {
        assert!(s.contains(t), "{t} is not listed");
    }
}

#[test]
fn a_topic_prints_its_page() {
    let s = stdout(&["help", "settings"]);
    assert!(s.starts_with("# Settings"), "{}", &s[..40.min(s.len())]);
    assert!(s.contains("IETSP_WORKERS"));

    let s = stdout(&["help", "errors"]);
    assert!(s.contains("| `build-failed` |"));

    let s = stdout(&["help", "divergences"]);
    assert!(s.contains("builtins.fromTOML"));
}

#[test]
fn a_command_wins_over_a_topic_of_the_same_name() {
    let s = stdout(&["help", "plumbing"]);
    assert!(s.contains("usage: ietsp plumbing OP"));
    assert!(s.contains("'ietsp help reference/plumbing'"));

    let s = stdout(&["help", "reference/plumbing"]);
    assert!(s.starts_with("# Plumbing operations"));
}

#[test]
fn a_page_the_binary_does_not_carry_names_its_path() {
    let s = stdout(&["help", "caching"]);
    assert!(s.contains("docs/explanation/caching.md"));
}

#[test]
fn an_unknown_topic_is_a_usage_error_with_a_suggestion() {
    let o = run(&["help", "settigns"]);
    assert_eq!(o.status.code(), Some(2));
    let e = String::from_utf8(o.stderr).expect("utf-8");
    assert!(e.contains("unknown command or topic 'settigns'"), "{e}");
    assert!(e.contains("did you mean 'settings'?"), "{e}");
}

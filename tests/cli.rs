//! The native command line: help and version on stdout with exit 0, a bad
//! command line on stderr with exit 2 and never the whole usage.

use std::process::{Command, Output};

fn ietsp(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ietsp"))
        .args(args)
        .env("IETSP_CONF_DIR", "/nonexistent")
        .output()
        .expect("run ietsp")
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

#[test]
fn help_and_version_go_to_stdout() {
    for args in [
        &["--help"][..],
        &["-h"],
        &["help"],
        &["help", "eval"],
        &["eval", "--help"],
        &["build", "-h"],
        &["fast-build", "--help"],
        &["cache", "gc", "--help"],
        &["lint", "--help"],
        &["version"],
        &["--version"],
    ] {
        let o = ietsp(args);
        assert_eq!(o.status.code(), Some(0), "{args:?}");
        assert!(o.stderr.is_empty(), "{args:?}: {}", text(&o.stderr));
        assert!(!o.stdout.is_empty(), "{args:?}");
    }
    let o = ietsp(&["--help"]);
    assert!(text(&o.stdout).contains("  eval-jobs "));
    let o = ietsp(&["eval", "--help"]);
    assert!(text(&o.stdout).starts_with("usage: ietsp eval "));
    assert!(text(&o.stdout).contains("--strict"));
    let v = text(&ietsp(&["--version"]).stdout);
    assert!(
        v.starts_with(&format!("ietsp {} (", env!("CARGO_PKG_VERSION"))),
        "{v}"
    );
}

#[test]
fn a_bad_command_line_names_the_problem() {
    for (args, want) in [
        (&["bogus"][..], "error: unknown command 'bogus'\n"),
        (
            &["eval", "--stirct", "-E", "1"],
            "error: unknown flag '--stirct'\ndid you mean '--strict'?\nrun 'ietsp eval --help' for the flags\n",
        ),
        (
            &["eval", "--workers", "0", "-E", "1"],
            "--workers wants a number >= 1, got '0'",
        ),
        (&["eval", "-E"], "error: -E needs an expression\n"),
        (
            &["eval", "-E", "1", "x.nix"],
            "-E and FILE cannot be combined",
        ),
        (&["build"], "nothing to evaluate"),
        (&["fast-build", "--what"], "unknown flag '--what'"),
        (&["cache", "dunp"], "did you mean 'dump'?"),
    ] {
        let o = ietsp(args);
        let err = text(&o.stderr);
        assert_eq!(o.status.code(), Some(2), "{args:?}: {err}");
        assert!(o.stdout.is_empty(), "{args:?}");
        assert!(err.contains(want), "{args:?}: {err}");
        assert!(
            !err.contains("usage:") && !err.contains("falling back"),
            "{args:?}: {err}"
        );
    }
}

#[test]
fn io_errors_read_like_strerror() {
    let o = ietsp(&["eval", "/nonexistent/missing.nix"]);
    let err = text(&o.stderr);
    assert!(err.contains("No such file or directory"), "{err}");
    assert!(!err.contains("os error"), "{err}");
}

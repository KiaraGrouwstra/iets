//! The `IETS_EVENTS` stream: one JSON object per store add, and silence
//! when nobody asked for it.

use std::process::{Command, Output};

fn eval(expr: &str, events: Option<&str>) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_iets"));
    c.args(["eval", "--no-cache", "--raw", "-E", expr])
        .env("XDG_CONFIG_HOME", "/nonexistent");
    match events {
        Some(v) => c.env("IETS_EVENTS", v),
        None => c.env_remove("IETS_EVENTS"),
    };
    c.output().expect("run iets")
}

fn stderr(expr: &str, events: Option<&str>) -> String {
    let o = eval(expr, events);
    assert_eq!(
        o.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stderr).into_owned()
}

#[test]
fn a_text_add_names_its_size_and_its_position() {
    let err = stderr(r#""${builtins.toFile "t" "hi"}""#, Some("-"));
    let line = err.lines().next().unwrap_or_default();
    assert!(line.contains(r#""event":"store-add""#), "{err}");
    assert!(line.contains(r#""kind":"text""#), "{err}");
    assert!(line.contains(r#""size":2"#), "{err}");
    assert!(line.contains(r#""from":"text""#), "{err}");
    assert!(line.contains("-t\""), "{err}");
    assert!(line.contains(r#""at":"«string»:1:4""#), "{err}");
}

#[test]
fn a_tree_add_names_the_local_path_it_copied() {
    let src = concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml");
    let err = stderr(&format!(r#""${{{src}}}""#), Some("-"));
    assert!(err.contains(r#""kind":"tree""#), "{err}");
    assert!(err.contains(&format!(r#""from":"{src}""#)), "{err}");
}

#[test]
fn an_unwatched_run_says_nothing() {
    assert_eq!(stderr(r#""${builtins.toFile "t" "hi"}""#, None), "");
}

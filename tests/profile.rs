//! The sampled profile: what a run says when it cannot collect one, and the
//! folded lines when it can.

use std::process::{Command, Output};

fn eval(expr: &str, env: &[(&str, &str)]) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_iets"));
    c.args(["eval", "--no-cache", "--workers", "1", "-E", expr])
        .env("XDG_CONFIG_HOME", "/nonexistent")
        .env_remove("IETS_PROFILE")
        .env_remove("IETS_PROFILE_TO")
        .env_remove("IETS_PROFILE_HZ");
    for (k, v) in env {
        c.env(k, v);
    }
    c.output().expect("run iets")
}

#[test]
fn a_profile_that_is_not_eval_is_a_settings_error() {
    let o = eval("1", &[("IETS_PROFILE", "cpu")]);
    assert_ne!(o.status.code(), Some(0));
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("IETS_PROFILE: 'cpu' is not 'eval'"), "{err}");
}

#[test]
fn an_unprofiled_run_says_nothing_about_a_profile() {
    let o = eval("1", &[]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(!err.contains("profile"), "{err}");
}

/// Without the feature the hooks are compiled out, so asking for a profile
/// must say so and not leave an empty file behind.
#[test]
#[cfg(not(feature = "profile-eval"))]
fn a_binary_without_the_feature_says_it_collects_nothing() {
    let o = eval("1", &[("IETS_PROFILE", "eval")]);
    assert_eq!(o.status.code(), Some(0));
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("no --features profile-eval"), "{err}");
}

/// A run with no call in it takes no sample, and an empty profile is said
/// out loud rather than written as an empty file.
#[test]
#[cfg(feature = "profile-eval")]
fn a_run_shorter_than_a_tick_says_it_sampled_nothing() {
    let o = eval("1 + 1", &[("IETS_PROFILE", "eval")]);
    assert_eq!(o.status.code(), Some(0));
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("profile: no samples"), "{err}");
}

#[test]
#[cfg(feature = "profile-eval")]
fn a_folded_line_is_a_stack_and_a_count() {
    let dir = std::env::temp_dir().join(format!("iets-profile-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let out = dir.join("p.folded");
    // mutual recursion, so a sample sees a stack and not one frame
    let expr = "let f = n: if n == 0 then 0 else g (n - 1); g = n: f n; \
                in builtins.foldl' (a: _: a + f 400) 0 (builtins.genList (x: x) 4000)";
    let o = eval(
        expr,
        &[
            ("IETS_PROFILE", "eval"),
            ("IETS_PROFILE_TO", out.to_str().unwrap()),
            ("IETS_PROFILE_HZ", "1000"),
        ],
    );
    assert_eq!(
        o.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    let folded = std::fs::read_to_string(&out).expect("the profile file");
    let _ = std::fs::remove_dir_all(&dir);
    let mut deepest = 0;
    for line in folded.lines() {
        let (stack, count) = line.rsplit_once(' ').expect("a stack and a count");
        count.parse::<u64>().expect("a count");
        for frame in stack.split(';') {
            assert!(!frame.is_empty(), "{line}");
        }
        deepest = deepest.max(stack.split(';').count());
    }
    assert!(deepest > 2, "every stack is shallow:\n{folded}");
}

/// A builtin is a frame too, so time inside one is not blamed on the lambda
/// that called it.
#[test]
#[cfg(feature = "profile-eval")]
fn a_builtin_names_itself_in_a_frame() {
    let dir = std::env::temp_dir().join(format!("iets-profile-op-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let out = dir.join("p.folded");
    let expr = "builtins.length (builtins.foldl' (a: _: a ++ builtins.genList (x: x) 50) [] \
                (builtins.genList (x: x) 2000))";
    let o = eval(
        expr,
        &[
            ("IETS_PROFILE", "eval"),
            ("IETS_PROFILE_TO", out.to_str().unwrap()),
            ("IETS_PROFILE_HZ", "1000"),
        ],
    );
    assert_eq!(
        o.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    let folded = std::fs::read_to_string(&out).expect("the profile file");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        folded.contains("«__genList»"),
        "no builtin frame:\n{folded}"
    );
}

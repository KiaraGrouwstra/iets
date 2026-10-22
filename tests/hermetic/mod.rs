//! The binary under test, cut off from the machine: it gets only the
//! environment below and a home of its own per test, so a `nix.conf` or a
//! cache from an earlier run cannot change what it prints.

// each test crate uses what it needs
#![allow(dead_code)]

use std::collections::BTreeSet;
use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Mutex;
use std::thread;

/// The IETS_ settings every run gets. tests/e2e gives them to Nix too, so a
/// shell either one starts has the same environment.
pub const ENV: &[(&str, &str)] = &[("IETS_PROGRESS", "false")];

/// What a process needs to start, and nothing that changes what iets does.
const INHERIT: &[&str] = &[
    "PATH",
    "TMPDIR",
    "TMP",
    "TEMP",
    "SystemRoot",
    "SystemDrive",
    "PATHEXT",
    "ComSpec",
];

pub fn iets() -> Command {
    iets_as(env!("CARGO_BIN_EXE_iets"))
}

/// `iets` started as `prog`, a link to it under another name.
pub fn iets_as(prog: impl AsRef<OsStr>) -> Command {
    let mut c = Command::new(prog);
    isolate(&mut c);
    let home = home();
    // its own so a test can fake HOME
    c.env("XDG_CACHE_HOME", home.join(".cache"))
        .env("HOME", home)
        .env("XDG_CONFIG_DIRS", "/nonexistent")
        .env("NIX_CONF_DIR", "/nonexistent")
        // the expected paths are Nix's under /nix/store, on every platform
        .env("NIX_STORE_DIR", "/nix/store");
    c
}

/// Only [`INHERIT`] and [`ENV`]. tests/e2e starts Nix with this too.
pub fn isolate(c: &mut Command) {
    c.env_clear();
    for k in INHERIT {
        if let Some(v) = env::var_os(k) {
            c.env(k, v);
        }
    }
    c.envs(ENV.iter().copied());
}

/// A directory per test, emptied on its first use in this process. The
/// test thread is named after the test, under cargo test and nextest.
fn home() -> PathBuf {
    static FRESH: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());
    let name = match thread::current().name() {
        Some(n) if n != "main" => n.replace("::", "-"),
        _ => format!("pid-{}", std::process::id()),
    };
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("hermetic")
        .join(env!("CARGO_CRATE_NAME"))
        .join(&name);
    if FRESH.lock().unwrap().insert(name) {
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create the test home");
    }
    dir
}

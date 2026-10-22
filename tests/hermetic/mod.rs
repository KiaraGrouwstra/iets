//! The binary under test, cut off from the machine's Nix configuration: a
//! `plugin-files` line in `/etc/nix/nix.conf`, a `$NIX_CONFIG` or a user
//! `nix.conf` would otherwise put warnings and settings into every run. It
//! draws no progress either: a `⏵` line comes or not by how fast a run is.

// each test crate uses what it needs
#![allow(dead_code)]

use std::env;
use std::ffi::OsStr;
use std::process::Command;

/// The IETS_ settings every run gets. tests/e2e gives them to Nix too, so a
/// shell either one starts has the same environment.
pub const ENV: &[(&str, &str)] = &[
    // a loaded machine must not turn a completion into none
    ("IETS_COMPLETE_MS", "0"),
    ("IETS_PROGRESS", "false"),
];

pub fn iets() -> Command {
    iets_as(env!("CARGO_BIN_EXE_iets"))
}

/// `iets` started as `prog`, a link to it under another name.
pub fn iets_as(prog: impl AsRef<OsStr>) -> Command {
    let mut c = Command::new(prog);
    c.env("NIX_CONF_DIR", "/nonexistent")
        // set but empty: no user file is read at all (config::user_conf_files)
        .env("NIX_USER_CONF_FILES", "")
        .env("XDG_CONFIG_HOME", "/nonexistent")
        .env("XDG_CONFIG_DIRS", "/nonexistent")
        // the expected paths are Nix's under /nix/store, on every platform
        .env("NIX_STORE_DIR", "/nix/store")
        .env_remove("NIX_STORE")
        .env_remove("NIX_STATE_DIR")
        .env_remove("NIX_DAEMON_SOCKET_PATH")
        .env_remove("NIX_CONFIG")
        .env_remove("NIX_CONFIG_HOME");
    for (k, _) in env::vars_os() {
        if k.to_string_lossy().starts_with("IETS_") {
            c.env_remove(k);
        }
    }
    c.envs(ENV.iter().copied());
    c
}

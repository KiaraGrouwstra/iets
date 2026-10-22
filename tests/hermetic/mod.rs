//! The binary under test, cut off from the machine's Nix configuration: a
//! `plugin-files` line in `/etc/nix/nix.conf`, a `$NIX_CONFIG` or a user
//! `nix.conf` would otherwise put warnings and settings into every run.

use std::env;
use std::process::Command;

pub fn iets() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_iets"));
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
    c
}

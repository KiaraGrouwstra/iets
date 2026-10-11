//! The binary under test, cut off from the machine's Nix configuration: a
//! `plugin-files` line in `/etc/nix/nix.conf`, a `$NIX_CONFIG` or a user
//! `nix.conf` would otherwise put warnings and settings into every run.

use std::process::Command;

pub fn iets() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_iets"));
    c.env("NIX_CONF_DIR", "/nonexistent")
        // set but empty: no user file is read at all (config::user_conf_files)
        .env("NIX_USER_CONF_FILES", "")
        .env("XDG_CONFIG_HOME", "/nonexistent")
        .env("XDG_CONFIG_DIRS", "/nonexistent")
        .env_remove("NIX_CONFIG")
        .env_remove("NIX_CONFIG_HOME");
    c
}

//! The real Nix binaries. Under the classic symlink farm, `nix` on `PATH` is
//! ietsp itself, so every subprocess and the fallback resolve past it.

use std::path::{Path, PathBuf};

fn executable(p: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.mode() & 0o111 != 0)
}

/// `$IETSP_NIX_BIN_DIR/<name>`, else the first `<name>` on `PATH` that is not
/// this executable.
pub fn real(name: &str) -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("IETSP_NIX_BIN_DIR") {
        let p = PathBuf::from(dir).join(name);
        return executable(&p).then_some(p);
    }
    let me = std::fs::canonicalize("/proc/self/exe").ok();
    for dir in std::env::split_paths(&std::env::var_os("PATH")?) {
        let p = dir.join(name);
        if !executable(&p) {
            continue;
        }
        match std::fs::canonicalize(&p) {
            Ok(c) if Some(&c) == me.as_ref() => continue,
            Ok(_) => return Some(p),
            Err(_) => continue,
        }
    }
    None
}

/// `nix` for a subprocess. A bare `nix` when none is found, so the spawn
/// error names it.
pub fn nix() -> PathBuf {
    real("nix").unwrap_or_else(|| PathBuf::from("nix"))
}

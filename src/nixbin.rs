//! The real Nix binaries. Under the classic symlink farm, `nix` on `PATH` is
//! ietsp itself, so every subprocess and the fallback resolve past it.

use std::path::{Path, PathBuf};

/// Written by the farm derivation next to its `bin/`: the directory that
/// holds the nix the farm shadows.
const REAL_DIR_FILE: &str = "share/ietsp/real-nix-bin";

fn executable(p: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.mode() & 0o111 != 0)
}

/// The bin directory a farm entry points at: every hop of the symlink chain
/// from `p` is a directory that may carry `REAL_DIR_FILE` beside it. The
/// system path is itself a symlink farm over ours, so the file is one or
/// more hops away.
fn farm_real_dir(p: &Path) -> Option<PathBuf> {
    let mut cur = p.to_path_buf();
    for _ in 0..16 {
        let dir = cur.parent()?;
        let marker = dir.join("..").join(REAL_DIR_FILE);
        if let Ok(s) = std::fs::read_to_string(&marker) {
            return Some(PathBuf::from(s.trim()));
        }
        let target = std::fs::read_link(&cur).ok()?;
        cur = if target.is_absolute() {
            target
        } else {
            dir.join(target)
        };
    }
    None
}

/// The real `<name>`: `$IETSP_NIX_BIN_DIR/<name>` when it exists there, else
/// the bin directory the farm on `PATH` was built for, else the first
/// `<name>` on `PATH` that is not this executable.
pub fn real(name: &str) -> Option<PathBuf> {
    if let Some(dir) = &crate::settings::get().nix_bin_dir {
        let p = dir.join(name);
        if executable(&p) {
            return Some(p);
        }
    }
    let me = std::fs::canonicalize("/proc/self/exe").ok();
    let mut on_path = None;
    for dir in std::env::split_paths(&std::env::var_os("PATH")?) {
        let p = dir.join(name);
        if !executable(&p) {
            continue;
        }
        match std::fs::canonicalize(&p) {
            Ok(c) if Some(&c) == me.as_ref() => {
                if let Some(real) = farm_real_dir(&p) {
                    let r = real.join(name);
                    if executable(&r) {
                        return Some(r);
                    }
                }
            }
            Ok(_) if on_path.is_none() => on_path = Some(p),
            _ => {}
        }
    }
    on_path.or_else(|| sibling(name))
}

/// The farm never shadows `nix-store`, so its real location names the nix
/// installation even on a system path built without the marker.
fn sibling(name: &str) -> Option<PathBuf> {
    for dir in std::env::split_paths(&std::env::var_os("PATH")?) {
        let probe = dir.join("nix-store");
        if !executable(&probe) {
            continue;
        }
        let real = std::fs::canonicalize(&probe).ok()?;
        let p = real.parent()?.join(name);
        if executable(&p)
            && std::fs::canonicalize(&p).ok() != std::fs::canonicalize("/proc/self/exe").ok()
        {
            return Some(p);
        }
    }
    None
}

/// `nix` for a subprocess. A bare `nix` when none is found, so the spawn
/// error names it.
pub fn nix() -> PathBuf {
    real("nix").unwrap_or_else(|| PathBuf::from("nix"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn farm_marker_is_found_through_two_hops() {
        let tmp = std::env::temp_dir().join(format!("ietsp-nixbin-{}", std::process::id()));
        let farm = tmp.join("farm");
        let sys = tmp.join("sys");
        std::fs::create_dir_all(farm.join("bin")).unwrap();
        std::fs::create_dir_all(farm.join("share/ietsp")).unwrap();
        std::fs::create_dir_all(sys.join("bin")).unwrap();
        std::fs::write(farm.join(REAL_DIR_FILE), "/real/bin\n").unwrap();
        std::os::unix::fs::symlink("/proc/self/exe", farm.join("bin/nix")).unwrap();
        std::os::unix::fs::symlink(farm.join("bin/nix"), sys.join("bin/nix")).unwrap();
        assert_eq!(
            farm_real_dir(&sys.join("bin/nix")),
            Some(PathBuf::from("/real/bin"))
        );
        assert_eq!(farm_real_dir(&sys.join("bin/other")), None);
        std::fs::remove_dir_all(&tmp).unwrap();
    }
}

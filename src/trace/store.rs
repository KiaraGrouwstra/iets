//! The on-disk cache: one directory per key, files written to a temporary
//! name and renamed into place.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub const READSET: &str = "readset";
pub const RESULT: &str = "result";
pub const DRVS: &str = "drvs";
pub const SOURCES: &str = "sources";

pub struct Store {
    root: PathBuf,
}

impl Store {
    /// `--cache-dir`, else `IETSP_CACHE_DIR`, else `$XDG_CACHE_HOME/ietsp/trace`,
    /// else `~/.cache/ietsp/trace`. `None` when no home is known.
    pub fn locate(override_dir: Option<&str>) -> Option<Store> {
        if let Some(d) = override_dir {
            return Some(Store {
                root: PathBuf::from(d),
            });
        }
        if let Some(d) = std::env::var_os("IETSP_CACHE_DIR") {
            return Some(Store {
                root: PathBuf::from(d),
            });
        }
        let base = match std::env::var_os("XDG_CACHE_HOME") {
            Some(x) if !x.is_empty() => PathBuf::from(x),
            _ => PathBuf::from(std::env::var_os("HOME")?).join(".cache"),
        };
        Some(Store {
            root: base.join("ietsp").join("trace"),
        })
    }

    pub fn entry_dir(&self, key: &str) -> PathBuf {
        self.root.join(key)
    }

    pub fn read(&self, key: &str, file: &str) -> Option<Vec<u8>> {
        fs::read(self.entry_dir(key).join(file)).ok()
    }

    pub fn write(&self, key: &str, file: &str, bytes: &[u8]) -> io::Result<()> {
        let dir = self.entry_dir(key);
        fs::create_dir_all(&dir)?;
        write_atomic(&dir, file, bytes)
    }
}

fn write_atomic(dir: &Path, file: &str, bytes: &[u8]) -> io::Result<()> {
    let tmp = dir.join(format!(".{file}.{}.tmp", std::process::id()));
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, dir.join(file)).inspect_err(|_| {
        let _ = fs::remove_file(&tmp);
    })
}

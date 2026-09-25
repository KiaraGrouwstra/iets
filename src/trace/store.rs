//! The on-disk cache: one directory per key, files written to a temporary
//! name and renamed into place. `crate::cachefs` holds the store-level
//! structure (version, writer lock, sweeps) this shares with the IR cache.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::cachefs;

pub const READSET: &str = "readset";
pub const RESULT: &str = "result";
pub const DRVS: &str = "drvs";
pub const SOURCES: &str = "sources";
/// The lint warnings the run printed, newline-separated; a hit prints them again.
pub const WARNINGS: &str = "warnings";

pub struct Store {
    root: PathBuf,
}

impl Store {
    /// `--cache-dir DIR`, else `IETSP_CACHE_DIR`, else `$XDG_CACHE_HOME/ietsp`,
    /// else `~/.cache/ietsp`; the entries live in its `trace/` subdirectory,
    /// so the directory the user names is never a removal target. `None` when
    /// no home is known.
    pub fn locate(override_dir: Option<&str>) -> Option<Store> {
        let base = if let Some(d) = override_dir {
            PathBuf::from(d)
        } else if let Some(d) = &crate::session::Session::current().settings().cache_dir {
            d.clone()
        } else {
            let base = match std::env::var_os("XDG_CACHE_HOME") {
                Some(x) if !x.is_empty() => PathBuf::from(x),
                _ => PathBuf::from(std::env::var_os("HOME")?).join(".cache"),
            };
            base.join("ietsp")
        };
        Some(Store {
            root: base.join("trace"),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn entry_dir(&self, key: &str) -> PathBuf {
        self.root.join(key)
    }

    pub fn read(&self, key: &str, file: &str) -> Option<Vec<u8>> {
        fs::read(self.entry_dir(key).join(file)).ok()
    }

    /// `false` when a sibling writer held the store lock: the entry stays as
    /// it was and the next run misses.
    pub fn write(&self, key: &str, file: &str, bytes: &[u8]) -> io::Result<bool> {
        let dir = self.entry_dir(key);
        fs::create_dir_all(&dir)?;
        let Some(_lock) = cachefs::write_lock(&self.root) else {
            return Ok(false);
        };
        cachefs::write_atomic(&dir, file, cachefs::tag(), bytes).map(|()| true)
    }

    /// A hit marks the entry as used; renaming a file into the directory
    /// already does that, so a refreshed `readset` needs no touch.
    pub fn touch(&self, key: &str) {
        cachefs::touch(&self.entry_dir(key));
    }

    /// Poison and wipe, never repair: an entry that does not decode goes.
    pub fn discard(&self, key: &str) {
        let _ = fs::remove_dir_all(self.entry_dir(key));
    }
}

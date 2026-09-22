//! The tracing cache for one command run: the key a subcommand's arguments
//! make, the lookup, and the entry it writes.

use std::collections::HashMap;

use crate::eval::Evaluator;
use crate::trace::key::Key;

/// The tracing cache for one run: where the entry lives and its key. See docs/tracing-cache.md.
pub struct Cache {
    pub store: crate::trace::store::Store,
    pub key: String,
    timings: bool,
}

impl Cache {
    /// `None` when no cache store can be located.
    pub fn open(cache_dir: Option<&str>, key: Key, timings: bool) -> Option<Cache> {
        let store = crate::trace::store::Store::locate(cache_dir)?;
        crate::cachefs::prepare(store.root(), timings);
        Some(Cache {
            store,
            key: key.finish(),
            timings,
        })
    }

    /// Validates the stored read set; true on a hit. A hit that refreshed
    /// stat fields rewrites `readset`.
    pub fn lookup(&self) -> bool {
        use crate::trace::store::READSET;
        let t0 = std::time::Instant::now();
        let Some(bytes) = self.store.read(&self.key, READSET) else {
            if self.timings {
                eprintln!("cache: miss (no entry)");
            }
            return false;
        };
        let Some(mut obs) = crate::trace::wire::decode_readset(&bytes) else {
            self.store.discard(&self.key);
            if self.timings {
                eprintln!("cache: miss (unreadable entry, discarded)");
            }
            return false;
        };
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get().min(4));
        let t_load = t0.elapsed();
        match crate::trace::validate::validate(&mut obs, threads) {
            Ok(r) => {
                if r.refreshed > 0 {
                    let _ = self.store.write(
                        &self.key,
                        READSET,
                        &crate::trace::wire::encode_readset(&obs),
                    );
                } else {
                    self.store.touch(&self.key);
                }
                if self.timings {
                    eprintln!(
                        "cache: hit, {} entries validated, {} rehashed, load {:.3}s, validate {:.3}s",
                        r.entries,
                        r.rehashed,
                        t_load.as_secs_f64(),
                        (t0.elapsed() - t_load).as_secs_f64()
                    );
                }
                true
            }
            Err(m) => {
                if self.timings {
                    eprintln!(
                        "cache: miss ({}), {} entries, {:.3}s",
                        m.0,
                        obs.len(),
                        t0.elapsed().as_secs_f64()
                    );
                }
                false
            }
        }
    }

    /// Writes the entry after a successful run; a run that observed
    /// `currentTime` or called a fetcher writes nothing. `tables` adds the
    /// drv and source tables, which only a `build` hit reads back.
    pub fn write(&self, ev: &Evaluator, result: &[u8], tables: bool) {
        use crate::trace::store::{DRVS, READSET, RESULT, SOURCES, WARNINGS};
        use crate::trace::wire;
        if !ev.trace.is_cacheable() {
            if self.timings {
                eprintln!("cache: not written (uncacheable run)");
            }
            return;
        }
        let t0 = std::time::Instant::now();
        let obs = ev.trace.take();
        // `readset` goes last: a write the store lock cut short leaves no
        // entry to hit rather than a half-written one.
        let warnings = ev.lints.lines();
        let r = (|| -> std::io::Result<bool> {
            if warnings.is_empty() {
                let _ = std::fs::remove_file(self.store.entry_dir(&self.key).join(WARNINGS));
            } else {
                let mut bytes = Vec::new();
                for w in &warnings {
                    bytes.extend_from_slice(w.as_bytes());
                    bytes.push(b'\n');
                }
                if !self.store.write(&self.key, WARNINGS, &bytes)? {
                    return Ok(false);
                }
            }
            if tables {
                if !self
                    .store
                    .write(&self.key, DRVS, &wire::encode_drvs(&ev.drv.drvs.lock()))?
                {
                    return Ok(false);
                }
                if !self.store.write(
                    &self.key,
                    SOURCES,
                    &wire::encode_sources(&ev.known.origins()),
                )? {
                    return Ok(false);
                }
            }
            if !self.store.write(&self.key, RESULT, result)? {
                return Ok(false);
            }
            self.store
                .write(&self.key, READSET, &wire::encode_readset(&obs))
        })();
        match r {
            Ok(wrote) => {
                if self.timings {
                    if wrote {
                        eprintln!(
                            "cache: wrote {} entries, {:.3}s",
                            obs.len(),
                            t0.elapsed().as_secs_f64()
                        );
                    } else {
                        eprintln!("cache: not written (a sibling holds the store lock)");
                    }
                }
            }
            Err(e) => eprintln!(
                "warning: cannot write cache entry {}: {e}",
                self.store.entry_dir(&self.key).display()
            ),
        }
        crate::cachefs::maybe_sweep(self.store.root(), crate::cachefs::Shape::Dirs, self.timings);
    }

    /// On a hit: the lint warnings the recorded run printed, to stderr.
    pub fn replay_warnings(&self) {
        use std::io::Write;
        if let Some(bytes) = self.store.read(&self.key, crate::trace::store::WARNINGS) {
            let _ = std::io::stderr().write_all(&bytes);
        }
    }

    /// The roots, drv table and source table of a `build` entry.
    #[allow(clippy::type_complexity)]
    pub fn read_build(
        &self,
    ) -> Option<(
        Vec<(crate::store::path::StorePath, Option<String>)>,
        HashMap<crate::store::path::StorePath, crate::builtins::derivation::DrvInfo>,
        HashMap<crate::store::path::StorePath, crate::store::source::SourceOrigin>,
    )> {
        use crate::trace::store::{DRVS, RESULT, SOURCES};
        use crate::trace::wire;
        let tables = (|| {
            let roots = wire::decode_roots(&self.store.read(&self.key, RESULT)?)?;
            let drvs = wire::decode_drvs(&self.store.read(&self.key, DRVS)?)?;
            let sources = wire::decode_sources(&self.store.read(&self.key, SOURCES)?)?;
            Some((roots, drvs, sources))
        })();
        if tables.is_none() {
            self.store.discard(&self.key);
            if self.timings {
                eprintln!("cache: miss (unreadable tables, discarded)");
            }
        }
        tables
    }
}

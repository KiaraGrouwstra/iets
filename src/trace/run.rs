//! The tracing cache for one command run: the key a subcommand's arguments
//! make, the lookup, and the entry it writes.

use std::collections::HashMap;

use crate::cli::{ArgKind, EvalArgs, Input, abs_path, cwd, flake_ref, full_includes};
use crate::eval::Evaluator;

/// The tracing cache for one run: where the entry lives and its key. See docs/tracing-cache.md.
pub struct Cache {
    pub store: crate::trace::store::Store,
    pub key: String,
    timings: bool,
}

impl Cache {
    /// `None` when the cache is off or the command cannot be keyed.
    pub fn setup(a: &EvalArgs, subcommand: &str, ev: &Evaluator) -> Option<Cache> {
        use crate::trace::key::{Key, has_relative_path};
        if a.no_cache || a.dump_drvs.is_some() {
            return None;
        }
        let store = crate::trace::store::Store::locate(a.cache_dir.as_deref())?;
        let cwd = cwd();
        let mut k = Key::new(subcommand);
        let mut needs_cwd = false;
        match a.input.as_ref()? {
            Input::Expr(e) => {
                k.field("expr", e.as_bytes());
                needs_cwd |= has_relative_path(e);
            }
            Input::File(f) => {
                let p = abs_path(f, &cwd);
                let bytes = std::fs::read(std::path::Path::new(
                    <std::ffi::OsStr as std::os::unix::ffi::OsStrExt>::from_bytes(&p),
                ))
                .ok()?;
                k.field("file", &p);
                k.field("file-hash", &crate::trace::sha256(&bytes));
            }
            Input::Flake(f) => {
                let fr = flake_ref(f, &cwd).ok()?;
                k.field("flake", f.as_bytes());
                k.field("flake-dir", fr.dir.as_os_str().as_encoded_bytes());
                for name in ["flake.nix", "flake.lock"] {
                    let bytes = std::fs::read(fr.dir.join(name)).ok()?;
                    k.field(name, &crate::trace::sha256(&bytes));
                }
                needs_cwd = true;
            }
        }
        for (name, kind) in &a.args {
            match kind {
                ArgKind::Expr(e) => {
                    k.field("arg", name.as_bytes());
                    k.field("val", e.as_bytes());
                    needs_cwd |= has_relative_path(e);
                }
                ArgKind::Str(s) => {
                    k.field("argstr", name.as_bytes());
                    k.field("val", s.as_bytes());
                }
            }
        }
        for (prefix, path) in full_includes(&a.includes) {
            k.field("include-prefix", &prefix);
            k.field("include-path", &path);
        }
        k.field("attr", a.attr.as_deref().unwrap_or("").as_bytes());
        k.flag("strict", a.strict);
        k.flag("json", a.json);
        k.flag("raw", a.raw);
        k.field("lint-absolute", format!("{:?}", a.lint_absolute).as_bytes());
        k.field("lint-short", format!("{:?}", a.lint_short).as_bytes());
        for f in &a.xp_features {
            k.field("xp", f.as_bytes());
        }
        k.field("system", ev.current_system.as_bytes());
        if needs_cwd {
            k.field("cwd", &cwd);
        }
        Some(Cache {
            store,
            key: k.finish(),
            timings: a.timings,
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
            if self.timings {
                eprintln!("cache: miss (unreadable entry)");
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
        use crate::trace::store::{DRVS, READSET, RESULT, SOURCES};
        use crate::trace::wire;
        if !ev.trace.is_cacheable() {
            if self.timings {
                eprintln!("cache: not written (uncacheable run)");
            }
            return;
        }
        let t0 = std::time::Instant::now();
        let obs = ev.trace.take();
        let r = if tables {
            self.store
                .write(&self.key, DRVS, &wire::encode_drvs(&ev.drv.drvs.lock()))
                .and_then(|_| {
                    self.store.write(
                        &self.key,
                        SOURCES,
                        &wire::encode_sources(&ev.source_origins()),
                    )
                })
        } else {
            Ok(())
        };
        let r = r
            .and_then(|_| self.store.write(&self.key, RESULT, result))
            .and_then(|_| {
                self.store
                    .write(&self.key, READSET, &wire::encode_readset(&obs))
            });
        match r {
            Ok(()) => {
                if self.timings {
                    eprintln!(
                        "cache: wrote {} entries, {:.3}s",
                        obs.len(),
                        t0.elapsed().as_secs_f64()
                    );
                }
            }
            Err(e) => eprintln!(
                "warning: cannot write cache entry {}: {e}",
                self.store.entry_dir(&self.key).display()
            ),
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
        let roots = wire::decode_roots(&self.store.read(&self.key, RESULT)?)?;
        let drvs = wire::decode_drvs(&self.store.read(&self.key, DRVS)?)?;
        let sources = wire::decode_sources(&self.store.read(&self.key, SOURCES)?)?;
        Some((roots, drvs, sources))
    }
}

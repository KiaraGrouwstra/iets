//! Debug-only guard on the fiber template.
//!
//! `try_run` clones the evaluator as the template every helper fiber is
//! made from. A field configured after that clone is the main fiber's
//! alone, and the failure is silent: output that is partial, a cache no
//! worker reads, a search path that resolves to nothing. The guard
//! fingerprints the template's configurable fields, then reads the same
//! fields off the main fiber's evaluator once it has returned. A field
//! that moved was set too late.
//!
//! A per-fiber field is fingerprinted by content. A field behind a shared
//! handle is fingerprinted by handle identity, because what such a field
//! holds reaches every fiber whenever it is set; only swapping the handle
//! hides a value. A field whose `Clone` resets it on purpose — the arena,
//! the eval-store connection — is not a setting and is left out.

use std::hash::{Hash, Hasher};
use std::sync::Arc;

use crate::eval::Evaluator;

/// The configurable state of the evaluator `try_run` cloned.
pub struct Template(Vec<(&'static str, u64)>);

fn digest(v: impl Hash) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    v.hash(&mut h);
    h.finish()
}

fn fields(ev: &Evaluator) -> Vec<(&'static str, u64)> {
    // the value of `__nixPath` is a fresh word on every `set_search_path`,
    // so the keys are the part a late insert has to change
    let mut globals = ev.globals.table.len() as u64;
    for sym in ev.globals.table.keys() {
        globals ^= digest(sym.0);
    }
    vec![
        ("lints.config", digest(ev.lints.config.key())),
        (
            "ir_cache",
            ev.ir_cache.as_ref().map_or(0, |c| Arc::as_ptr(c) as u64),
        ),
        ("search_path", digest(&ev.search_path)),
        ("globals.table", globals),
        (
            "globals.builtins",
            ev.globals.builtins.map_or(0, |b| b as *const _ as u64),
        ),
        ("builtins_word", ev.builtins_word.bits()),
        ("root_env", ev.root_env as *const _ as u64),
        ("drv.keep_aterm", ev.drv.keep_aterm as u64),
        ("drv.keep_site", ev.drv.keep_site as u64),
        ("drv.dump_dir", Arc::as_ptr(&ev.drv.dump_dir) as u64),
        ("trace", ev.trace.is_on() as u64),
        ("pure", ev.pure as u64),
        ("store_dir", digest(&ev.store_dir)),
        ("current_system", digest(ev.current_system)),
        ("session", Arc::as_ptr(&ev.session) as u64),
        ("totals", ev.totals_ptr()),
    ]
}

impl Template {
    pub fn of(ev: &Evaluator) -> Template {
        Template(fields(ev))
    }

    /// Panics when `ev` no longer matches the template it was cloned from.
    pub fn check(&self, ev: &Evaluator) {
        for (&(name, was), &(_, now)) in self.0.iter().zip(fields(ev).iter()) {
            assert!(
                was == now,
                "`{name}` was set after parallel::try_run cloned the fiber template, so no \
                 helper fiber has it: configure the evaluator before try_run (api::configure)"
            );
        }
    }
}

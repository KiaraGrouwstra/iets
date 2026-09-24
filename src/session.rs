//! What a run owns. A library must not keep this in process-wide statics:
//! two evaluators in one process each want their own settings, their own
//! log destination and their own mismatch list. `Session` holds all of it.
//!
//! An `Evaluator` keeps a handle to the session it was made in, so the hot
//! paths read a field. Code with no evaluator at hand — the command line,
//! a fetch, a build — uses [`Session::current`], the session this thread
//! entered. A thread the library starts inherits it through
//! [`bind`]. docs/library.md says what a host owns.

use std::cell::RefCell;
use std::sync::Arc;

use crate::log::Log;
use crate::settings::Settings;

#[derive(Default)]
pub struct Session {
    pub log: Log,
    settings: std::sync::OnceLock<Settings>,
    /// The Determinate Nix settings this run saw (`crate::config`).
    pub determinate: crate::config::Determinate,
    /// The input store this run reads flake inputs from (`crate::inputs`).
    pub inputs: crate::inputs::Store,
    /// The `eval-jobs` walk of this run (`crate::jobs`).
    pub jobs: crate::jobs::Run,
    /// nix.conf, read once per run: it cannot change under an evaluation.
    pub nix_conf: std::sync::OnceLock<crate::config::Config>,
    /// Store path to local tree, for the commands that build the map while
    /// the evaluation is live (`crate::provenance`).
    pub source_map: std::sync::OnceLock<crate::provenance::SourceMap>,
    /// The `parse-toml-timestamps` experimental feature.
    pub toml_timestamps: std::sync::atomic::AtomicBool,
}

thread_local! {
    static CURRENT: RefCell<Option<Arc<Session>>> = const { RefCell::new(None) };
}

/// The session of a thread that entered none: everything the library does
/// before a host makes one of its own. It writes nowhere.
fn fallback() -> Arc<Session> {
    static D: std::sync::OnceLock<Arc<Session>> = std::sync::OnceLock::new();
    D.get_or_init(|| Arc::new(Session::default())).clone()
}

impl Session {
    pub fn new() -> Arc<Session> {
        Arc::new(Session::default())
    }

    /// The session this thread entered, else the fallback one.
    pub fn current() -> Arc<Session> {
        CURRENT
            .with(|c| c.borrow().clone())
            .unwrap_or_else(fallback)
    }

    /// Makes `self` this thread's session until the guard drops.
    pub fn enter(self: &Arc<Session>) -> Entered {
        let prev = CURRENT.with(|c| c.borrow_mut().replace(self.clone()));
        Entered(prev)
    }

    /// This session's settings: the environment knobs alone until
    /// [`Session::load_settings`] reads the files (docs/settings.md).
    pub fn settings(&self) -> &Settings {
        self.settings.get_or_init(Settings::default)
    }

    /// Reads the `ietsp.conf` files and applies the `--ietsp-option` pairs.
    /// Call it once, on an entered session, before anything reads a setting.
    pub fn load_settings(&self, options: &[(String, String)]) -> crate::error::Result<()> {
        let s = Settings::load(options)?;
        let _ = self.settings.set(s);
        Ok(())
    }
}

/// Puts back the session [`Session::enter`] replaced.
pub struct Entered(Option<Arc<Session>>);

impl Drop for Entered {
    fn drop(&mut self) {
        CURRENT.with(|c| *c.borrow_mut() = self.0.take());
    }
}

/// `f` on a new thread, with this thread's session entered there.
pub fn bind<R>(f: impl FnOnce() -> R) -> impl FnOnce() -> R {
    let s = Session::current();
    move || {
        let _entered = s.enter();
        f()
    }
}

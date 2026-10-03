//! Where an evaluation's time went, as a sampled flame graph.
//!
//! A tick thread asks for a sample and the call path tests one relaxed load,
//! so no clock is read per call. Each fiber samples its own shadow stack on
//! the worker that owns it, and never reads another fiber's. The output is
//! the folded form `frame;frame <count>` that `flamegraph.pl` and every
//! compatible tool read (docs/reference/profile.md).
//!
//! The shadow stack costs two stores per call, so it lives behind
//! `--features profile-eval`: a binary without the feature has no push, no
//! pop and no branch.

use std::collections::HashMap;
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use crate::eval::error::Pos;
use crate::parallel::interner::Interner;
use crate::parallel::shared::Files;
use crate::syntax::symbol::Sym;

/// Frames below this are left out of a sample, so hashing one stays bounded.
/// A nixpkgs attribute reaches about 200.
pub const MAX_DEPTH: usize = 512;

/// Low word of a frame whose lambda has no name.
const ANON: u32 = u32::MAX;

/// A call site and the lambda it entered, in one word: both halves are
/// already interned, and `Profile::write` turns them into text at the end
/// instead of while the samples are collected.
#[inline]
#[cfg(feature = "profile-eval")]
pub fn frame(pos: Pos, name: Option<Sym>) -> u64 {
    ((pos.0 as u64) << 32) | name.map_or(ANON, |s| s.0) as u64
}

/// One fiber's shadow stack. Pushed and popped by the call path, read by
/// nothing else.
#[cfg(feature = "profile-eval")]
pub struct Stack {
    frames: Vec<u64>,
    last_tick: u64,
}

/// A fiber made from another starts its own stack: `Evaluator` is `Clone`
/// and the clone runs elsewhere, so copying the frames would blame the new
/// fiber for its maker's call chain.
#[cfg(feature = "profile-eval")]
impl Clone for Stack {
    fn clone(&self) -> Stack {
        Stack::default()
    }
}

#[cfg(feature = "profile-eval")]
impl Default for Stack {
    fn default() -> Stack {
        Stack {
            frames: Vec::with_capacity(1024),
            last_tick: 0,
        }
    }
}

#[cfg(feature = "profile-eval")]
impl Stack {
    /// Enters `f`, and takes a sample when the tick thread has asked for one
    /// since this fiber's last.
    #[inline]
    pub fn enter(&mut self, f: u64, p: &Profile) {
        self.frames.push(f);
        let t = p.tick.load(Ordering::Relaxed);
        if t != self.last_tick {
            self.last_tick = t;
            let n = self.frames.len().min(MAX_DEPTH);
            p.sample(&self.frames[..n]);
        }
    }

    #[inline]
    pub fn depth(&self) -> usize {
        self.frames.len()
    }

    /// Returns to `depth`. A call that failed left its frames behind, and
    /// `tryEval` goes on evaluating, so the unwind is by depth and not by a
    /// pop per frame.
    #[inline]
    pub fn unwind(&mut self, depth: usize) {
        self.frames.truncate(depth);
    }
}

/// One run's samples. Lives on the session, so two evaluators in one process
/// each profile their own run.
#[derive(Default)]
pub struct Profile {
    /// Bumped by the tick thread; compared by the call path.
    tick: AtomicU64,
    stop: AtomicBool,
    folded: Mutex<HashMap<Box<[u64]>, u64>>,
    /// Set by the first evaluator that profiles, for `write` to symbolise
    /// with after the evaluation is gone.
    names: OnceLock<(Files, Interner)>,
    /// Samples a second the tick thread asks for.
    hz: AtomicU64,
    /// Where the folded lines go. `-` is stderr.
    to: OnceLock<String>,
}

impl Profile {
    /// Whether this run profiles. A sample is never taken when it does not,
    /// because the tick stays at zero.
    pub fn on(&self) -> bool {
        self.to.get().is_some()
    }

    /// What `write` needs to turn a frame into text.
    pub fn names(&self, files: &Files, interner: &Interner) {
        let _ = self.names.set((files.clone(), interner.clone()));
    }

    #[cfg(feature = "profile-eval")]
    fn sample(&self, frames: &[u64]) {
        let Ok(mut g) = self.folded.lock() else {
            return;
        };
        *g.entry(frames.into()).or_insert(0) += 1;
    }

    /// Writes the folded lines, and stops the tick thread. Called once, when
    /// the run has finished.
    pub fn write(&self) {
        self.stop.store(true, Ordering::Relaxed);
        let Some(to) = self.to.get() else { return };
        let Ok(g) = self.folded.lock() else { return };
        if g.is_empty() {
            crate::errln!(
                "profile: no samples at {} Hz, so the evaluation was shorter than one tick",
                self.hz.load(Ordering::Relaxed)
            );
            return;
        }
        let mut out = Vec::new();
        for (stack, count) in g.iter() {
            let mut first = true;
            for f in stack.iter() {
                if !first {
                    out.push(b';');
                }
                first = false;
                out.extend_from_slice(self.describe(*f).as_bytes());
            }
            let _ = writeln!(out, " {count}");
        }
        if to == "-" {
            let _ = std::io::stderr().write_all(&out);
            return;
        }
        match crate::log::open_sink(std::path::Path::new(to)) {
            Ok(mut w) => {
                let _ = w.write_all(&out);
            }
            Err(e) => crate::errln!(
                "error (ignored): opening profile file \"{to}\": {}",
                crate::error::io_text(&e)
            ),
        }
    }

    /// `file:line:col:name`, and no name for an anonymous lambda. A frame a
    /// flame graph tool reads must hold no `;` and no space; a Nix path can
    /// hold either, so both are replaced.
    fn describe(&self, f: u64) -> String {
        let pos = Pos((f >> 32) as u32);
        let low = f as u32;
        let Some((files, interner)) = self.names.get() else {
            return format!("pos{}", pos.0);
        };
        let at = match files.file_of(pos) {
            Some((file, off)) => {
                let (l, c) = file.line_col(off);
                format!("{}:{l}:{c}", String::from_utf8_lossy(&file.path))
            }
            None => "«none»".to_string(),
        };
        let mut s = match low {
            ANON => at,
            sym => format!("{at}:{}", interner.name_str(Sym(sym))),
        };
        if s.contains(';') || s.contains(' ') {
            s = s.replace(';', ",").replace(' ', "_");
        }
        s
    }
}

/// Starts profiling this session, when it asked for it. The tick thread
/// holds the session: it ends on the first tick after [`Profile::write`].
pub fn open(session: &Arc<crate::session::Session>) {
    let s = session.settings();
    if !s.profile_eval {
        return;
    }
    if !cfg!(feature = "profile-eval") {
        crate::errln!(
            "error (ignored): this iets has no --features profile-eval, so IETS_PROFILE collects nothing"
        );
        return;
    }
    let hz = s.profile_hz.max(1);
    session.profile.hz.store(hz as u64, Ordering::Relaxed);
    let _ = session
        .profile
        .to
        .set(s.profile_to.clone().unwrap_or_else(|| "-".to_string()));
    let own = session.clone();
    let _ = std::thread::Builder::new()
        .name("iets-profile".into())
        .spawn(move || {
            let period = std::time::Duration::from_micros(1_000_000 / hz as u64);
            while !own.profile.stop.load(Ordering::Relaxed) {
                std::thread::sleep(period);
                own.profile.tick.fetch_add(1, Ordering::Relaxed);
            }
        });
}

#[cfg(all(test, feature = "profile-eval"))]
mod tests {
    use super::*;

    fn folded(p: &Profile) -> Vec<(Vec<u64>, u64)> {
        let g = p.folded.lock().unwrap();
        let mut v: Vec<_> = g.iter().map(|(k, c)| (k.to_vec(), *c)).collect();
        v.sort();
        v
    }

    #[test]
    fn a_frame_keeps_its_call_site_and_its_name() {
        let f = frame(Pos(7), Some(Sym(3)));
        assert_eq!(f >> 32, 7);
        assert_eq!(f as u32, 3);
        assert_eq!(frame(Pos(7), None) as u32, ANON);
    }

    #[test]
    fn a_sample_is_taken_once_per_tick() {
        let p = Profile::default();
        let mut s = Stack::default();
        p.tick.store(1, Ordering::Relaxed);
        s.enter(10, &p);
        s.enter(20, &p);
        assert_eq!(folded(&p), vec![(vec![10], 1)]);
        p.tick.store(2, Ordering::Relaxed);
        s.enter(30, &p);
        assert_eq!(folded(&p), vec![(vec![10], 1), (vec![10, 20, 30], 1)]);
    }

    #[test]
    fn an_unwind_returns_to_a_depth() {
        let p = Profile::default();
        let mut s = Stack::default();
        let d = s.depth();
        s.enter(10, &p);
        s.enter(20, &p);
        s.unwind(d);
        assert_eq!(s.depth(), 0);
        p.tick.store(1, Ordering::Relaxed);
        s.enter(30, &p);
        assert_eq!(folded(&p), vec![(vec![30], 1)]);
    }

    #[test]
    fn a_deep_stack_is_capped() {
        let p = Profile::default();
        let mut s = Stack::default();
        for i in 0..(MAX_DEPTH as u64 + 10) {
            s.enter(i, &p);
            p.tick.store(i + 1, Ordering::Relaxed);
        }
        assert!(folded(&p).iter().all(|(k, _)| k.len() <= MAX_DEPTH));
    }
}

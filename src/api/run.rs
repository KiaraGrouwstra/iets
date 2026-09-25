//! One run of one command: the evaluator, the lints, the tracing cache, the
//! trace switch and the parallel entry. A frontend that owns one of those
//! parts — `eval-jobs` its worker processes, `fast-build` its evaluator —
//! starts from [`Run::on`] and keeps only what it needs (docs/explanation/plumbing-and-porcelain.md).

use crate::eval::{ErrorKind, Evaluator};
use crate::parallel::fiber::StackError;
use crate::trace::run::Cache;

use super::EvalArgs;

/// What one run produced: what the caller asked for, an error that was
/// reported and its exit code, or something this frontend does not serve,
/// which classic mode answers by exec'ing the real Nix.
pub enum Outcome<T> {
    Ok(T),
    Failed(i32),
    Unsupported(String),
}

pub struct Run {
    pub ev: Evaluator,
    /// `None` when the cache is off, the arguments cannot be keyed, or the
    /// command asked for none.
    pub cache: Option<Cache>,
    pub workers: usize,
}

impl Run {
    /// The evaluator `a` asks for, configured. `Err` is the exit code of a
    /// setting that does not resolve, a lint selector or `--dump-drvs`.
    pub fn new(a: &EvalArgs) -> Result<Run, i32> {
        let mut ev = a.evaluator();
        if let Err(e) = super::configure(&mut ev, a) {
            return Err(super::report(&ev, &e));
        }
        Ok(Run::on(ev).workers(a.workers))
    }

    /// An evaluator the frontend made itself; its lints stay as they are,
    /// which is how the classic commands keep them off.
    pub fn on(ev: Evaluator) -> Run {
        Run {
            ev,
            cache: None,
            workers: crate::parallel::default_workers(),
        }
    }

    /// Opens the tracing cache of `subcommand`, keyed on `a`.
    pub fn cached(mut self, a: &EvalArgs, subcommand: &str) -> Run {
        self.cache = super::setup_cache(a, subcommand, &self.ev);
        self
    }

    /// [`Run::on`] plus the evaluator state `a` asks for, installed before
    /// the fiber template is cloned (`api::configure`). A frontend with its
    /// own evaluator uses this when it evaluates through `api::evaluate`.
    /// The error is dropped: `evaluate` applies the same settings and
    /// reports it in the run's own error path.
    pub fn configured(mut self, a: &EvalArgs) -> Run {
        let _ = super::configure(&mut self.ev, a);
        self
    }

    pub fn workers(mut self, n: usize) -> Run {
        self.workers = n;
        self
    }

    /// The cache entry `f` accepts, when this run has one and it validated.
    /// `f` returning `None` is a miss, so the run goes on.
    pub fn hit<T>(&self, f: impl FnOnce(&Cache) -> Option<T>) -> Option<T> {
        let c = self.cache.as_ref()?;
        if !c.lookup() {
            return None;
        }
        f(c)
    }

    /// The result bytes of the entry this run validated: what the command
    /// wrote with `Cache::write` last time, when the read set still holds.
    pub fn hit_result(&self) -> Option<Vec<u8>> {
        self.hit(|c| c.part(crate::trace::store::Part::Result))
    }

    /// `f` as the main fiber of `workers` evaluator threads, with the trace
    /// on when there is a cache entry to write. The error is the fiber
    /// stacks: the reservation is over the cap, or the kernel refused it.
    pub fn go<R: Send + 'static>(
        mut self,
        f: impl FnOnce(&mut Evaluator, Option<Cache>) -> R + Send + 'static,
    ) -> Result<R, StackError> {
        if self.cache.is_some() {
            self.ev.trace = crate::trace::Trace::enabled();
        }
        let cache = self.cache;
        crate::parallel::try_run(self.workers, self.ev, move |ev| f(ev, cache))
    }

    /// [`Run::go`] with the error classified: what this frontend does not
    /// serve becomes `Unsupported`, anything else is reported once.
    pub fn eval<T: Send + 'static>(
        self,
        f: impl FnOnce(&mut Evaluator, Option<Cache>) -> crate::error::Result<T> + Send + 'static,
    ) -> Result<Outcome<T>, StackError> {
        use crate::error::Error;
        self.go(move |ev, cache| match f(ev, cache) {
            Ok(t) => Outcome::Ok(t),
            Err(Error::Unsupported(m)) => Outcome::Unsupported(m),
            Err(Error::Eval(e)) if e.kind == ErrorKind::Unsupported => {
                Outcome::Unsupported(e.msg.clone())
            }
            Err(Error::Eval(e)) => Outcome::Failed(super::report(ev, &e)),
            Err(e) => Outcome::Failed(e.report()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{EvalArgs, Input};

    /// `n` files that each cost real parsing and compiling, and the
    /// expression that imports them all from one list. They are slow on
    /// purpose: a file the main fiber is still compiling is a file the
    /// helpers steal, which is what this has to observe.
    fn workload(dir: &std::path::Path, n: usize) -> String {
        std::fs::create_dir_all(dir).expect("workload directory");
        let mut elems = String::new();
        for i in 0..4000 {
            elems += &format!("{i} ");
        }
        let mut list = String::from("[");
        for i in 0..n {
            let f = dir.join(format!("m{i}.nix"));
            let src = format!("let xs = [ {elems}]; in builtins.length xs + {i}\n");
            std::fs::write(&f, src).expect("workload file");
            list += &format!(" (import {}) ", f.display());
        }
        list + "]"
    }

    fn args(expr: &str, cache_dir: &std::path::Path, workers: usize) -> EvalArgs {
        EvalArgs {
            input: Some(Input::Expr(expr.to_string())),
            cache_dir: Some(cache_dir.to_string_lossy().into_owned()),
            no_tracing_cache: true,
            no_ir_cache: false,
            workers,
            ..EvalArgs::new()
        }
    }

    /// The run's IR cache, and the entries it read while it evaluated `a`
    /// deeply on `a.workers` fibers.
    fn hits(a: EvalArgs) -> u64 {
        Run::new(&a)
            .expect("the arguments configure")
            .go(move |ev, _| {
                let v = crate::api::evaluate(ev, a).expect("the workload evaluates");
                crate::parallel::force_deep(ev, v).expect("the workload forces");
                ev.ir_cache
                    .as_ref()
                    .expect("the run has an IR cache")
                    .hits()
            })
            .expect("the fiber stacks")
    }

    /// The fiber template is the evaluator as it was when `try_run` cloned
    /// it, so everything `configure` installs must be on it by then.
    #[test]
    fn a_fiber_template_carries_the_configuration() {
        let d = std::env::temp_dir().join(format!("ietsp-run-cfg-{}", std::process::id()));
        let a = args("1", &d, 4);
        let run = Run::new(&a).expect("the arguments configure");
        let fiber = run.ev.clone();
        assert!(fiber.ir_cache.is_some(), "a fiber without the IR cache");
        assert_eq!(fiber.search_path, run.ev.search_path);
        assert_eq!(fiber.lints.config, run.ev.lints.config);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The guard `try_run` puts on that template, in a debug build. Each
    /// test leaves one field of `api::configure` for the main fiber to
    /// install, which is the shape of a bug that shipped: `evaluate` called
    /// `configure` inside the main fiber, so the IR cache and the search
    /// path reached no helper (docs/explanation/evaluation.md).
    #[cfg(debug_assertions)]
    mod guard {
        use super::*;

        /// `evaluate` on four fibers with `ev` as it is: whatever it still
        /// misses, the main fiber installs after the template was cloned.
        fn late(ev: Evaluator, a: EvalArgs) {
            let w = a.workers;
            let _ = Run::on(ev).workers(w).go(move |ev, _| {
                let _ = crate::api::evaluate(ev, a);
            });
        }

        #[test]
        #[should_panic(expected = "`ir_cache` was set after parallel::try_run")]
        fn a_late_ir_cache_trips_the_guard() {
            let d = std::env::temp_dir().join("ietsp-run-late-cache");
            let a = args("1", &d, 4);
            let mut ev = a.evaluator();
            crate::api::configure_lints(&mut ev, &a).expect("the lints resolve");
            crate::builtins::set_search_path(&mut ev, crate::api::full_includes(&a.includes));
            late(ev, a);
        }

        #[test]
        #[should_panic(expected = "`search_path` was set after parallel::try_run")]
        fn a_late_search_path_trips_the_guard() {
            let d = std::env::temp_dir().join("ietsp-run-late-path");
            let mut a = args("1", &d, 4);
            a.includes = vec![(b"nixpkgs".to_vec(), b"/nonexistent".to_vec())];
            let mut ev = a.evaluator();
            crate::api::configure_lints(&mut ev, &a).expect("the lints resolve");
            crate::api::install_ir_cache(&mut ev, &a);
            late(ev, a);
        }
    }

    /// Every fiber shares one IR cache handle, so a warm run reads one entry
    /// per imported file whatever the worker count. With the cache on the
    /// main fiber alone, the helpers compile their imports again and the
    /// four-worker count collapses.
    #[test]
    fn helper_fibers_read_the_ir_cache() {
        let d = std::env::temp_dir().join(format!("ietsp-run-ircache-{}", std::process::id()));
        let n = 400;
        let expr = workload(&d.join("src"), n);
        let cache = d.join("cache");
        hits(args(&expr, &cache, 1));
        let one = hits(args(&expr, &cache, 1));
        let four = hits(args(&expr, &cache, 4));
        let _ = std::fs::remove_dir_all(&d);
        assert_eq!(one, n as u64, "a warm run reads one entry per file");
        assert_eq!(four, one, "helper fibers read the IR cache too");
    }

    /// The derivations this run made, counted after a deep force of the
    /// workload on `a.workers` fibers. A fiber folds its counters when it
    /// parks or finishes its task, so a helper still inside one when the
    /// walk returns is a few counts behind: the read waits for `want`,
    /// yielding, and reports what it has when the deadline passes.
    fn derivations(a: EvalArgs, want: u64) -> u64 {
        Run::new(&a)
            .expect("the arguments configure")
            .go(move |ev, _| {
                let v = crate::api::evaluate(ev, a).expect("the workload evaluates");
                crate::parallel::force_deep(ev, v).expect("the workload forces");
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
                loop {
                    let n = ev.counts().derivations;
                    if n >= want || std::time::Instant::now() > deadline {
                        return n;
                    }
                    std::thread::yield_now();
                }
            })
            .expect("the fiber stacks")
    }

    /// Each fiber counts into its own evaluator and folds those counters
    /// into the run's totals, so a stats line does not depend on the worker
    /// count. The derivation count is deterministic; with the main fiber's
    /// counters alone the four-worker number is a fraction of it.
    #[test]
    fn the_counters_cover_the_helper_fibers() {
        let d = std::env::temp_dir().join(format!("ietsp-run-counts-{}", std::process::id()));
        let n = 400;
        let expr = format!(
            "builtins.genList (i: derivation {{ name = \"c${{toString i}}\"; \
             system = \"x86_64-linux\"; builder = \"/bin/sh\"; }}) {n}"
        );
        let one = derivations(args(&expr, &d, 1), n as u64);
        let four = derivations(args(&expr, &d, 4), one);
        let _ = std::fs::remove_dir_all(&d);
        assert_eq!(one, n as u64, "one derivation per list element");
        assert_eq!(four, one, "the helper fibers' derivations are counted too");
    }
}

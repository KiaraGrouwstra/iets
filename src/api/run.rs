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
    /// The evaluator `a` asks for, with its lints installed. `Err` is the
    /// exit code of a lint configuration that does not resolve.
    pub fn new(a: &EvalArgs) -> Result<Run, i32> {
        let mut ev = a.evaluator();
        if let Err(e) = super::configure_lints(&mut ev, a) {
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

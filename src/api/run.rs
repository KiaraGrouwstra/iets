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
}

//! `ietsp eval`: evaluate an expression and print it.

use std::collections::HashSet;

use crate::cli::{EvalArgs, evaluate, report};
use crate::eval::{Evaluator, Pos};
use crate::trace::run::Cache;

pub fn run_eval(a: EvalArgs) -> i32 {
    let mut ev = Evaluator::new();
    if let Err(e) = super::configure_lints(&mut ev, &a) {
        report(&ev, &e);
        return 1;
    }
    let cache = super::setup_cache(&a, "eval", &ev);
    if let Some(c) = &cache
        && c.lookup()
        && let Some(out) = c.store.read(&c.key, crate::trace::store::RESULT)
    {
        use std::io::Write;
        c.replay_warnings();
        let _ = std::io::stdout().write_all(&out);
        let _ = std::io::stdout().flush();
        return 0;
    }
    if cache.is_some() {
        ev.trace = crate::trace::Trace::enabled();
    }
    // only the cache and --dump-drvs read the ATerm text after evaluation
    ev.drv.keep_aterm = cache.is_some() || a.dump_drvs.is_some();
    // parallel: the rest runs as the main fiber
    crate::parallel::run(a.workers, ev, move |ev| eval_and_print(ev, a, cache))
}

fn eval_and_print(ev: &mut Evaluator, a: EvalArgs, cache: Option<Cache>) -> i32 {
    let strict = a.strict;
    let json = a.json;
    let raw = a.raw;
    let t0 = std::time::Instant::now();
    let timings = a.timings;
    let v = match evaluate(ev, a) {
        Ok(v) => v,
        Err(e) => {
            report(ev, &e);
            return 1;
        }
    };

    let mut out: Vec<u8> = Vec::new();
    let r = if raw {
        // nix eval --raw: coerce without copying to the store, no newline
        let mut ctx = Vec::new();
        ev.coerce_to_string(
            v,
            &mut ctx,
            Pos::NONE,
            "while generating the eval command output",
            false,
            false,
            true,
        )
        .map(|s| out.extend_from_slice(&s))
    } else if json {
        let mut ctx = Vec::new();
        crate::print::print_json(ev, v, &mut out, &mut ctx, strict, Pos::NONE)
    } else {
        (|| {
            let v = if strict {
                ev.force_deep(v)?
            } else {
                ev.force(v)?
            };
            let mut seen = HashSet::new();
            crate::print::print_ambiguous(ev, v, 1, &mut out, &mut seen, 0)
        })()
    };
    if crate::settings::get().stats >= 1 {
        ev.print_stats();
    }
    match r {
        Ok(()) => {
            if !raw {
                out.push(b'\n');
            }
            use std::io::Write;
            let _ = std::io::stdout().write_all(&out);
            let _ = std::io::stdout().flush();
            if timings {
                eprintln!("eval: {:.3}s", t0.elapsed().as_secs_f64());
                crate::inputs::report_timings();
            }
            if let Some(c) = &cache {
                c.write(ev, &out, false);
            }
            0
        }
        Err(e) => {
            report(ev, &e);
            1
        }
    }
}

//! Attribute completion: `FLAKEREF#fragment` and `-A`. It evaluates, so it
//! is limited in time (`IETS_COMPLETE_MS`) and silent on stderr.

use crate::session::exit;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use crate::api::installable::{self, Prefixes};
use crate::api::run::Run;
use crate::api::{EvalArgs, Input};
use crate::cli::complete::{NO_FILES, NO_SPACE};
use crate::cli::help;
use crate::session::{self, Session};

static ANSWERED: AtomicBool = AtomicBool::new(false);

/// Call it on the evaluator's stack.
pub fn flake_fragment(p: Prefixes, word: &str) -> Vec<String> {
    let Some((loc, fragment)) = word.split_once('#') else {
        return Vec::new();
    };
    let at = if loc.is_empty() { "." } else { loc }.to_string();
    let fragment = fragment.to_string();
    let args = args(Input::Flake(at.clone()));
    let names = guarded(|| {
        let run = Run::new(&args).ok()?;
        run.go(move |ev, _| installable::complete_in_flake(ev, &at, p, &fragment))
            .ok()?
    });
    names.into_iter().map(|n| format!("{loc}#{n}")).collect()
}

pub fn attr_path(input: Input, prefix: &str) -> Vec<String> {
    let prefix = prefix.to_string();
    let args = args(input.clone());
    guarded(|| {
        let run = Run::new(&args).ok()?;
        run.go(move |ev, _| installable::complete_in_input(ev, &input, &prefix))
            .ok()?
    })
}

fn args(input: Input) -> EvalArgs {
    EvalArgs {
        input: Some(input),
        no_tracing_cache: true,
        workers: 1,
        ..EvalArgs::new()
    }
}

fn guarded(f: impl FnOnce() -> Option<Vec<String>>) -> Vec<String> {
    Session::current().log.write_to(Box::new(io::sink()));
    watchdog();
    let names = f().unwrap_or_default();
    if ANSWERED.swap(true, Ordering::SeqCst) {
        loop {
            thread::park();
        }
    }
    names
}

fn watchdog() {
    let ms = Session::current().settings().complete_ms;
    if ms == 0 {
        return;
    }
    let work = session::bind(move || {
        thread::sleep(Duration::from_millis(ms));
        if !ANSWERED.swap(true, Ordering::SeqCst) {
            help::out(&format!(":{}\n", NO_SPACE | NO_FILES));
            exit(0);
        }
    });
    let _ = thread::Builder::new().spawn(work);
}

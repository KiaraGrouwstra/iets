#![no_main]

use iets_fuzz::{Checker, Verdict, known};
use libfuzzer_sys::{Corpus, fuzz_target};
use std::sync::{LazyLock, Mutex};

static CHECKER: LazyLock<Mutex<Checker>> = LazyLock::new(|| {
    Mutex::new(Checker::new(
        known::from_env().unwrap_or_else(|e| panic!("{e}")),
    ))
});

fuzz_target!(|src: &[u8]| -> Corpus {
    match CHECKER.lock().unwrap().check(src) {
        Verdict::Same => Corpus::Keep,
        // a known divergence teaches the fuzzer nothing new
        Verdict::Expected(_) => Corpus::Reject,
        Verdict::Bug(d) => panic!(
            "parse divergence ({}{})\n  nix:  {}\n  iets: {}",
            d.kind.name(),
            if d.at.is_empty() {
                String::new()
            } else {
                format!(" at {}", d.at)
            },
            d.nix,
            d.iets
        ),
    }
});

//! Differential fuzzing of iets against Nix 2.34.8 (docs/how-to/fuzz.md).

pub mod canon;
pub mod known;
pub mod nar;
pub mod oracle;
pub mod parse;
pub mod sx;

/// One input that the two sides answer differently.
pub struct Divergence {
    /// What differs; one of the target's `KINDS`. A known divergence
    /// matches on it.
    pub kind: &'static str,
    pub nix: String,
    pub iets: String,
    /// Where two trees part, or empty.
    pub at: String,
}

pub enum Verdict {
    Same,
    Expected(String),
    Bug(Divergence),
}

pub trait Target {
    fn check(&mut self, input: &[u8]) -> Verdict;
}

/// The verdict on `d`: expected when an entry of `known` matches.
pub fn classify(known: &[known::Entry], d: Option<Divergence>, input: &[u8]) -> Verdict {
    let Some(d) = d else { return Verdict::Same };
    match known.iter().find(|k| k.matches(&d, input)) {
        Some(k) => Verdict::Expected(k.id.clone()),
        None => Verdict::Bug(d),
    }
}

/// Runs `f`, turning a panic into its message.
pub fn no_panic<T>(f: impl FnOnce() -> T) -> Result<T, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).map_err(|p| {
        p.downcast_ref::<String>()
            .cloned()
            .or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string()))
            .unwrap_or_default()
    })
}

/// The target `name`, with its known divergences.
pub fn target(name: &str) -> Result<Box<dyn Target + Send>, String> {
    Ok(match name {
        "parse" => Box::new(parse::Parse::new(known::from_env("parse", parse::KINDS)?)),
        "nar-read" | "nar-gen" | "nar-tree" => {
            let mode = match name {
                "nar-read" => nar::Mode::Read,
                "nar-gen" => nar::Mode::Gen,
                _ => nar::Mode::Tree,
            };
            Box::new(nar::Nar::new(known::from_env("nar", nar::KINDS)?, mode))
        }
        _ => {
            return Err(format!(
                "unknown target `{name}`: parse, nar-read, nar-gen or nar-tree"
            ));
        }
    })
}

/// What libFuzzer does with an input: keep it, drop it (a known divergence
/// teaches the fuzzer nothing new), or stop on a bug.
pub fn corpus(name: &str, v: Verdict) -> libfuzzer_sys::Corpus {
    match v {
        Verdict::Same => libfuzzer_sys::Corpus::Keep,
        Verdict::Expected(_) => libfuzzer_sys::Corpus::Reject,
        Verdict::Bug(d) => panic!(
            "{name} divergence ({}{})\n  nix:  {}\n  iets: {}",
            d.kind,
            if d.at.is_empty() {
                String::new()
            } else {
                format!(" at {}", d.at)
            },
            d.nix,
            d.iets
        ),
    }
}

/// The body of a fuzz target binary.
#[macro_export]
macro_rules! fuzz_main {
    ($name:literal) => {
        static TARGET: std::sync::LazyLock<std::sync::Mutex<Box<dyn $crate::Target + Send>>> =
            std::sync::LazyLock::new(|| {
                std::sync::Mutex::new($crate::target($name).unwrap_or_else(|e| panic!("{e}")))
            });
        libfuzzer_sys::fuzz_target!(|input: &[u8]| -> libfuzzer_sys::Corpus {
            $crate::corpus($name, TARGET.lock().unwrap().check(input))
        });
    };
}

//! `ietsp fast-build`'s command line, the same names and meanings as
//! nix-fast-build's (options.py). Three classes of flag: the ones ietsp
//! implements, the ones it accepts and ignores, and the ones it refuses
//! because the feature is not its job. Anything else is not a flag ietsp
//! knows: `nix-fast-build` falls back to the real one, the way classic
//! mode falls back; `ietsp fast-build` is an error (docs/fast-build.md).

use crate::cli::{EvalFlags, eval_flag};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Json,
    Junit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvalMode {
    Flake,
    File,
}

pub struct Args {
    pub mode: EvalMode,
    /// `--flake URL[#fragment]`, default `.#checks`.
    pub flake: String,
    /// `--file [FILE]`, default `default.nix`.
    pub file: String,
    /// `-A`, `--arg`, `-I`; `overrides` is every `--option`, `pure-eval`
    /// included, for `Config::load`.
    pub flags: EvalFlags,
    pub impure: bool,
    /// `-j`; `None` means "take `max-jobs` from the settings".
    pub max_jobs: Option<u64>,
    /// `--store`; `None` means "take `builder` from the ietsp settings".
    pub store: Option<String>,
    pub eval_workers: Option<usize>,
    pub skip_cached: bool,
    pub skip_unsupported: bool,
    /// `--systems "a b"`; empty means "the current system".
    pub systems: Vec<String>,
    pub out_link: Option<String>,
    pub no_download: bool,
    pub retries: usize,
    pub fail_fast: bool,
    pub result_file: Option<std::path::PathBuf>,
    pub result_format: Format,
    pub stream_json_lines: bool,
    pub no_fold: bool,
    pub select: Option<String>,
    pub debug: bool,
}

impl Default for Args {
    fn default() -> Args {
        Args {
            mode: EvalMode::Flake,
            flake: ".#checks".into(),
            file: "default.nix".into(),
            flags: EvalFlags::default(),
            impure: false,
            max_jobs: None,
            store: None,
            eval_workers: None,
            skip_cached: false,
            skip_unsupported: false,
            systems: Vec::new(),
            out_link: None,
            no_download: false,
            retries: 0,
            fail_fast: false,
            result_file: None,
            result_format: Format::Json,
            stream_json_lines: false,
            no_fold: false,
            select: None,
            debug: false,
        }
    }
}

/// Why a command line cannot run here.
pub enum Reject {
    /// A feature ietsp will not grow; the run stops.
    Refuse(String),
    /// Not a command line ietsp runs: `nix-fast-build` falls back to the
    /// real binary, `ietsp fast-build` reports it.
    Unknown(String),
    /// A flag ietsp does not know, handled the same way.
    Flag(String),
}

impl From<String> for Reject {
    fn from(m: String) -> Reject {
        Reject::Unknown(m)
    }
}

/// The uploaders nix-fast-build has and ietsp does not: pushing to a binary
/// cache is the farm's job, and there is no `--remote` mode by design.
const REFUSED: &[(&str, usize)] = &[
    ("--remote", 1),
    ("--remote-ssh-option", 2),
    ("--always-upload-source", 0),
    ("--copy-to", 1),
    ("--cachix-cache", 1),
    ("--attic-cache", 1),
    ("--attic-ignore-upstream-cache-filter", 0),
    ("--attic-push-build-closure", 0),
    ("--push-build-closure", 0),
    ("--niks3-server", 1),
];

/// Accepted and ignored, with the value they take.
const IGNORED: &[(&str, usize)] = &[
    // ietsp renders itself, so the nom flag is already the default.
    ("--no-nom", 0),
    ("--no-link", 0),
    // ietsp has no worker processes to restart.
    ("--eval-max-memory-size", 1),
    ("--stall-timeout", 1),
];

/// The binaries nix-fast-build shells out to and ietsp does not.
const IGNORED_BINARIES: &[&str] = &["--nix", "--nix-eval-jobs", "--nix-build"];

fn note(msg: &str) {
    eprintln!("ietsp fast-build: {msg}");
}

pub fn parse(argv: &[String]) -> Result<Args, Reject> {
    let mut a = Args::default();
    let mut flake_given = false;
    let mut pure_given = false;
    let mut i = 0;
    let value = |i: &mut usize, flag: &str| -> Result<String, String> {
        *i += 1;
        argv.get(*i)
            .cloned()
            .ok_or_else(|| format!("'{flag}' requires an argument"))
    };
    while i < argv.len() {
        let s = argv[i].clone();
        if let Some((flag, arity)) = REFUSED.iter().find(|(f, _)| *f == s.as_str()) {
            i += arity;
            return Err(Reject::Refuse(format!(
                "{flag} is not supported by ietsp fast-build"
            )));
        }
        if let Some((flag, arity)) = IGNORED.iter().find(|(f, _)| *f == s.as_str()) {
            note(&format!("{flag} accepted and ignored"));
            i += arity + 1;
            continue;
        }
        if IGNORED_BINARIES.contains(&s.as_str()) {
            let v = value(&mut i, &s)?;
            note(&format!(
                "{s} {v} accepted and ignored: ietsp evaluates and builds in this process"
            ));
            i += 1;
            continue;
        }
        match s.as_str() {
            "-f" | "--flake" => {
                a.flake = value(&mut i, &s)?;
                flake_given = true;
            }
            "--file" => {
                a.mode = EvalMode::File;
                // The argument is optional: `--file` alone means default.nix.
                if let Some(next) = argv.get(i + 1)
                    && !next.starts_with('-')
                {
                    a.file = next.clone();
                    i += 1;
                }
            }
            "-A" | "--attr" | "--arg" | "--argstr" | "-I" | "--include" => {
                eval_flag(&mut a.flags, &s, || value(&mut i, &s))?;
            }
            "--impure" => a.impure = true,
            "--pure" => pure_given = true,
            "--option" => {
                let pair = (value(&mut i, &s)?, value(&mut i, &s)?);
                a.flags.overrides.push(pair);
            }
            "-j" | "--max-jobs" => {
                let v = value(&mut i, &s)?;
                a.max_jobs =
                    Some(v.parse().map_err(|_| {
                        Reject::Unknown(format!("'{s}' wants a number, got '{v}'"))
                    })?);
            }
            "--store" => a.store = Some(value(&mut i, &s)?),
            "--eval-workers" => {
                let v = value(&mut i, &s)?;
                a.eval_workers =
                    Some(v.parse().map_err(|_| {
                        Reject::Unknown(format!("'{s}' wants a number, got '{v}'"))
                    })?);
            }
            "--skip-cached" => a.skip_cached = true,
            "--skip-unsupported" => a.skip_unsupported = true,
            "--systems" => {
                a.systems = value(&mut i, &s)?
                    .split_whitespace()
                    .map(str::to_string)
                    .collect();
            }
            "--out-link" => a.out_link = Some(value(&mut i, &s)?),
            "--no-download" => a.no_download = true,
            "--retries" => {
                let v = value(&mut i, &s)?;
                a.retries = v
                    .parse()
                    .map_err(|_| Reject::Unknown(format!("'{s}' wants a number, got '{v}'")))?;
            }
            "--fail-fast" => a.fail_fast = true,
            "--result-file" => a.result_file = Some(value(&mut i, &s)?.into()),
            "--result-format" => {
                a.result_format = match value(&mut i, &s)?.as_str() {
                    "json" => Format::Json,
                    "junit" => Format::Junit,
                    other => {
                        return Err(Reject::Unknown(format!(
                            "'--result-format {other}': expected json or junit"
                        )));
                    }
                }
            }
            "--stream-json-lines" => a.stream_json_lines = true,
            "--no-fold" => a.no_fold = true,
            "--debug" => a.debug = true,
            "--select" => a.select = Some(value(&mut i, &s)?),
            "--override-input" => {
                return Err(Reject::Refuse(
                    "--override-input is not supported by ietsp fast-build: ietsp evaluates locked flakes only".into(),
                ));
            }
            "--reference-lock-file" => {
                return Err(Reject::Refuse(
                    "--reference-lock-file is not supported by ietsp fast-build: ietsp reads the flake's own flake.lock only".into(),
                ));
            }
            other => return Err(Reject::Flag(other.to_string())),
        }
        i += 1;
    }
    if flake_given && a.mode == EvalMode::File {
        return Err(Reject::Unknown(
            "--flake and --file are mutually exclusive".into(),
        ));
    }
    // options.py: --file mode is impure unless --pure says otherwise.
    if a.mode == EvalMode::File {
        a.impure = !pure_given;
    }
    Ok(a)
}

impl Args {
    /// What the reports call this run (options.py:display_name).
    pub fn display_name(&self) -> String {
        match self.mode {
            EvalMode::Flake => self.flake.clone(),
            EvalMode::File => match self.flags.attrs.last() {
                Some(a) => format!("{}.{a}", self.file),
                None => self.file.clone(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Args {
        match parse(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>()) {
            Ok(a) => a,
            Err(Reject::Refuse(m) | Reject::Unknown(m) | Reject::Flag(m)) => panic!("{m}"),
        }
    }

    fn reject(v: &[&str]) -> Reject {
        parse(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>())
            .err()
            .expect("rejected")
    }

    #[test]
    fn defaults_match_nix_fast_build() {
        let a = args(&[]);
        assert_eq!(a.mode, EvalMode::Flake);
        assert_eq!(a.flake, ".#checks");
        assert_eq!(a.store, None);
        assert_eq!(a.result_format, Format::Json);
        assert!(!a.impure);
        assert_eq!(a.max_jobs, None);
    }

    #[test]
    fn file_mode_is_impure_unless_pure() {
        let a = args(&["--file"]);
        assert_eq!(a.mode, EvalMode::File);
        assert_eq!(a.file, "default.nix");
        assert!(a.impure);
        let a = args(&["--file", "release.nix", "--pure", "-A", "x"]);
        assert_eq!(a.file, "release.nix");
        assert_eq!(a.flags.attrs, ["x"]);
        assert!(!a.impure);
    }

    #[test]
    fn the_flags_nix_farm_passes() {
        let a = args(&[
            "--store",
            "ssh-ng://farm.example",
            "--skip-cached",
            "-j",
            "8",
            "--systems",
            "x86_64-linux aarch64-linux",
            "--result-file",
            "r.xml",
            "--result-format",
            "junit",
            "--out-link",
            "result",
            "--retries",
            "2",
            "--fail-fast",
            "--stream-json-lines",
            "--no-fold",
            "--flake",
            ".#checks.x86_64-linux",
        ]);
        assert_eq!(a.store.as_deref(), Some("ssh-ng://farm.example"));
        assert_eq!(a.max_jobs, Some(8));
        assert_eq!(a.systems, ["x86_64-linux", "aarch64-linux"]);
        assert_eq!(a.result_format, Format::Junit);
        assert_eq!(a.out_link.as_deref(), Some("result"));
        assert_eq!(a.retries, 2);
        assert!(a.fail_fast && a.stream_json_lines && a.no_fold && a.skip_cached);
        assert_eq!(a.flake, ".#checks.x86_64-linux");
    }

    #[test]
    fn uploads_are_refused_and_unknown_flags_fall_back() {
        assert!(matches!(
            reject(&["--cachix-cache", "mic92"]),
            Reject::Refuse(m) if m.contains("not supported by ietsp fast-build")
        ));
        assert!(matches!(reject(&["--remote", "host"]), Reject::Refuse(_)));
        assert!(matches!(
            reject(&["--override-input", "nixpkgs", "x"]),
            Reject::Refuse(_)
        ));
        assert!(matches!(reject(&["--what"]), Reject::Flag(_)));
        assert!(matches!(reject(&["--flake"]), Reject::Unknown(_)));
    }

    #[test]
    fn accepted_and_ignored() {
        let a = args(&[
            "--no-nom",
            "--no-link",
            "--debug",
            "--nix",
            "/bin/nix",
            "--eval-max-memory-size",
            "2048",
            "--stall-timeout",
            "60",
            "--eval-workers",
            "4",
        ]);
        assert!(a.debug);
        assert_eq!(a.eval_workers, Some(4));
    }

    #[test]
    fn display_names() {
        assert_eq!(args(&["--flake", ".#checks"]).display_name(), ".#checks");
        assert_eq!(
            args(&["--file", "ci.nix", "-A", "pkg"]).display_name(),
            "ci.nix.pkg"
        );
    }
}

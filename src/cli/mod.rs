//! The `ietsp` command line: argument parsing shared by the subcommands and
//! one module per subcommand. Everything that is not about parsing a command
//! line lives in `crate::api`.

pub mod build;
pub mod cache_dump;
pub mod cache_gc;
pub mod classic;
pub mod devenv;
pub mod eval;
pub mod fastbuild;
pub mod jobs;
pub mod parse;

use std::process::exit;

use crate::eval::{Error, Evaluator};
use crate::runtime::Word;

pub use crate::api::{
    ArgKind, EvalArgs, Input, abs_path, auto_args, build_targets, cwd, flake_ref, full_includes,
    parse_include, report, setup_cache,
};

pub fn usage() -> ! {
    eprintln!(
        "usage: ietsp eval [--strict] [--json] [--raw] [--workers N] [--timings] [--no-cache] [--no-ir-cache] [--cache-dir DIR] [-I prefix=path]... [--arg name expr] [--argstr name str] [-A attr] [--dump-drvs DIR] (-E EXPR | FILE)\n       ietsp parse [-q] FILE...   (FILE may be - to read a list from stdin)\n       ietsp cache-dump READSET   (print a cache entry's read set)\n       ietsp cache gc [--ttl DAYS] [--dry-run] [--cache-dir DIR]   (sweep old cache entries)\n       ietsp build [--builder URL] [--timings] [--no-build] [--fetch-to DIR] [--no-cache] [--cache-dir DIR] [--out NAME]... [--workers N] [-I prefix=path]... [--arg name expr] [--argstr name str] [-A attr] (-E EXPR | FILE)\n       ietsp build [--builder URL] [--timings] [--fetch-to DIR] [--out NAME]... [--source STOREPATH=LOCAL]... DRV\n       ietsp eval-jobs [--workers N] [--force-recurse] [--meta] [--provenance] [--show-input-drvs] [--chunks M --chunk N [--chunk-depth D]] [--max-memory-size MB (0: one process)] [-I prefix=path]... [--arg name expr] [--argstr name str] (-E EXPR | FILE | FLAKEREF#attr)\n       ietsp fast-build [--flake FLAKEREF[#attr] | --file [FILE] [-A attr]] [--store URL] [-j N] [--systems \"a b\"] [--skip-cached] [--skip-unsupported] [--out-link PREFIX] [--retries N] [--fail-fast] [--result-file PATH] [--result-format json|junit] [--stream-json-lines] [--select EXPR]   (default: .#checks, docs/fast-build.md)\n       ietsp print-dev-env [--builder URL] [--gc-root PATH] [--json] [--workers N] [--no-cache] [--cache-dir DIR] [--timings] [FLAKEREF[#attr]]   (default: ., a daemon URL)\n       URL: daemon, unix:///path, ssh-ng://[user@]host (Nix worker protocol), grpc://host:port (nix-grpc-store); without a flag, the `builder` setting\n       every command also takes --ietsp-option NAME VALUE\n       ietsp.conf settings: docs/config.md; IETSP_* environment settings: docs/settings.md"
    );
    exit(2)
}

pub fn parse_eval_args(argv: &[String]) -> EvalArgs {
    let mut a = EvalArgs::new();
    let mut i = 0;
    while i < argv.len() {
        let s = argv[i].as_str();
        match s {
            "--strict" => a.strict = true,
            "--json" => a.json = true,
            "--raw" => a.raw = true,
            "--timings" => a.timings = true,
            "--no-cache" => a.no_cache = true,
            "--no-ir-cache" => a.no_ir_cache = true,
            "--cache-dir" => {
                i += 1;
                a.cache_dir = Some(argv.get(i).unwrap_or_else(|| usage()).clone());
            }
            "-I" | "--include" => {
                i += 1;
                a.includes
                    .push(parse_include(argv.get(i).unwrap_or_else(|| usage())));
            }
            "-E" | "--expr" => {
                i += 1;
                a.input = Some(Input::Expr(argv.get(i).unwrap_or_else(|| usage()).clone()));
            }
            "--arg" | "--argstr" => {
                let name = argv.get(i + 1).unwrap_or_else(|| usage()).clone();
                let val = argv.get(i + 2).unwrap_or_else(|| usage()).clone();
                a.args.push((
                    name,
                    if s == "--arg" {
                        ArgKind::Expr(val)
                    } else {
                        ArgKind::Str(val)
                    },
                ));
                i += 2;
            }
            "-A" | "--attr" => {
                i += 1;
                a.attr = Some(argv.get(i).unwrap_or_else(|| usage()).clone());
            }
            "--dump-drvs" => {
                i += 1;
                a.dump_drvs = Some(argv.get(i).unwrap_or_else(|| usage()).clone());
            }
            "--workers" => {
                i += 1;
                a.workers = argv
                    .get(i)
                    .and_then(|v| v.parse().ok())
                    .filter(|n| *n >= 1)
                    .unwrap_or_else(|| usage());
            }
            "--option" => {
                // accepted for nix-instantiate compatibility; ignored
                i += 2;
            }
            "--lint-absolute-path-literals"
            | "--lint-short-path-literals"
            | "--lint-url-literals" => {
                i += 1;
                let v = argv.get(i).unwrap_or_else(|| usage());
                let Some(mode) = crate::compile::Lint::parse(v) else {
                    eprintln!("error: invalid lint mode '{v}'");
                    usage()
                };
                match s {
                    "--lint-absolute-path-literals" => a.lint_absolute = mode,
                    "--lint-short-path-literals" => a.lint_short = mode,
                    _ => {}
                }
            }
            "--impure" | "--pure-eval" | "--readonly-mode" => {}
            "--extra-experimental-features" | "--experimental-features" => {
                i += 1;
                let v = argv.get(i).unwrap_or_else(|| usage());
                a.xp_features
                    .extend(v.split_whitespace().map(str::to_owned));
            }
            _ if s.starts_with("-I") && s.len() > 2 => a.includes.push(parse_include(&s[2..])),
            _ if s.starts_with('-') && s != "-" => {
                eprintln!("error: unknown flag '{s}'");
                usage()
            }
            _ if crate::flake::is_flake_ref(s) => a.input = Some(Input::Flake(s.to_string())),
            _ => a.input = Some(Input::File(s.to_string())),
        }
        i += 1;
    }
    a
}

/// `api::evaluate`, with a missing input reported as a usage error.
pub fn evaluate(ev: &mut Evaluator, a: EvalArgs) -> Result<Word, Box<Error>> {
    if a.input.is_none() {
        usage()
    }
    crate::api::evaluate(ev, a)
}

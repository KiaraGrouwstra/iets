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
pub mod help;
pub mod jobs;
pub mod parse;

use help::{arg_error, value};

use crate::eval::{Error, Evaluator};
use crate::runtime::Word;

pub use crate::api::{
    ArgKind, EvalArgs, Input, abs_path, auto_args, build_targets, configure_lints, cwd, flake_ref,
    full_includes, parse_include, report, setup_cache,
};

/// The `pure-eval` a flag sets (common-eval-args.cc), `None` for any other.
pub fn pure_flag(s: &str) -> Option<bool> {
    match s {
        "--pure-eval" => Some(true),
        "--no-pure-eval" | "--impure" => Some(false),
        _ => None,
    }
}

pub fn parse_eval_args(argv: &[String]) -> EvalArgs {
    let mut a = EvalArgs::new();
    let mut i = 0;
    while i < argv.len() {
        let s = argv[i].as_str();
        let mut next = || {
            i += 1;
            value(argv, i, s).clone()
        };
        match s {
            "--strict" => a.strict = true,
            "--json" => a.json = true,
            "--raw" => a.raw = true,
            "--timings" => a.timings = true,
            "--no-cache" => a.no_cache = true,
            "--no-ir-cache" => a.no_ir_cache = true,
            "--cache-dir" => a.cache_dir = Some(next()),
            "-I" | "--include" => a.includes.push(parse_include(&next())),
            "-E" | "--expr" => {
                let e = next();
                set_input(&mut a, Input::Expr(e));
            }
            "--arg" | "--argstr" => {
                let (name, val) = (next(), next());
                a.args.push((
                    name,
                    if s == "--arg" {
                        ArgKind::Expr(val)
                    } else {
                        ArgKind::Str(val)
                    },
                ));
            }
            "-A" | "--attr" => a.attr = Some(next()),
            "--dump-drvs" => a.dump_drvs = Some(next()),
            "--workers" => {
                let v = next();
                a.workers = v.parse().ok().filter(|n| *n >= 1).unwrap_or_else(|| {
                    arg_error(format!("--workers wants a number >= 1, got '{v}'"))
                });
            }
            "--option" => {
                let (name, v) = (next(), next());
                if name == "pure-eval" {
                    a.pure = Some(
                        crate::config::parse_bool(&name, &v)
                            .unwrap_or_else(|e| arg_error(e.to_string())),
                    );
                }
            }
            "--lint" => a.lints.push(parse_lint_flag(&next())),
            "--lint-root" => a.lint_roots.push(next()),
            "--lint-scope" => a.lint_scope = Some(parse_lint_scope(&next())),
            "--lint-absolute-path-literals"
            | "--lint-short-path-literals"
            | "--lint-url-literals" => {
                let v = next();
                let Some(level) = crate::lint::Level::parse(&v) else {
                    arg_error(format!("invalid lint mode '{v}'"))
                };
                a.lints.push((s[2..].to_string(), level));
            }
            _ if pure_flag(s).is_some() => a.pure = pure_flag(s),
            "--readonly-mode" => {}
            "--extra-experimental-features" | "--experimental-features" => {
                let v = next();
                a.xp_features
                    .extend(v.split_whitespace().map(str::to_owned));
            }
            _ if s.starts_with("-I") && s.len() > 2 => a.includes.push(parse_include(&s[2..])),
            _ if s.starts_with('-') && s != "-" => help::unknown_flag(s),
            _ if crate::flake::is_flake_ref(s) => set_input(&mut a, Input::Flake(s.to_string())),
            _ => set_input(&mut a, Input::File(s.to_string())),
        }
        i += 1;
    }
    a
}

/// The last input wins, as it did, but an expression and a file together is
/// a mistake worth naming.
fn set_input(a: &mut EvalArgs, input: Input) {
    if let Some(old) = &a.input
        && matches!(old, Input::Expr(_)) != matches!(input, Input::Expr(_))
    {
        arg_error("-E and FILE cannot be combined")
    }
    a.input = Some(input);
}

/// `--lint SELECTOR=LEVEL`: the selector is checked against the registry
/// now, so a typo is a usage error before anything runs.
pub fn parse_lint_flag(v: &str) -> (String, crate::lint::Level) {
    let mut probe = crate::lint::Config::default();
    if let Err(m) = probe.set_pair(v) {
        arg_error(format!("--lint {v}: {m}"))
    }
    let (name, level) = v.split_once('=').expect("set_pair accepted the pair");
    (
        name.trim().to_string(),
        crate::lint::Level::parse(level.trim()).expect("set_pair accepted the level"),
    )
}

/// `--lint-scope root|all`.
pub fn parse_lint_scope(v: &str) -> crate::lint::Scope {
    crate::lint::Scope::parse(v)
        .unwrap_or_else(|| arg_error(format!("--lint-scope {v}: use root or all")))
}

/// `ietsp lint list [--lint SELECTOR=LEVEL]... [--lint-root DIR]...
/// [--lint-scope root|all] [PATH]`: the registry with the levels a run
/// from `PATH` (default: cwd) would use.
pub fn cmd_lint(argv: &[String]) -> i32 {
    match argv.first().map(String::as_str) {
        Some("list") => {}
        w => help::unknown_subcommand(w, &["list"]),
    }
    let mut a = EvalArgs::new();
    let mut i = 1;
    while i < argv.len() {
        let s = argv[i].as_str();
        let mut next = || {
            i += 1;
            value(argv, i, s).clone()
        };
        match s {
            "--lint" => a.lints.push(parse_lint_flag(&next())),
            "--lint-root" => a.lint_roots.push(next()),
            "--lint-scope" => a.lint_scope = Some(parse_lint_scope(&next())),
            s if s.starts_with('-') => help::unknown_flag(s),
            s => a.input = Some(Input::File(s.to_string())),
        }
        i += 1;
    }
    match crate::api::lint_config(&a) {
        Ok(c) => {
            print!("{}", crate::lint::list(&c));
            0
        }
        Err(e) => {
            eprintln!("error: {}", e.msg);
            1
        }
    }
}

/// `api::evaluate`, with a missing input reported as a usage error.
pub fn evaluate(ev: &mut Evaluator, a: EvalArgs) -> Result<Word, Box<Error>> {
    if a.input.is_none() {
        arg_error("nothing to evaluate: give -E EXPR or FILE")
    }
    crate::api::evaluate(ev, a)
}

use std::process::exit;

use ietsp::cli::{self, help};

const STACK_SIZE: usize = 4 << 30;

/// Rust masks SIGPIPE, so a closed stdout (`ietsp eval-jobs | head`) surfaces
/// as an EPIPE panic. The default disposition makes the process die quietly.
fn restore_sigpipe() {
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
}

/// The classic Nix name this process was invoked under, if any.
fn classic_name() -> Option<String> {
    let argv0 = std::env::args_os().next()?;
    let base = std::path::Path::new(&argv0)
        .file_name()?
        .to_str()?
        .to_string();
    cli::classic::NAMES.contains(&base.as_str()).then_some(base)
}

/// Classic commands need the evaluator's stack, like the subcommands below.
fn on_big_stack(f: impl FnOnce() -> i32 + Send + 'static) -> i32 {
    std::thread::Builder::new()
        .stack_size(STACK_SIZE)
        .spawn(ietsp::session::bind(f))
        .expect("spawn evaluator thread")
        .join()
        .unwrap_or(1)
}

fn main() {
    restore_sigpipe();
    let session = ietsp::session::Session::new();
    // a command line says what it has to say on stderr; a library says it
    // to whatever sink its host set, or to none (docs/reference/library.md)
    session.log.write_to_stderr();
    let _entered = session.enter();
    let argv: Vec<String> = std::env::args_os()
        .skip(1)
        .map(ietsp::argv::encode)
        .collect();
    // `--ietsp-option` is ours, so it leaves the argv here: a classic fallback
    // execs the real nix with what is left (docs/reference/settings.md).
    let (argv, options) = match ietsp::settings::take_options(&argv) {
        Ok(a) => a,
        Err(e) => {
            e.report();
            exit(2)
        }
    };
    // the log format shapes every record, an error's included, so it is read
    // before the first thing that can fail
    if let Some(i) = argv.iter().position(|a| a == "--log-format") {
        let _ = ietsp::log::set_format(argv.get(i + 1).map_or("", String::as_str));
    }
    // `version` and `help` read no setting, so a settings file with a
    // mistake in it does not stop them
    if classic_name().is_none() {
        match argv.first().map(String::as_str) {
            Some("--version" | "version") => {
                cli::render::bytes(concat!(env!("IETSP_VERSION"), "\n").as_bytes());
                exit(0)
            }
            Some("--help" | "-h" | "help") => {
                help::set_command("ietsp");
                help::help(argv.get(1).map(String::as_str))
            }
            _ => {}
        }
    }
    if let Err(e) = session.load_settings(&options) {
        exit(e.report())
    }
    if let Some(name) = classic_name() {
        exit(on_big_stack(move || cli::classic::dispatch(&name, &argv)))
    }
    let Some(cmd) = argv.first() else {
        eprint!("{}", help::OVERVIEW);
        exit(2)
    };
    if cli::classic::NAMES.contains(&cmd.as_str()) {
        let name = cmd.clone();
        let rest: Vec<String> = argv[1..].to_vec();
        exit(on_big_stack(move || cli::classic::dispatch(&name, &rest)))
    }
    let mut rest: Vec<String> = argv[1..].to_vec();
    help::set_command("ietsp");
    let cmd = match cmd.as_str() {
        "cache-dump" => {
            rest.insert(0, "dump".into());
            "cache"
        }
        c => c,
    };
    let Some(name) = help::COMMANDS.split(' ').find(|c| *c == cmd) else {
        if cmd.starts_with('-') {
            help::unknown_flag(cmd)
        }
        help::unknown_command(cmd)
    };
    help::set_command(name);
    // these read their flags in order, so `--help` reaches `eval_flag` where
    // it stands and a flag before it may take it as a value
    if !matches!(name, "eval" | "diff" | "eval-jobs")
        && rest.iter().any(|a| a == "--help" || a == "-h")
    {
        help::help(Some(name))
    }
    let code = match name {
        "cache" => match rest.first().map(String::as_str) {
            Some("gc") => cli::cache_gc::cmd_cache_gc(&rest[1..]),
            Some("dump") => match &rest[1..] {
                [file] => cli::cache_dump::cmd_cache_dump(file),
                [] => help::arg_error("cache dump needs an ENTRY file"),
                [_, extra, ..] => help::arg_error(format!("unexpected argument '{extra}'")),
            },
            w => help::unknown_subcommand(w, &["gc", "dump"]),
        },
        _ => on_big_stack(move || match name {
            "build" => cli::build::cmd_build(&rest),
            "diff" => cli::diff::cmd_diff(&rest),
            "eval-jobs" => cli::jobs::cmd_eval_jobs(&rest),
            "print-dev-env" => cli::devenv::cmd_print_dev_env(&rest),
            "plumbing" => cli::plumbing::main(&rest),
            "prefetch" => cli::prefetch::cmd_prefetch(&rest),
            "search" => cli::search::cmd_search(&rest),
            "lint" => cli::cmd_lint(&rest),
            "parse" => cli::parse::main(&rest),
            _ => cli::eval::run_eval(cli::parse_eval_args(&rest)),
        }),
    };
    exit(code)
}

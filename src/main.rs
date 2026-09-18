use std::process::exit;

use ietsp::cli;

const STACK_SIZE: usize = 4 << 30;

/// Rust masks SIGPIPE, so a closed stdout (`ietsp eval-jobs | head`) surfaces
/// as an EPIPE panic. The default disposition makes the process die quietly.
fn restore_sigpipe() {
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
}

fn main() {
    restore_sigpipe();
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = argv.first() else {
        cli::usage()
    };
    match cmd.as_str() {
        "build" | "eval" | "eval-jobs" | "print-dev-env" => {
            let rest: Vec<String> = argv[1..].to_vec();
            let cmd = cmd.clone();
            let code = std::thread::Builder::new()
                .stack_size(STACK_SIZE)
                .spawn(move || match cmd.as_str() {
                    "build" => cli::build::cmd_build(&rest),
                    "eval-jobs" => cli::jobs::cmd_eval_jobs(&rest),
                    "print-dev-env" => cli::devenv::cmd_print_dev_env(&rest),
                    _ => cli::eval::run_eval(cli::parse_eval_args(&rest)),
                })
                .expect("spawn evaluator thread")
                .join()
                .unwrap_or(1);
            exit(code)
        }
        "cache-dump" if argv.len() == 2 => exit(cli::cache_dump::cmd_cache_dump(&argv[1])),
        "parse" if argv.len() > 1 => {
            let rest = &argv[1..];
            let quiet = rest[0] == "-q";
            let files = if quiet { &rest[1..] } else { rest };
            let files: Vec<String> = if files == ["-"] {
                std::io::read_to_string(std::io::stdin())
                    .unwrap_or_default()
                    .lines()
                    .map(str::to_owned)
                    .collect()
            } else {
                files.to_vec()
            };
            exit(cli::parse::cmd_parse(&files, quiet))
        }
        _ => cli::usage(),
    }
}

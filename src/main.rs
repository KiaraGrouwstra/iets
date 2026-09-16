use std::process::ExitCode;
use std::time::Instant;

use ietsp::syntax::ast::FileId;
use ietsp::syntax::symbol::Interner;
use ietsp::syntax::{dump, line_col, parse};

fn usage() -> ExitCode {
    eprintln!("usage: ietsp parse [-q] FILE...   (FILE may be - to read a list from stdin)");
    ExitCode::from(2)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [cmd, rest @ ..] if cmd == "parse" && !rest.is_empty() => {
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
            cmd_parse(&files, quiet)
        }
        _ => usage(),
    }
}

/// With `-q`, parse every file, report errors, and print totals. Used to
/// check and time the parser on whole trees such as nixpkgs.
fn cmd_parse(files: &[String], quiet: bool) -> ExitCode {
    let mut interner = Interner::new();
    let mut errors = 0usize;
    let mut bytes = 0usize;
    let start = Instant::now();
    for (i, file) in files.iter().enumerate() {
        let src = match std::fs::read(file) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("error: cannot read {file}: {e}");
                errors += 1;
                continue;
            }
        };
        bytes += src.len();
        match parse(&src, FileId(i as u32), &mut interner) {
            Ok(ast) => {
                if !quiet {
                    println!("{}", dump::dump(&ast, &interner));
                }
            }
            Err(e) => {
                let (line, col) = line_col(&src, e.span.start);
                eprintln!("error: {}\n       at {file}:{line}:{col}", e.msg);
                errors += 1;
            }
        }
    }
    if quiet {
        let dt = start.elapsed();
        eprintln!(
            "{} files, {} bytes, {} errors, {:.3}s",
            files.len(),
            bytes,
            errors,
            dt.as_secs_f64()
        );
    }
    if errors == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

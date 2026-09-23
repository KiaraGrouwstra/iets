//! `ietsp parse`: the parser run over whole trees.

use crate::syntax::ast::FileId;
use crate::syntax::symbol::Interner;
use crate::syntax::{dump, line_col, parse};

/// `ietsp parse [-q] FILE...`; a lone `-` reads the list of files from stdin.
pub fn main(argv: &[String]) -> i32 {
    let quiet = argv.first().is_some_and(|s| s == "-q");
    let files = &argv[usize::from(quiet)..];
    let files: Vec<String> = match files {
        [] => crate::cli::help::arg_error("parse needs FILE... or - to read them from stdin"),
        [dash] if dash == "-" => std::io::read_to_string(std::io::stdin())
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect(),
        _ => files.to_vec(),
    };
    cmd_parse(&files, quiet)
}

/// With `-q`, parse every file, report errors, and print totals. Used to
/// check and time the parser on whole trees such as nixpkgs.
pub fn cmd_parse(files: &[String], quiet: bool) -> i32 {
    let mut interner = Interner::new();
    let mut errors = 0usize;
    let mut bytes = 0usize;
    let start = std::time::Instant::now();
    for (i, file) in files.iter().enumerate() {
        let src = match std::fs::read(file) {
            Ok(s) => s,
            Err(e) => {
                crate::error::Error::io(format!("cannot read {file}"), e).report();
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
                crate::log::Report {
                    pos: Some(format!("{file}:{line}:{col}")),
                    ..crate::log::Report::new("parse", e.msg)
                }
                .emit();
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
    if errors == 0 { 0 } else { 1 }
}

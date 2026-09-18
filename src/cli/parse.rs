//! `ietsp parse`: the parser run over whole trees.

use crate::syntax::ast::FileId;
use crate::syntax::symbol::Interner;
use crate::syntax::{dump, line_col, parse};

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
    if errors == 0 { 0 } else { 1 }
}

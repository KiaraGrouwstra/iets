//! `ietsp parse`: the parser run over whole trees.

use crate::syntax::ast::FileId;
use crate::syntax::symbol::Interner;
use crate::syntax::{dump, line_col, parse};

/// What `ietsp parse` prints for a file that parses.
#[derive(Clone, Copy, PartialEq)]
pub enum Form {
    /// S-expressions.
    Text,
    /// One `{"file", "root"}` line (docs/reference/syntax-tree.md).
    Json,
    /// Nothing; the totals follow the last file.
    Quiet,
}

/// `ietsp parse [-q | --json] FILE...`; a lone `-` reads the list of files
/// from stdin.
pub fn main(argv: &[String]) -> i32 {
    let form = match argv.first().map(String::as_str) {
        Some("-q") => Form::Quiet,
        Some("--json") => Form::Json,
        _ => Form::Text,
    };
    let files = &argv[usize::from(form != Form::Text)..];
    let files: Vec<String> = match files {
        [] => crate::cli::help::arg_error("parse needs FILE... or - to read them from stdin"),
        [dash] if dash == "-" => std::io::read_to_string(std::io::stdin())
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect(),
        _ => files.to_vec(),
    };
    cmd_parse(&files, form)
}

/// With `-q`, parse every file, report errors, and print totals. Used to
/// check and time the parser on whole trees such as nixpkgs.
pub fn cmd_parse(files: &[String], form: Form) -> i32 {
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
            Ok(ast) => match form {
                Form::Text => crate::cli::render::bytes(
                    format!("{}\n", dump::dump(&ast, &interner)).as_bytes(),
                ),
                Form::Json => {
                    let mut line = format!("{{\"file\":{},\"root\":", serde_json::json!(file));
                    dump::json(&dump::tree(&ast, &interner), &mut line);
                    crate::cli::render::bytes(format!("{line}}}\n").as_bytes());
                }
                Form::Quiet => {}
            },
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
    if form == Form::Quiet {
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

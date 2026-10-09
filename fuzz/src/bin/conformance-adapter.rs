//! iets behind the adapter protocol of nix-conformance (its README.md):
//! `conformance-adapter OP FILE [FLAG...]`. `parse` and `nar` call the
//! library; `check` and `eval` run `$IETS`, else `iets` on `PATH`.

use iets::store::nar;
use iets::syntax::{self, ast::FileId, symbol::Interner};
use iets_fuzz::{canon, sx};
use sha2::{Digest, Sha256};
use std::path::{self, Path};
use std::process::{Command, exit};
use std::{env, fs};

/// "This implementation does not do that": the runner skips the case.
const UNSUPPORTED: i32 = 3;

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let (op, file, flags) = match args.as_slice() {
        [op, file, flags @ ..] => (op.as_str(), file.as_str(), flags),
        _ => {
            eprintln!("usage: conformance-adapter OP FILE [FLAG...]");
            exit(2);
        }
    };
    exit(match op {
        "parse" => parse(file),
        "nar" => read_nar(file),
        // `import` resolves every variable of the file; a refusal is an error
        "check" => iets(&[
            "eval",
            "--impure",
            "-E",
            &format!("builtins.seq (import {}) null", absolute(file)),
        ]),
        "eval" => {
            let mut a = vec!["eval", "--impure", "--strict"];
            a.extend(flags.iter().map(String::as_str));
            a.push(file);
            iets(&a)
        }
        _ => UNSUPPORTED,
    })
}

fn parse(file: &str) -> i32 {
    let Ok(src) = fs::read(file) else {
        return 2;
    };
    let mut interner = Interner::new();
    match syntax::parse(&src, FileId(0), &mut interner) {
        Ok(ast) => {
            let tree = sx::normalise(canon::tree(&ast, &interner));
            println!("ok {}", sx::print(&tree));
        }
        Err(e) => {
            let (line, col) = syntax::line_col(&src, e.span.start);
            println!("error {line}:{col}");
        }
    }
    0
}

fn read_nar(file: &str) -> i32 {
    let Ok(input) = fs::read(file) else {
        return 2;
    };
    let dir = env::temp_dir().join(format!("iets-conformance-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    let out = dir.join("t");
    let answer = (|| {
        fs::create_dir_all(&dir).ok()?;
        let consumed = nar::parse_length(&mut &input[..]).ok()?;
        nar::restore_path(&mut &input[..], &out).ok()?;
        let mut dump = Vec::new();
        nar::dump_path(&out, &mut dump, &mut |_, _| true).ok()?;
        let hash: String = Sha256::digest(&dump)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        Some(format!("ok {consumed} {hash}"))
    })();
    let _ = fs::remove_dir_all(&dir);
    println!("{}", answer.unwrap_or_else(|| "error".into()));
    0
}

fn absolute(file: &str) -> String {
    path::absolute(Path::new(file))
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| file.to_string())
}

fn iets(args: &[&str]) -> i32 {
    let bin = env::var("IETS").unwrap_or_else(|_| "iets".into());
    match Command::new(&bin).args(args).status() {
        Ok(s) => s.code().unwrap_or(1),
        Err(e) => {
            eprintln!("conformance-adapter: cannot run {bin}: {e}");
            2
        }
    }
}

use std::collections::HashSet;
use std::process::exit;

use ietsp::eval::{Error, Evaluator, Pos};
use ietsp::runtime::Word;
use ietsp::syntax::ast::FileId;
use ietsp::syntax::symbol::Interner;
use ietsp::syntax::{dump, line_col, parse};

const STACK_SIZE: usize = 4 << 30;

fn usage() -> ! {
    eprintln!(
        "usage: ietsp eval [--strict] [--json] [-I prefix=path]... [--arg name expr] [--argstr name str] (-E EXPR | FILE)\n       ietsp parse [-q] FILE...   (FILE may be - to read a list from stdin)"
    );
    exit(2)
}

/// With `-q`, parse every file, report errors, and print totals. Used to
/// check and time the parser on whole trees such as nixpkgs.
fn cmd_parse(files: &[String], quiet: bool) -> i32 {
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

struct EvalArgs {
    strict: bool,
    json: bool,
    includes: Vec<(Vec<u8>, Vec<u8>)>,
    args: Vec<(String, ArgKind)>,
    input: Option<Input>,
    lint_absolute: ietsp::compile::Lint,
    lint_short: ietsp::compile::Lint,
}

enum ArgKind {
    Expr(String),
    Str(String),
}

enum Input {
    Expr(String),
    File(String),
}

fn parse_include(s: &str) -> (Vec<u8>, Vec<u8>) {
    let (prefix, path) = match s.split_once('=') {
        Some((p, v)) => (p.as_bytes().to_vec(), v),
        None => (Vec::new(), s),
    };
    let abs = if path.starts_with('/') {
        path.as_bytes().to_vec()
    } else {
        let mut cwd = std::env::current_dir()
            .map(|p| p.into_os_string().into_encoded_bytes())
            .unwrap_or_else(|_| b"/".to_vec());
        cwd.push(b'/');
        cwd.extend_from_slice(path.as_bytes());
        ietsp::compile::canon_path(&cwd)
    };
    (prefix, abs)
}

fn parse_eval_args(argv: &[String]) -> EvalArgs {
    let mut a = EvalArgs {
        strict: false,
        json: false,
        includes: Vec::new(),
        args: Vec::new(),
        input: None,
        lint_absolute: ietsp::compile::Lint::Ignore,
        lint_short: ietsp::compile::Lint::Ignore,
    };
    let mut i = 0;
    while i < argv.len() {
        let s = argv[i].as_str();
        match s {
            "--strict" => a.strict = true,
            "--json" => a.json = true,
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
            "--option" => {
                // accepted for nix-instantiate compatibility; ignored
                i += 2;
            }
            "--lint-absolute-path-literals"
            | "--lint-short-path-literals"
            | "--lint-url-literals" => {
                i += 1;
                let v = argv.get(i).unwrap_or_else(|| usage());
                let Some(mode) = ietsp::compile::Lint::parse(v) else {
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
            _ if s.starts_with("-I") && s.len() > 2 => a.includes.push(parse_include(&s[2..])),
            _ if s.starts_with('-') && s != "-" => {
                eprintln!("error: unknown flag '{s}'");
                usage()
            }
            _ => a.input = Some(Input::File(s.to_string())),
        }
        i += 1;
    }
    a
}

fn report(ev: &Evaluator, e: &Error) {
    eprintln!("error: {}", e.msg);
    if !e.pos.is_none() {
        eprintln!("       at {}", ev.describe_pos(e.pos));
    }
    for (pos, msg) in e.trace.iter() {
        if pos.is_none() {
            eprintln!("       … {msg}");
        } else {
            eprintln!("       … {msg} at {}", ev.describe_pos(*pos));
        }
    }
}

fn run_eval(a: EvalArgs) -> i32 {
    let mut ev = Evaluator::new();
    ev.compile_settings.lint_absolute_path_literals = a.lint_absolute;
    ev.compile_settings.lint_short_path_literals = a.lint_short;
    let mut includes = a.includes;
    if let Ok(np) = std::env::var("NIX_PATH") {
        for entry in np.split(':').filter(|s| !s.is_empty()) {
            includes.push(parse_include(entry));
        }
    }
    ietsp::builtins::set_search_path(&mut ev, includes);

    let cwd = std::env::current_dir()
        .map(|p| p.into_os_string().into_encoded_bytes())
        .unwrap_or_else(|_| b"/".to_vec());

    let result = (|| -> Result<Word, Box<Error>> {
        let mut v = match a.input {
            Some(Input::Expr(e)) => ev.eval_string(&e, &cwd)?,
            Some(Input::File(f)) => {
                let mut p = f.as_bytes().to_vec();
                if !p.starts_with(b"/") {
                    let mut full = cwd.clone();
                    full.push(b'/');
                    full.extend_from_slice(&p);
                    p = ietsp::compile::canon_path(&full);
                }
                ev.eval_file(&p, Pos::NONE)?
            }
            None => usage(),
        };
        if !a.args.is_empty() {
            let mut entries = Vec::new();
            for (name, kind) in &a.args {
                let w = match kind {
                    ArgKind::Expr(e) => {
                        let ir = ev.compile_source(
                            b"\xc2\xabarg\xc2\xbb",
                            e.as_bytes().to_vec(),
                            &cwd,
                            None,
                        )?;
                        let root = ev.root_env;
                        ev.arena.mk_thunk(root, ir)
                    }
                    ArgKind::Str(s) => ev.mk_string(s.as_bytes()),
                };
                let sym = ev.sym(name.as_bytes());
                entries.push((sym, Pos::NONE, w));
            }
            let args = ev.mk_attrs(&mut entries, Pos::NONE)?;
            v = ev.auto_call(v, args)?;
        }
        Ok(v)
    })();

    let v = match result {
        Ok(v) => v,
        Err(e) => {
            report(&ev, &e);
            return 1;
        }
    };

    let mut out: Vec<u8> = Vec::new();
    let r = if a.json {
        let mut ctx = Vec::new();
        ietsp::print::print_json(&mut ev, v, &mut out, &mut ctx, a.strict, Pos::NONE)
    } else {
        (|| {
            let v = if a.strict {
                ev.force_deep(v)?
            } else {
                ev.force(v)?
            };
            let mut seen = HashSet::new();
            ietsp::print::print_ambiguous(&mut ev, v, 1, &mut out, &mut seen, 0)
        })()
    };
    match r {
        Ok(()) => {
            out.push(b'\n');
            use std::io::Write;
            let _ = std::io::stdout().write_all(&out);
            let _ = std::io::stdout().flush();
            0
        }
        Err(e) => {
            report(&ev, &e);
            1
        }
    }
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = argv.first() else { usage() };
    match cmd.as_str() {
        "eval" => {
            let a = parse_eval_args(&argv[1..]);
            let code = std::thread::Builder::new()
                .stack_size(STACK_SIZE)
                .spawn(move || run_eval(a))
                .expect("spawn evaluator thread")
                .join()
                .unwrap_or(1);
            exit(code)
        }
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
            exit(cmd_parse(&files, quiet))
        }
        _ => usage(),
    }
}

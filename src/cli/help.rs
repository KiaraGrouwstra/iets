//! `ietsp --help`, `ietsp COMMAND --help`, `--version`, and the one way a
//! native command reports a bad command line.

use std::process::exit;
use std::sync::RwLock;

pub const OVERVIEW: &str = "\
usage: ietsp COMMAND [ARGS...]

A Nix evaluator, and a client that builds what it evaluates.

commands:
  eval           evaluate an expression or a file and print the value
  build          evaluate to derivations and build them on a daemon
  eval-jobs      print a jobset's derivations as JSON lines (nix-eval-jobs)
  fast-build     evaluate and build a flake's checks (nix-fast-build)
  print-dev-env  print the environment of a flake's devShell, for direnv
  lint           list the lints and the levels a run would use
  parse          parse Nix files and print or check the syntax tree
  cache          sweep old cache entries, or dump a cache entry's read set
  help, version

classic: 'ietsp NAME ARGS...' with NAME one of nix-instantiate, nix-build,
  nix-shell, nix-eval-jobs, nix-fast-build or nix acts like that Nix command,
  and falls back to the real one for the rest (docs/classic.md).

Every command also takes --ietsp-option NAME VALUE (docs/settings.md).
Settings: docs/config.md. Everything else: docs/*.md.
Run 'ietsp COMMAND --help' for the flags of a command.
";

const INPUT: &str = "
common:
  -E, --expr EXPR       evaluate EXPR instead of FILE
  -A, --attr PATH       select the attribute PATH of the value
  --arg N EXPR, --argstr N STR  call a top-level function with N = EXPR/\"STR\"
  -I, --include P=PATH  make <P> resolve to PATH
  --impure, --pure-eval  turn pure evaluation off or on (docs/pure-eval.md)
  --workers N           evaluator threads (default: the cores, at most 4)
  --timings             report each phase and the cache on stderr
  --no-cache, --no-ir-cache  do not use the tracing or the compiled-file cache
  --cache-dir DIR       keep the tracing cache in DIR (docs/tracing-cache.md)
";

const EVAL: &str = "\
usage: ietsp eval [FLAGS] (-E EXPR | FILE | FLAKEREF#attr)

Evaluate an expression, a file or a locked flake and print the value.

  --strict              evaluate the whole value, not only the top level
  --json                print the value as JSON
  --raw                 print a string as it is, without quotes or newline
  --dump-drvs DIR       write the ATerm of each derivation to DIR
  --lint NAME=LEVEL     set a lint to off, warn or error (docs/lints.md)
  --lint-root DIR       report lint sites under DIR too
  --lint-scope root|all  report sites in the lint roots only, or everywhere
";

const BUILD: &str = "\
usage: ietsp build [FLAGS] (-E EXPR | FILE)
       ietsp build [FLAGS] [--source STOREPATH=LOCAL]... DRV

Evaluate to derivations, send the closure the builder lacks from memory,
build, and print the output paths (docs/remote-build.md).

  --builder URL         daemon, unix:///path, ssh-ng://[user@]host or
                        grpc://host:port (default: the builder setting)
  --no-build            upload the closure, do not build
  --out NAME            build the output NAME only (repeatable)
  --fetch-to DIR        copy the outputs from the builder to DIR
  --source SP=LOCAL     with DRV: the local tree of the input source SP
";

const EVAL_JOBS: &str = "\
usage: ietsp eval-jobs [FLAGS] (-E EXPR | FILE | --flake FLAKEREF[#attr])

Walk a jobset and print one JSON line per derivation, like nix-eval-jobs
(docs/eval-jobs.md).

  -E, --expr EXPR       evaluate EXPR instead of FILE
  --flake               the argument is a flake reference
  --arg N EXPR, --argstr N STR  call the top level with N = EXPR/\"STR\"
  --arg-from-file N F, --arg-from-stdin N  --argstr N from F or stdin
  -I, --include P=PATH  make <P> resolve to PATH
  --impure              turn off the pure evaluation --flake implies
  --workers N           worker processes (default 4)
  --max-memory-size MB  restart a worker above MB (4096; 0: one process)
  --force-recurse       walk every attribute set, not only marked ones
  --meta, --show-input-drvs  add each job's meta, inputDrvs
  --provenance          add each derivation's source position as pos
  --no-instantiate      do not write the .drv files
  --eval-store URL      the daemon that gets the .drv files (default daemon)
  --gc-roots-dir DIR    add a GC root for each job in DIR
  --check-cache-status  add whether each job is built or substitutable
  --constituents        resolve Hydra aggregate constituents
  --apply EXPR          add the result of EXPR applied to each job
  --select EXPR         walk EXPR applied to the root instead
  --chunks M, --chunk N  evaluate part N (0 to M-1) of M parts of the tree
  --chunk-depth D       split the tree at depth D (default 2)
  --quiet               no summary line on stderr
";

const FAST_BUILD: &str = "\
usage: ietsp fast-build [FLAGS]

Evaluate and build an attribute tree in one process, with nix-fast-build's
flags and result files (docs/fast-build.md).

  -f, --flake REF       the flake attribute to build (default .#checks)
  --file [FILE], -A PATH  build a file (default.nix), or its attribute PATH
  --arg N EXPR, --argstr N STR, -I P=PATH  as for 'ietsp eval'
  --pure, --impure      turn pure evaluation on or off (--file: impure)
  --option NAME VALUE   set a Nix setting
  --select EXPR         build EXPR applied to the tree instead
  --store URL           where to build (default: the builder setting)
  -j, --max-jobs N      builds at the same time
  --eval-workers N      evaluator threads
  --systems \"A B\"       build for these systems (default: this one)
  --skip-cached         do not build what is built or substitutable
  --skip-unsupported    skip jobs not available on their system
  --out-link PREFIX     link each output as PREFIX-ATTR
  --retries N           build a failed job again, N times
  --fail-fast           stop at the first failure
  --result-file PATH, --result-format json|junit  write the results
  --stream-json-lines   print each result as a JSON line
  --no-fold             no GitHub Actions log groups
";

const PRINT_DEV_ENV: &str = "\
usage: ietsp print-dev-env [FLAGS] [FLAKEREF[#attr]]

Print the bash that 'nix print-dev-env' prints for a flake's devShell
(default: .), for direnv (docs/direnv.md).

  --builder URL         the Nix daemon: daemon (default), unix:///path or
                        ssh-ng://[user@]host
  --gc-root PATH        link PATH to the environment and keep it as a GC root
  --json                print what 'nix print-dev-env --json' prints
  --workers N, --timings, --no-cache, --cache-dir DIR  as for 'ietsp eval'
";

const LINT: &str = "\
usage: ietsp lint list [FLAGS] [PATH]

Print the lint registry with the levels a run from PATH (default: the
current directory) uses, then the scope and its roots (docs/lints.md).

  --lint NAME=LEVEL     set a lint to off, warn or error
  --lint-root DIR       add a lint root
  --lint-scope root|all  report sites in the lint roots only, or everywhere
";

const PARSE: &str = "\
usage: ietsp parse [-q] FILE...

Parse each FILE and print its syntax tree; FILE - reads the list from stdin.

  -q                    print only the parse errors and the totals
";

const CACHE: &str = "\
usage: ietsp cache gc [--ttl DAYS] [--dry-run] [--cache-dir DIR]
       ietsp cache dump READSET

gc removes the cache entries nobody read for DAYS days; dump prints the
read set of a tracing-cache entry (docs/tracing-cache.md).

  --ttl DAYS            the age limit in days
  -n, --dry-run         count what gc would remove, remove nothing
  --cache-dir DIR       sweep DIR instead of the default cache
";

pub const COMMANDS: &str = "eval build eval-jobs fast-build print-dev-env lint parse cache";

/// The native command being parsed, "ietsp" before the command word; unset
/// under a classic name.
static COMMAND: RwLock<Option<&'static str>> = RwLock::new(None);

pub fn set_command(cmd: &'static str) {
    *COMMAND.write().unwrap() = Some(cmd);
}

pub fn command() -> Option<&'static str> {
    *COMMAND.read().unwrap()
}

/// The help of `cmd` in two parts, `None` for the overview.
fn parts(cmd: Option<&str>) -> Option<[&'static str; 2]> {
    Some(match cmd? {
        "eval" => [EVAL, INPUT],
        "build" => [BUILD, INPUT],
        "eval-jobs" => [EVAL_JOBS, ""],
        "fast-build" => [FAST_BUILD, ""],
        "print-dev-env" => [PRINT_DEV_ENV, ""],
        "lint" => [LINT, ""],
        "parse" => [PARSE, ""],
        "cache" | "cache-dump" => [CACHE, ""],
        _ => return None,
    })
}

pub fn help(cmd: Option<&str>) -> ! {
    match (cmd, parts(cmd)) {
        (None, _) => print!("{OVERVIEW}"),
        (Some(_), Some([own, input])) => print!("{own}{input}"),
        (Some(c), None) => unknown_command(c),
    }
    exit(0)
}

/// The flags the help of `cmd` lists: the words before the description.
fn flags(cmd: &str) -> Vec<&'static str> {
    let Some(parts) = parts(Some(cmd)) else {
        return vec!["--help", "-h", "--version"];
    };
    let lines = parts.into_iter().flat_map(str::lines);
    let heads = lines.filter_map(|l| l.strip_prefix("  -").map(|_| l[2..].split("  ").next()));
    heads
        .flatten()
        .flat_map(|h| h.split([' ', ',']).filter(|w| w.starts_with('-')))
        .collect()
}

fn distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut diag = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let sub = diag + usize::from(ca != *cb);
            diag = row[j + 1];
            row[j + 1] = sub.min(row[j] + 1).min(diag + 1);
        }
    }
    row[b.len()]
}

/// The closest candidate, when it is close enough to be a typo.
pub fn suggest<'a>(word: &str, candidates: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    let close = candidates.into_iter().map(|c| (distance(word, c), c));
    close
        .filter(|(d, c)| d * 3 <= c.len())
        .min_by_key(|(d, _)| *d)
        .map(|(_, c)| c)
}

/// What a bad command line prints under `cmd` ("ietsp" before the command
/// word, `None` under a classic name), without the exit.
pub fn error_text(cmd: Option<&str>, msg: &str, maybe: Option<&str>) -> String {
    let maybe = maybe.map(|m| format!("did you mean '{m}'?\n"));
    let hint = match cmd {
        Some("ietsp") => "run 'ietsp --help' for the commands\n".to_string(),
        Some(c) => format!("run 'ietsp {c} --help' for the flags\n"),
        None => String::new(),
    };
    format!("error: {msg}\n{}{hint}", maybe.unwrap_or_default())
}

fn fail(msg: &str, maybe: Option<&str>) -> ! {
    eprint!("{}", error_text(command(), msg, maybe));
    exit(2)
}

pub fn arg_error(msg: impl AsRef<str>) -> ! {
    fail(msg.as_ref(), None)
}

pub fn unknown_flag(flag: &str) -> ! {
    let maybe = command().and_then(|c| suggest(flag, flags(c)));
    fail(&format!("unknown flag '{flag}'"), maybe)
}

pub fn unknown_command(word: &str) -> ! {
    fail(
        &format!("unknown command '{word}'"),
        suggest(word, COMMANDS.split(' ')),
    )
}

/// `word` is not one of `subs`, the subcommands of this command.
pub fn unknown_subcommand(word: Option<&str>, subs: &[&'static str]) -> ! {
    match word {
        None => arg_error(format!("missing subcommand: {}", subs.join(" or "))),
        Some(w) => fail(
            &format!("unknown subcommand '{w}'"),
            suggest(w, subs.iter().copied()),
        ),
    }
}

/// `argv[i]`, the value of `flag`.
pub fn value<'a>(argv: &'a [String], i: usize, flag: &str) -> &'a String {
    let expr = matches!(flag, "-E" | "--expr");
    let wants = if expr { "an expression" } else { "a value" };
    argv.get(i)
        .unwrap_or_else(|| arg_error(format!("{flag} needs {wants}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn help_fits_80_columns() {
        let all = COMMANDS.split(' ').filter_map(|c| parts(Some(c))).flatten();
        for l in all.chain([OVERVIEW]).flat_map(str::lines) {
            assert!(l.chars().count() <= 80, "{l}");
        }
    }

    #[test]
    fn suggestions_come_from_the_help() {
        for (cmd, typo, want) in [
            ("eval", "--stirct", Some("--strict")),
            ("build", "--no-ir-cach", Some("--no-ir-cache")),
            ("eval-jobs", "--chunk-dept", Some("--chunk-depth")),
            ("fast-build", "--result-fromat", Some("--result-format")),
            ("fast-build", "\"A", None),
            ("eval", "-x", None),
        ] {
            assert_eq!(suggest(typo, flags(cmd)), want, "{typo}");
        }
        assert_eq!(suggest("evl", COMMANDS.split(' ')), Some("eval"));
        assert_eq!(suggest("bogus", COMMANDS.split(' ')), None);
    }

    #[test]
    fn messages() {
        let t = error_text(Some("eval"), "unknown flag '--stirct'", Some("--strict"));
        let want = "did you mean '--strict'?\nrun 'ietsp eval --help' for the flags\n";
        assert_eq!(t, format!("error: unknown flag '--stirct'\n{want}"));
        assert_eq!(error_text(None, "x", None), "error: x\n");
    }
}

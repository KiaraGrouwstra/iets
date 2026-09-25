//! `nix derivation show` (src/nix/derivation-show.cc, Nix 2.34.8): the JSON of
//! the store derivations the installables evaluate to, and with `-r` of their
//! whole input-derivation closure. The document is
//! `crate::api::drvshow`'s; this file is the command line and the printing.
//! See `docs/reference/classic.md`.

use std::sync::Arc;

use super::nix::{Nix, global_flag, nix_settings, resolve};
use super::{Argv, fallback};
use crate::api::drvshow;
use crate::api::installable;
use crate::cli::{Outcome, cwd, render};

const NAME: &str = "nix";

fn parse(argv: &[String]) -> Result<Nix, String> {
    let mut n = Nix::default();
    let mut a = Argv { all: argv, i: 0 };
    while a.i < argv.len() {
        let s = argv[a.i].clone();
        match s.as_str() {
            "-r" | "--recursive" => n.recursive = true,
            "--pretty" => n.pretty = Some(true),
            "--no-pretty" => n.pretty = Some(false),
            "--expr" => n.expr = Some(a.value(&s)?),
            "-f" | "--file" => n.file = Some(a.value(&s)?),
            _ => {
                if !global_flag(&mut n.c, &mut a, &s)? {
                    if s.starts_with('-') {
                        return Err(format!("'{s}' is not implemented"));
                    }
                    n.installables.push(s);
                }
            }
        }
        a.i += 1;
    }
    if n.expr.is_some() && n.file.is_some() {
        return Err("'--file' and '--expr' are exclusive".into());
    }
    // installables.cc: an InstallablesCommand with no argument takes `.`.
    if n.installables.is_empty() {
        n.installables.push(".".into());
    }
    n.check_args_with_flake()?;
    Ok(n)
}

/// Runs `nix derivation show`. `argv` is everything after argv[0], for the
/// fallback; `rest` is everything after the subcommand.
pub fn main(argv: &[String], rest: &[String]) -> i32 {
    let mut n = match parse(rest) {
        Ok(n) => n,
        Err(reason) => fallback(NAME, argv, &reason),
    };
    let (cfg, _, _) = match nix_settings(&mut n.c, n.file.is_some(), true) {
        Ok(t) => t,
        Err(reason) => fallback(NAME, argv, &reason),
    };
    let pretty = n.pretty.unwrap_or_else(render::pretty);
    let ev = super::evaluator(&n.c, &cfg);
    let n = Arc::new(n);
    let run = Arc::clone(&n);
    match crate::cli::run_reported(crate::cli::Run::on(ev), move |ev, _| {
        let cwd = cwd();
        let mut roots = Vec::new();
        for what in &run.installables {
            let v = resolve(ev, &run, what, &cwd)?;
            roots.push(installable::to_derivation(ev, v, what)?);
        }
        drvshow::show(ev, &roots, run.recursive)
    }) {
        Outcome::Failed(code) => code,
        Outcome::Unsupported(reason) => fallback(NAME, argv, &reason),
        Outcome::Ok(doc) => {
            render::bytes(&render::json(&doc, pretty));
            0
        }
    }
}

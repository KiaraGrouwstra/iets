//! `nix-instantiate` (nix-instantiate.cc). Evaluates, registers the derivation
//! closure through the daemon and prints the drv paths, or evaluates and prints
//! the value with `--eval`.

use std::collections::HashSet;
use std::sync::Arc;

use super::{Argv, Common, Root, common_flag, fallback, is_unsupported};
use crate::build::backend::Worker;
use crate::build::upload::{PlanUpload, Uploader};
use crate::cli::{abs_path, auto_args, cwd, report};
use crate::eval::{Error, Evaluator, Pos};
use crate::remote::plan::Plan;
use crate::remote::worker::{Options, Transport};
use crate::runtime::Word;

const NAME: &str = "nix-instantiate";

#[derive(Default, PartialEq, Eq, Clone, Copy)]
enum Output {
    #[default]
    Plain,
    Raw,
    Json,
}

#[derive(Default)]
struct Inst {
    c: Common,
    eval_only: bool,
    strict: bool,
    output: Output,
    add_root: Option<String>,
    read_write: bool,
    readonly: bool,
    gc_warning: bool,
}

fn parse(argv: &[String]) -> Result<Inst, String> {
    let mut i = Inst {
        gc_warning: true,
        ..Inst::default()
    };
    let mut a = Argv { all: argv, i: 0 };
    while a.i < argv.len() {
        let s = argv[a.i].clone();
        match s.as_str() {
            "--eval" | "--eval-only" => i.eval_only = true,
            "--read-write-mode" => i.read_write = true,
            "--readonly-mode" | "--dry-run" => i.readonly = true,
            "--strict" => i.strict = true,
            "--json" => i.output = Output::Json,
            "--raw" => i.output = Output::Raw,
            "--add-root" => i.add_root = Some(a.value("--add-root")?),
            "--no-gc-warning" => i.gc_warning = false,
            "-Q" | "--no-build-output" => i.c.quiet = true,
            "--no-location" => {}
            "-" => return Err("reading the expression from stdin is not implemented".into()),
            "--parse" | "--parse-only" | "--xml" | "--find-file" => {
                return Err(format!("'{s}' is not implemented"));
            }
            _ => {
                if !common_flag(&mut i.c, &mut a, &s)? {
                    if s.starts_with('-') {
                        return Err(format!("'{s}' is not implemented"));
                    }
                    i.c.files.push(s);
                }
            }
        }
        a.i += 1;
    }
    if i.eval_only && !i.read_write {
        i.readonly = true;
    }
    if i.c.files.is_empty() && !i.c.from_args {
        i.c.files.push("./default.nix".into());
    }
    if i.c.flags.attrs.is_empty() {
        i.c.flags.attrs.push(String::new());
    }
    if i.add_root.is_some() && i.readonly {
        return Err("'--add-root' with a read-only store is not implemented".into());
    }
    Ok(i)
}

enum Run {
    Failed(i32),
    Unsupported(String),
    /// `--eval`: the whole stdout, already formatted.
    Values(Vec<u8>),
    /// The default mode: the roots to register and print, and their closure.
    Drvs(Vec<Root>, Option<Box<Plan>>),
}

pub fn main(argv: &[String]) -> i32 {
    let i = match parse(argv) {
        Ok(i) => i,
        Err(reason) => fallback(NAME, argv, &reason),
    };
    // `nix-instantiate` builds nothing: the drvs, their sources and the
    // `--add-root` links belong to the local store whatever `builder` says.
    let (cfg, _, options) = match super::settings(&i.c) {
        Ok(t) => t,
        Err(reason) => fallback(NAME, argv, &reason),
    };
    let transport = super::local();
    let ev = super::evaluator(&i.c, &cfg);
    let i = Arc::new(i);
    let inst = Arc::clone(&i);
    match crate::parallel::run(crate::parallel::default_workers(), ev, move |ev| {
        run(ev, &inst)
    }) {
        Run::Failed(code) => code,
        Run::Unsupported(reason) => fallback(NAME, argv, &reason),
        Run::Values(text) => {
            print_bytes(&text);
            0
        }
        Run::Drvs(roots, plan) => finish(&i, &transport, &options, &roots, plan.as_deref()),
    }
}

fn print_bytes(b: &[u8]) {
    use std::io::Write;
    let mut out = std::io::stdout();
    let _ = out.write_all(b);
    let _ = out.flush();
}

fn run(ev: &mut Evaluator, i: &Inst) -> Run {
    let cwd = cwd();
    let mut text: Vec<u8> = Vec::new();
    let mut roots: Vec<Root> = Vec::new();
    let mut seen = HashSet::new();
    let r = (|| -> Result<(), Box<Error>> {
        let args = auto_args(ev, &i.c.flags.args, &cwd)?;
        let has_args = !i.c.flags.args.is_empty();
        for file in &i.c.files {
            let root = super::eval_input(ev, &i.c, file, &cwd)?;
            for attr in &i.c.flags.attrs {
                let v = super::select(ev, root, attr, args)?;
                if i.eval_only {
                    let v = if has_args { ev.auto_call(v, args)? } else { v };
                    print_value(ev, v, i, &mut text)?;
                } else {
                    super::derivations_at(ev, v, args, &mut roots, &mut seen, attr)?;
                }
            }
        }
        Ok(())
    })();
    if let Err(e) = r {
        if is_unsupported(&e) {
            return Run::Unsupported(e.msg.clone());
        }
        return Run::Failed(report(ev, &e));
    }
    if i.eval_only {
        return Run::Values(text);
    }
    if i.readonly || roots.is_empty() {
        return Run::Drvs(roots, None);
    }
    match super::plan_roots(ev, &roots) {
        Ok(p) => Run::Drvs(roots, Some(Box::new(p))),
        Err(e) => Run::Failed(e.report()),
    }
}

fn print_value(ev: &mut Evaluator, v: Word, i: &Inst, out: &mut Vec<u8>) -> Result<(), Box<Error>> {
    match i.output {
        Output::Raw => {
            let mut ctx = Vec::new();
            let s = ev.coerce_to_string(
                v,
                &mut ctx,
                Pos::NONE,
                "while generating the nix-instantiate output",
                false,
                false,
                true,
            )?;
            out.extend_from_slice(&s);
        }
        Output::Json => {
            let mut ctx = Vec::new();
            crate::print::print_json(ev, v, out, &mut ctx, i.strict, Pos::NONE)?;
            out.push(b'\n');
        }
        Output::Plain => {
            let v = if i.strict {
                ev.force_deep(v)?
            } else {
                ev.force(v)?
            };
            let mut seen = HashSet::new();
            crate::print::print_ambiguous(ev, v, 1, out, &mut seen, 0)?;
            out.push(b'\n');
        }
    }
    Ok(())
}

/// Registers the closure, makes the `--add-root` links, prints the drv paths.
fn finish(
    i: &Inst,
    transport: &Transport,
    options: &Options,
    roots: &[Root],
    plan: Option<&Plan>,
) -> i32 {
    if i.add_root.is_none() && i.gc_warning && !roots.is_empty() {
        crate::log::line(
            "warning: you did not specify '--add-root'; the result might be removed by the garbage collector",
        );
    }
    let mut worker = None;
    if let Some(plan) = plan {
        let r = Worker::open(transport, options, &mut log_event).and_then(|mut w| {
            PlanUpload::new(plan, &super::locate_store).upload(
                &mut w,
                std::slice::from_ref(&roots[0].drv),
                &mut log_event,
            )?;
            Ok(w)
        });
        match r {
            Ok(w) => worker = Some(w),
            Err(e) => return e.report(),
        }
    }
    let cwd = cwd();
    let mut out = String::new();
    let mut root_nr = 0;
    for r in roots {
        let shown = match (&i.add_root, worker.as_mut()) {
            (Some(g), Some(w)) => {
                root_nr += 1;
                let mut name = String::from_utf8_lossy(&abs_path(g, &cwd)).into_owned();
                if root_nr > 1 {
                    name.push_str(&format!("-{root_nr}"));
                }
                if let Err(e) = w
                    .conn
                    .add_perm_root(&r.drv.to_string(), std::path::Path::new(&name))
                {
                    return e.report();
                }
                name
            }
            _ => r.drv.to_string(),
        };
        if r.output == "out" {
            out.push_str(&shown);
        } else {
            out.push_str(&format!("{shown}!{}", r.output));
        }
        out.push('\n');
    }
    print_bytes(out.as_bytes());
    0
}

fn log_event(e: crate::remote::event::Event) {
    if let crate::remote::event::Event::Log(l) = e {
        crate::log::line(&String::from_utf8_lossy(&l.line));
    }
}

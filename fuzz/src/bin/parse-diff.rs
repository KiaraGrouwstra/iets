//! Replays files through both parsers: a corpus, a crash artifact, or a
//! tree of `.nix` files. Exit 0: no unexpected divergence; 1: at least one;
//! 2: the run itself failed.

use iets_fuzz::{Checker, Verdict, known};
use serde_json::json;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

const USAGE: &str = "usage: parse-diff [--json] [--nix-only] PATH...

Parses each file under PATH with iets and with Nix 2.34.8 and compares.
A directory is walked; with --nix-only only its *.nix files are read.
--json prints one JSON object per divergence and a summary object.";

fn main() {
    std::process::exit(run());
}

fn run() -> i32 {
    let mut json = false;
    let mut nix_only = false;
    let mut roots = Vec::new();
    for a in std::env::args().skip(1) {
        match a.as_str() {
            "--json" => json = true,
            "--nix-only" => nix_only = true,
            "-h" | "--help" => {
                println!("{USAGE}");
                return 0;
            }
            _ if a.starts_with('-') => {
                eprintln!("parse-diff: unknown flag {a}\n{USAGE}");
                return 2;
            }
            _ => roots.push(PathBuf::from(a)),
        }
    }
    if roots.is_empty() {
        eprintln!("{USAGE}");
        return 2;
    }
    let known = match known::from_env() {
        Ok(k) => k,
        Err(e) => {
            eprintln!("parse-diff: {e}");
            return 2;
        }
    };
    let mut files = Vec::new();
    for r in &roots {
        walk(r, nix_only, &mut files);
    }
    let mut checker = Checker::new(known);
    let (mut same, mut bugs) = (0usize, 0usize);
    let mut expected: BTreeMap<String, usize> = BTreeMap::new();
    let start = Instant::now();
    for f in &files {
        let Ok(src) = std::fs::read(f) else { continue };
        match checker.check(&src) {
            Verdict::Same => same += 1,
            Verdict::Expected(id) => *expected.entry(id).or_default() += 1,
            Verdict::Bug(d) => {
                bugs += 1;
                if json {
                    println!(
                        "{}",
                        json!({"file": f, "kind": d.kind.name(), "at": d.at, "nix": d.nix, "iets": d.iets})
                    );
                } else {
                    println!(
                        "{}: {}{}",
                        f.display(),
                        d.kind.name(),
                        if d.at.is_empty() {
                            String::new()
                        } else {
                            format!(" at {}", d.at)
                        }
                    );
                    println!("  nix:  {}", clip(&d.nix));
                    println!("  iets: {}", clip(&d.iets));
                }
            }
        }
    }
    let secs = start.elapsed().as_secs_f64();
    let total = files.len();
    if json {
        println!(
            "{}",
            json!({"summary": {"inputs": total, "same": same, "expected": expected, "bugs": bugs, "seconds": secs}})
        );
    } else {
        let exp: usize = expected.values().sum();
        eprintln!(
            "{total} inputs: {same} same, {exp} known divergences, {bugs} unexpected ({:.0} inputs/s)",
            total as f64 / secs.max(1e-9)
        );
        for (id, n) in &expected {
            eprintln!("  known {id}: {n}");
        }
    }
    i32::from(bugs > 0)
}

fn walk(p: &Path, nix_only: bool, out: &mut Vec<PathBuf>) {
    if p.is_dir() {
        let Ok(rd) = std::fs::read_dir(p) else { return };
        let mut entries: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).collect();
        entries.sort();
        for e in entries {
            if !e.is_symlink() {
                walk(&e, nix_only, out);
            }
        }
    } else if !nix_only || p.extension().is_some_and(|e| e == "nix") {
        out.push(p.to_path_buf());
    }
}

/// A tree of a whole file is long; the head is where a human starts.
fn clip(s: &str) -> String {
    const MAX: usize = 400;
    match s.char_indices().nth(MAX) {
        Some((i, _)) => format!("{}… ({} bytes)", &s[..i], s.len()),
        None => s.to_string(),
    }
}

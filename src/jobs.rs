//! `ietsp eval-jobs`: the nix-eval-jobs 2.35 walk over an attribute tree
//! and its JSON lines. The main fiber walks the tree in lexicographic
//! depth-first order and prints; helpers run `T_JOB` range tasks that
//! force what a job needs (`type`, `name`, `drvPath`, output paths, `meta`)
//! ahead of the walk. See docs/eval-jobs.md.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::sync::OnceLock;

use crate::eval::{Error, ErrorKind, EvalResult, Evaluator, Pos};
use crate::parallel::force::{CellState, T_JOB, bail_requested, cell_state, fanout_range};
use crate::print::{json_escape, lexicographic_order, print_json};
use crate::runtime::*;
use crate::store::drv::{Derivation, DerivationOutput, load_derivation_from_store};
use crate::store::path::{STORE_DIR, StorePath};
use crate::syntax::symbol::Sym;

#[derive(Clone, Copy)]
pub struct Opts {
    pub force_recurse: bool,
    pub meta: bool,
    pub show_input_drvs: bool,
    /// `--chunks M --chunk N`: only leaves whose path hash is N mod M.
    pub chunks: u32,
    pub chunk: u32,
}

struct Syms {
    type_: Sym,
    meta: Sym,
    outputs: Sym,
    recurse: Sym,
}

struct Config {
    opts: Opts,
    syms: Syms,
    /// The `--arg`/`--argstr` attrset, auto-called on every function met.
    /// A `Word` into the arena, which is never freed.
    auto_args: u64,
}

static CONFIG: OnceLock<Config> = OnceLock::new();

fn config() -> &'static Config {
    CONFIG.get().expect("eval-jobs config")
}

/// Cells per helper task. A leaf costs a whole build graph, so batches
/// are small.
const JOB_BATCH: u32 = 4;
/// Children the main walk fans out ahead of itself in one go; the urgent
/// deque is bounded, so a huge set is fed in windows.
const WINDOW: usize = 1024;

#[derive(Default, Debug)]
pub struct Stats {
    pub jobs: u64,
    pub errors: u64,
}

/// Install the options for every fiber. Call before `parallel::run`.
/// `auto_args` is the `--arg`/`--argstr` set applied to every function
/// met on the way.
pub fn configure(ev: &mut Evaluator, opts: Opts, auto_args: Word) {
    let syms = Syms {
        type_: ev.sym(b"type"),
        meta: ev.sym(b"meta"),
        outputs: ev.sym(b"outputs"),
        recurse: ev.sym(b"recurseForDerivations"),
    };
    let _ = CONFIG.set(Config {
        opts,
        syms,
        auto_args: auto_args.0,
    });
}

/// Walk `root` and print one JSON line per job or error.
pub fn run(ev: &mut Evaluator, root: Word, out: &mut dyn Write) -> Stats {
    let mut stats = Stats::default();
    let mut path = Vec::new();
    walk(ev, root, &mut path, out, &mut stats);
    stats
}

enum Node {
    Job(Vec<u8>),
    Recurse(&'static AttrsObj),
    Skip,
}

fn walk(
    ev: &mut Evaluator,
    w: Word,
    path: &mut Vec<String>,
    out: &mut dyn Write,
    stats: &mut Stats,
) {
    match node(ev, w, path) {
        Ok(Node::Job(line)) => {
            stats.jobs += 1;
            emit(out, &line);
        }
        Ok(Node::Recurse(attrs)) => {
            let ordered = lexicographic_order(ev, attrs);
            let cells = attrs.attrs();
            let index: Vec<usize> = ordered
                .iter()
                .map(|a| (*a as *const Attr as usize - cells.as_ptr() as usize) / size_of::<Attr>())
                .collect();
            for (n, attr) in ordered.iter().enumerate() {
                if n % WINDOW == 0 {
                    // Fan out the next window (in sym order, which is what
                    // the cells are stored in; the walk order differs but
                    // every window is covered before the walk reaches it).
                    let lo = index[n..(n + WINDOW).min(ordered.len())]
                        .iter()
                        .copied()
                        .min()
                        .unwrap_or(0);
                    let hi = index[n..(n + WINDOW).min(ordered.len())]
                        .iter()
                        .copied()
                        .max()
                        .map_or(0, |i| i + 1);
                    fanout_range(ev, T_JOB, Word::from_ptr(attrs), lo, hi, JOB_BATCH);
                }
                let name = ev.interner.name_str(attr.sym).to_string();
                path.push(name);
                walk(ev, attr.cell.get(), path, out, stats);
                path.pop();
            }
        }
        Ok(Node::Skip) => {}
        Err(e) => {
            stats.errors += 1;
            let text = error_text(ev, &e);
            eprintln!("{text}");
            let mut line = Vec::new();
            line.extend_from_slice(b"{\"attr\":");
            let _ = json_escape(&mut line, attr_join(path).as_bytes());
            line.extend_from_slice(b",\"attrPath\":");
            attr_path_json(&mut line, path);
            line.extend_from_slice(b",\"error\":");
            let _ = json_escape(&mut line, text.as_bytes());
            line.extend_from_slice(b",\"fatal\":false}");
            emit(out, &line);
        }
    }
}

fn emit(out: &mut dyn Write, line: &[u8]) {
    let _ = out.write_all(line);
    let _ = out.write_all(b"\n");
    let _ = out.flush();
}

/// worker.cc:attrPathJoin: elements containing a dot are quoted.
fn attr_join(path: &[String]) -> String {
    let mut s = String::new();
    for (i, p) in path.iter().enumerate() {
        if i > 0 {
            s.push('.');
        }
        if p.contains('.') {
            s.push('"');
            s.push_str(p);
            s.push('"');
        } else {
            s.push_str(p);
        }
    }
    s
}

fn attr_path_json(out: &mut Vec<u8>, path: &[String]) {
    out.push(b'[');
    for (i, p) in path.iter().enumerate() {
        if i > 0 {
            out.push(b',');
        }
        let _ = json_escape(out, p.as_bytes());
    }
    out.push(b']');
}

/// `error: msg` plus the trace, one line each, like `main::report`.
pub fn error_text(ev: &Evaluator, e: &Error) -> String {
    let mut s = format!("error: {}", e.msg);
    if !e.pos.is_none() {
        s.push_str(&format!("\n       at {}", ev.describe_pos(e.pos)));
    }
    for (pos, msg) in e.trace.iter() {
        if pos.is_none() {
            s.push_str(&format!("\n       … {msg}"));
        } else {
            s.push_str(&format!("\n       … {msg} at {}", ev.describe_pos(*pos)));
        }
    }
    s
}

/// FNV-1a over the path, for `--chunks`.
fn path_hash(path: &[String]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for p in path {
        for b in p.bytes().chain(std::iter::once(0)) {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
    }
    h
}

fn in_chunk(path: &[String]) -> bool {
    let o = config().opts;
    o.chunks <= 1 || path_hash(path) % o.chunks as u64 == o.chunk as u64
}

/// attr-path.cc: an all-digit component is a list index.
fn is_index(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|b| b.is_ascii_digit()) && name.parse::<u32>().is_ok()
}

/// eval.cc:isDerivation: an attrset whose `type` is the string "derivation".
fn is_derivation(ev: &mut Evaluator, a: &'static AttrsObj) -> EvalResult<bool> {
    let Some(t) = a.get(config().syms.type_) else {
        return Ok(false);
    };
    let v = ev.force_cell(&t.cell)?;
    Ok(v.as_string().is_some_and(|s| s.bytes == b"derivation"))
}

/// worker.cc:processJobRequest for one attribute path: what to print or
/// where to recurse.
fn node(ev: &mut Evaluator, w: Word, path: &[String]) -> EvalResult<Node> {
    let cfg = config();
    if let Some(last) = path.last()
        && is_index(last)
    {
        // The parent is an attrset, so nix-eval-jobs's list-index lookup fails.
        return Err(Error::boxed(
            ErrorKind::Type,
            format!(
                "the expression selected by the selection path '{}' should be a list but is a set",
                attr_join(path)
            ),
            Pos::NONE,
        ));
    }
    let v = ev.auto_call(w, Word(cfg.auto_args))?;
    let v = ev.force(v)?;
    let Some(attrs) = v.as_attrs() else {
        return Ok(Node::Skip);
    };
    if is_derivation(ev, attrs)? {
        if !in_chunk(path) {
            return Ok(Node::Skip);
        }
        return job(ev, attrs, path).map(Node::Job);
    }
    let mut recurse = cfg.opts.force_recurse || path.is_empty();
    if !cfg.opts.force_recurse
        && let Some(r) = attrs.get(cfg.syms.recurse)
    {
        recurse = ev.force_bool(
            r.cell.get(),
            r.pos,
            "while evaluating recurseForDerivations",
        )?;
    }
    Ok(if recurse {
        Node::Recurse(attrs)
    } else {
        Node::Skip
    })
}

fn coerce_store_path(ev: &mut Evaluator, w: Word, pos: Pos, what: &str) -> EvalResult<StorePath> {
    let mut ctx = Vec::new();
    let s = ev.coerce_to_string(w, &mut ctx, pos, what, false, false, true)?;
    let s = String::from_utf8_lossy(&s).into_owned();
    if !s.starts_with(STORE_DIR) {
        return Err(Error::boxed(
            ErrorKind::Eval,
            format!("path '{s}' is not in the Nix store"),
            pos,
        ));
    }
    StorePath::parse(&s).map_err(|e| Error::boxed(ErrorKind::Eval, e.0, pos))
}

/// get-drvs.cc:queryOutputs(withPaths = true).
fn outputs(ev: &mut Evaluator, a: &'static AttrsObj) -> EvalResult<BTreeMap<String, StorePath>> {
    let mut out = BTreeMap::new();
    match a.get(config().syms.outputs) {
        Some(o) => {
            let l = ev.force_list(
                o.cell.get(),
                o.pos,
                "while evaluating the 'outputs' attribute of a derivation",
            )?;
            for c in l.cells() {
                let name = ev.force_string_no_ctx(
                    c.get(),
                    o.pos,
                    "while evaluating the name of an output of a derivation",
                )?;
                let name = String::from_utf8_lossy(name).into_owned();
                let sym = ev.sym(name.as_bytes());
                let Some(v) = a.get(sym) else { continue };
                let va = ev.force_attrs(
                    v.cell.get(),
                    o.pos,
                    "while evaluating an output of a derivation",
                )?;
                let Some(p) = va.get(ev.s.out_path) else {
                    continue;
                };
                let sp = coerce_store_path(
                    ev,
                    p.cell.get(),
                    p.pos,
                    "while evaluating an output path of a derivation",
                )?;
                out.insert(name, sp);
            }
        }
        None => {
            let Some(p) = a.get(ev.s.out_path) else {
                return Err(Error::boxed(
                    ErrorKind::Eval,
                    "CA derivations are not yet supported",
                    Pos::NONE,
                ));
            };
            let sp = coerce_store_path(
                ev,
                p.cell.get(),
                p.pos,
                "while evaluating the output path of a derivation",
            )?;
            out.insert("out".into(), sp);
        }
    }
    Ok(out)
}

/// get-drvs.cc:PackageInfo::checkMeta: values JSON can carry, no
/// derivations, no functions, no null, no paths.
fn check_meta(ev: &mut Evaluator, w: Word) -> EvalResult<bool> {
    let v = ev.force(w)?;
    Ok(match v.view() {
        Value::List(l) => {
            for c in l.cells() {
                if !check_meta(ev, c.get())? {
                    return Ok(false);
                }
            }
            true
        }
        Value::Attrs(a) => {
            if a.get(ev.s.out_path).is_some() {
                return Ok(false);
            }
            for attr in a.attrs() {
                if !check_meta(ev, attr.cell.get())? {
                    return Ok(false);
                }
            }
            true
        }
        Value::Int(_) | Value::Bool(_) | Value::String(_) | Value::Float(_) => true,
        _ => false,
    })
}

/// drv.cc:queryMeta: `null` without usable entries, else an object.
fn meta_json(ev: &mut Evaluator, a: &'static AttrsObj, out: &mut Vec<u8>) -> EvalResult<()> {
    let Some(m) = a.get(config().syms.meta) else {
        out.extend_from_slice(b"null");
        return Ok(());
    };
    let meta = ev.force_attrs(
        m.cell.get(),
        m.pos,
        "while evaluating the 'meta' attribute of a derivation",
    )?;
    let mut first = true;
    for attr in lexicographic_order(ev, meta) {
        if !check_meta(ev, attr.cell.get())? {
            continue;
        }
        if first {
            out.push(b'{');
        } else {
            out.push(b',');
        }
        first = false;
        json_escape(out, ev.interner.name(attr.sym))?;
        out.push(b':');
        let mut ctx = Vec::new();
        print_json(ev, attr.cell.get(), out, &mut ctx, true, attr.pos)?;
    }
    if first {
        out.extend_from_slice(b"null");
    } else {
        out.push(b'}');
    }
    Ok(())
}

/// The `.drv` nix-eval-jobs reads back from the store: from this
/// evaluation's table, else from `/nix/store`.
fn read_drv(ev: &Evaluator, p: &StorePath) -> EvalResult<Derivation> {
    let err = |m: String| Error::boxed(ErrorKind::Eval, m, Pos::NONE);
    let name = Derivation::name_from_path(p).map_err(|e| err(e.0))?;
    match ev.drv.drvs.get(p).and_then(|i| i.aterm) {
        Some(aterm) => {
            let text = std::str::from_utf8(&aterm)
                .map_err(|_| err(format!("derivation '{p}' is not valid UTF-8")))?;
            Derivation::parse(text, name).map_err(|e| err(e.0))
        }
        None => load_derivation_from_store(p).map_err(|e| err(e.0)),
    }
}

/// derivation-options.cc:getRequiredSystemFeatures.
fn required_system_features(drv: &Derivation) -> BTreeSet<String> {
    let mut set: BTreeSet<String> = match &drv.structured_attrs {
        Some(json) => serde_json::from_str::<serde_json::Value>(json)
            .ok()
            .and_then(|v| v.get("requiredSystemFeatures").cloned())
            .and_then(|v| v.as_array().cloned())
            .map(|a| {
                a.iter()
                    .filter_map(|s| s.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default(),
        None => drv
            .env_get("requiredSystemFeatures")
            .map(|s| s.split_whitespace().map(str::to_owned).collect())
            .unwrap_or_default(),
    };
    let known = drv.outputs.values().all(|o| {
        matches!(
            o,
            DerivationOutput::InputAddressed(_) | DerivationOutput::Fixed { .. }
        )
    });
    if !known {
        set.insert("ca-derivations".into());
    }
    set
}

/// drv.cc:Drv::fromPackageInfo and its JSON: one line, keys in the
/// order nlohmann's std::map gives them.
fn job(ev: &mut Evaluator, a: &'static AttrsObj, path: &[String]) -> EvalResult<Vec<u8>> {
    let cfg = config();
    let name = match a.get(ev.s.name) {
        Some(n) => ev
            .force_string_no_ctx(
                n.cell.get(),
                n.pos,
                "while evaluating the 'name' attribute of a derivation",
            )?
            .to_vec(),
        None => {
            return Err(Error::boxed(
                ErrorKind::Eval,
                "derivation name missing",
                Pos::NONE,
            ));
        }
    };
    let drv_path = match a.get(ev.s.drv_path) {
        Some(d) => {
            let p = coerce_store_path(
                ev,
                d.cell.get(),
                d.pos,
                "while evaluating the 'drvPath' attribute of a derivation",
            )?;
            if !p.is_derivation() {
                return Err(Error::boxed(
                    ErrorKind::Eval,
                    format!("store path '{p}' is not a derivation"),
                    d.pos,
                ));
            }
            p
        }
        None => {
            return Err(Error::boxed(
                ErrorKind::Eval,
                "derivation does not contain a 'drvPath' attribute",
                Pos::NONE,
            ));
        }
    };
    let outs = outputs(ev, a).map_err(|mut e| {
        e.msg = format!(
            "derivation '{}' does not have valid outputs: {}",
            attr_join(path),
            e.msg
        );
        e
    })?;
    let drv = read_drv(ev, &drv_path)?;
    let features = required_system_features(&drv);

    let mut line = Vec::new();
    line.extend_from_slice(b"{\"attr\":");
    json_escape(&mut line, attr_join(path).as_bytes())?;
    line.extend_from_slice(b",\"attrPath\":");
    attr_path_json(&mut line, path);
    line.extend_from_slice(b",\"constituents\":[],\"drvPath\":");
    json_escape(&mut line, drv_path.to_string().as_bytes())?;
    line.extend_from_slice(b",\"globConstituents\":false");
    if cfg.opts.show_input_drvs {
        line.extend_from_slice(b",\"inputDrvs\":{");
        for (i, (p, names)) in drv.input_drvs.iter().enumerate() {
            if i > 0 {
                line.push(b',');
            }
            json_escape(&mut line, p.to_string().as_bytes())?;
            line.extend_from_slice(b":[");
            for (j, n) in names.iter().enumerate() {
                if j > 0 {
                    line.push(b',');
                }
                json_escape(&mut line, n.as_bytes())?;
            }
            line.push(b']');
        }
        line.push(b'}');
    }
    if cfg.opts.meta {
        line.extend_from_slice(b",\"meta\":");
        meta_json(ev, a, &mut line)?;
    }
    line.extend_from_slice(b",\"name\":");
    json_escape(&mut line, &name)?;
    line.extend_from_slice(b",\"namedConstituents\":[],\"outputs\":{");
    for (i, (n, p)) in outs.iter().enumerate() {
        if i > 0 {
            line.push(b',');
        }
        json_escape(&mut line, n.as_bytes())?;
        line.push(b':');
        json_escape(&mut line, p.to_string().as_bytes())?;
    }
    line.extend_from_slice(b"},\"requiredSystemFeatures\":[");
    for (i, f) in features.iter().enumerate() {
        if i > 0 {
            line.push(b',');
        }
        json_escape(&mut line, f.as_bytes())?;
    }
    line.extend_from_slice(b"],\"storeDir\":\"/nix/store\",\"system\":");
    json_escape(&mut line, drv.platform.as_bytes())?;
    line.push(b'}');
    Ok(line)
}

// ---- helper side ------------------------------------------------------------

/// A `T_JOB` range task: force what the main walk will ask of each child
/// in `start..end` of `attrs`. Never parks on a cell another fiber holds;
/// an evaluation error in one child does not stop the others.
pub fn prefetch_range(ev: &mut Evaluator, attrs: &'static AttrsObj, start: usize, end: usize) {
    for attr in &attrs.attrs()[start..end] {
        if bail_requested(ev) || ev.par.cancelled {
            return;
        }
        let v = match cell_state(&attr.cell) {
            CellState::Busy => continue,
            CellState::Ready(v) => {
                attr.cell.set(v);
                v
            }
            CellState::Unclaimed => match ev.force_cell(&attr.cell) {
                Ok(v) => v,
                Err(_) => continue,
            },
        };
        let _ = prefetch_value(ev, v, 0);
    }
}

fn prefetch_value(ev: &mut Evaluator, v: Word, depth: u32) -> EvalResult<()> {
    if depth > 64 {
        return Ok(());
    }
    let cfg = config();
    let v = ev.auto_call(v, Word(cfg.auto_args))?;
    let Some(a) = v.as_attrs() else {
        return Ok(());
    };
    if is_derivation(ev, a)? {
        let _ = prefetch_job(ev, a);
        return Ok(());
    }
    let mut recurse = cfg.opts.force_recurse;
    if !recurse && let Some(r) = a.get(cfg.syms.recurse) {
        recurse = ev.force_bool(r.cell.get(), r.pos, "")?;
    }
    if !recurse {
        return Ok(());
    }
    fanout_range(ev, T_JOB, v, 0, a.len(), JOB_BATCH);
    for attr in a.attrs() {
        if bail_requested(ev) || ev.par.cancelled {
            return Ok(());
        }
        let c = match cell_state(&attr.cell) {
            CellState::Busy => continue,
            CellState::Ready(w) => {
                attr.cell.set(w);
                w
            }
            CellState::Unclaimed => match ev.force_cell(&attr.cell) {
                Ok(w) => w,
                Err(e) if e.kind == ErrorKind::Cancelled => return Err(e),
                Err(_) => continue,
            },
        };
        let _ = prefetch_value(ev, c, depth + 1);
    }
    Ok(())
}

/// Helpers cannot know the chunk of a leaf without its path, and the
/// main walk skips the leaf anyway, so the chunk filter is main-only.
fn prefetch_job(ev: &mut Evaluator, a: &'static AttrsObj) -> EvalResult<()> {
    let cfg = config();
    if let Some(n) = a.get(ev.s.name) {
        ev.force(n.cell.get())?;
    }
    if let Some(d) = a.get(ev.s.drv_path) {
        let mut ctx = Vec::new();
        ev.coerce_to_string(d.cell.get(), &mut ctx, d.pos, "", false, false, true)?;
    }
    outputs(ev, a)?;
    if cfg.opts.meta
        && let Some(m) = a.get(cfg.syms.meta)
    {
        let meta = ev.force_attrs(m.cell.get(), m.pos, "")?;
        for attr in meta.attrs() {
            if check_meta(ev, attr.cell.get())? {
                ev.force_deep(attr.cell.get())?;
            }
        }
    }
    Ok(())
}

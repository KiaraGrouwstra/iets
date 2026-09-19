//! Primop registration. Each family lives in its own file and exposes
//! `pub fn register(r: &mut Registry)`. See `docs/builtins-api.md`.

pub mod attrs;
pub mod context;
pub mod core;
pub mod derivation;
pub mod lists;
pub mod math;
pub mod paths;
pub mod store_hooks;
pub mod strings;
pub mod text;

use crate::eval::{Evaluator, Pos};
use crate::runtime::*;
use crate::syntax::symbol::Sym;

pub struct Registry {
    pub ops: Vec<(&'static str, u8, PrimFn, bool)>,
    pub consts: Vec<(&'static str, Word, bool)>,
}

impl Registry {
    /// Register `name` (with or without the `__` prefix). Names without the
    /// prefix are also visible as globals; prefixed ones are globals under
    /// their prefixed name and appear in `builtins` unprefixed.
    pub fn op(&mut self, name: &'static str, arity: u8, f: PrimFn) {
        self.ops.push((name, arity, f, true));
    }

    /// Only in `builtins`, no global at all.
    pub fn op_hidden(&mut self, name: &'static str, arity: u8, f: PrimFn) {
        self.ops.push((name, arity, f, false));
    }

    pub fn constant(&mut self, name: &'static str, w: Word) {
        self.consts.push((name, w, true));
    }
}

/// Embedded corepkgs, addressed as `<nix/NAME>`.
pub fn corepkgs(name: &[u8]) -> Option<&'static [u8]> {
    match name {
        b"fetchurl.nix" => Some(include_bytes!("../corepkgs/fetchurl.nix")),
        b"derivation.nix" => Some(include_bytes!("../corepkgs/derivation.nix")),
        _ => None,
    }
}

fn builtins_name(name: &'static str) -> &'static str {
    name.strip_prefix("__").unwrap_or(name)
}

pub fn install(ev: &mut Evaluator) {
    let mut r = Registry {
        ops: Vec::new(),
        consts: Vec::new(),
    };
    core::register(&mut r);
    attrs::register(&mut r);
    lists::register(&mut r);
    strings::register(&mut r);
    math::register(&mut r);
    paths::register(&mut r);
    context::register(&mut r);
    text::register(&mut r);
    derivation::register(&mut r);

    r.constant("true", Word::TRUE);
    r.constant("false", Word::FALSE);
    r.constant("null", Word::NULL);
    let sys = ev.mk_string(ev.current_system.as_bytes());
    r.constant("__currentSystem", sys);
    let ver = ev.mk_string(b"2.34.8");
    r.constant("__nixVersion", ver);
    r.constant("__langVersion", Word::imm_int(6));
    let sd = ev.store_dir.clone();
    let sd = ev.mk_string(&sd);
    r.constant("__storeDir", sd);
    let np = ev.mk_list(&[]);
    r.constant("__nixPath", np);
    let ct_op = r
        .ops
        .iter()
        .find(|(n, ..)| *n == "currentTimeRaw")
        .map(|(n, a, f, _)| ev.arena.mk_primop(n, *a, *f))
        .expect("currentTimeRaw registered");
    let ct = ev.arena.mk_thunk_app(ct_op, Word::NULL);
    r.constant("__currentTime", ct);

    let mut entries: Vec<(Sym, Pos, Word)> = Vec::new();
    for (name, arity, f, global) in &r.ops {
        let w = ev.arena.mk_primop(name, *arity, *f);
        let bsym = ev.sym(builtins_name(name).as_bytes());
        entries.push((bsym, Pos::NONE, w));
        if *global {
            let gsym = ev.sym(name.as_bytes());
            ev.globals.table.insert(gsym, w);
        }
    }
    for (name, w, global) in &r.consts {
        let bsym = ev.sym(builtins_name(name).as_bytes());
        entries.push((bsym, Pos::NONE, *w));
        if *global {
            let gsym = ev.sym(name.as_bytes());
            ev.globals.table.insert(gsym, *w);
        }
    }
    let derivation_sym = ev.sym(b"derivation");
    let builtins_sym = ev.sym(b"builtins");
    entries.push((derivation_sym, Pos::NONE, Word::NULL));
    entries.push((builtins_sym, Pos::NONE, Word::NULL));
    let bw = ev
        .arena
        .mk_attrs(&mut entries)
        .expect("builtin names are unique");
    let battrs = bw.as_attrs().unwrap();
    battrs.get(builtins_sym).unwrap().cell.set(bw);
    ev.builtins_word = bw;
    ev.globals.table.insert(builtins_sym, bw);
    ev.globals.builtins = Some(battrs);

    let src = include_str!("../corepkgs/derivation.nix");
    let ir = ev
        .compile_internal(b"\xc2\xabderivation-internal\xc2\xbb", src.as_bytes())
        .expect("derivation.nix compiles");
    let root = ev.root_env;
    let d = ev.eval(root, ir).expect("derivation.nix evaluates");
    battrs.get(derivation_sym).unwrap().cell.set(d);
    ev.globals.table.insert(derivation_sym, d);
}

/// Install `-I`/`NIX_PATH` entries and refresh `builtins.nixPath`.
pub fn set_search_path(ev: &mut Evaluator, entries: Vec<(Vec<u8>, Vec<u8>)>) {
    let mut items = Vec::with_capacity(entries.len());
    for (prefix, path) in &entries {
        let p = ev.mk_string(path);
        let pre = ev.mk_string(prefix);
        let mut e = vec![(ev.s.path, Pos::NONE, p), (ev.s.prefix, Pos::NONE, pre)];
        items.push(ev.arena.mk_attrs(&mut e).unwrap());
    }
    let list = ev.mk_list(&items);
    let sym = ev.sym(b"nixPath");
    if let Some(b) = ev.globals.builtins
        && let Some(a) = b.get(sym)
    {
        a.cell.set(list);
    }
    let gsym = ev.sym(b"__nixPath");
    ev.globals.table.insert(gsym, list);
    ev.search_path = entries;
}

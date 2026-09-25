//! Compiled-IR cache on disk. A file's IR is written after its first
//! compile and loaded instead of parsing and compiling on later runs
//! (docs/explanation/caching.md, "IR cache").
//!
//! The IR is process-specific in three ways, and the entry undoes each:
//! `Sym` ids (a per-entry name table is re-interned on load), `Pos` values
//! (stored relative to the file, rebased on load), and constant `Word`s
//! (literals are stored by content, globals and `builtins.x` by name and
//! looked up in the live tables, so `__nixPath` follows the run). Binding
//! frames (`let`, `rec`, formals) are slot-ordered by `Sym` id, so the
//! loader re-sorts them and rewrites every `Local` index through a
//! permutation stack that mirrors the compiler's scope pushes.

use std::path::PathBuf;

use super::ir::*;
use crate::eval::{Evaluator, Pos};
use crate::runtime::{Kind, Word};
use crate::syntax::symbol::{FxHashMap, Sym};

const MAGIC: &[u8; 8] = b"IETSPIR2";

pub struct IrCache {
    root: PathBuf,
    /// Sources below this size compile faster than their entry loads.
    pub min_size: usize,
    /// `IETSP_IR_STATS=1`: one line per file on stderr.
    pub stats: bool,
    /// The store is swept at most once per process, on the first write.
    swept: std::sync::atomic::AtomicBool,
}

impl IrCache {
    /// `--cache-dir DIR`, else `IETSP_CACHE_DIR`, else `$XDG_CACHE_HOME/ietsp`,
    /// else `~/.cache/ietsp`; the entries live in its `ir/` subdirectory.
    pub fn locate(override_dir: Option<&str>, timings: bool) -> Option<IrCache> {
        let session = crate::session::Session::current();
        let set = session.settings();
        let base = if let Some(d) = override_dir {
            PathBuf::from(d)
        } else if let Some(d) = &set.cache_dir {
            d.clone()
        } else {
            let base = match std::env::var_os("XDG_CACHE_HOME") {
                Some(x) if !x.is_empty() => PathBuf::from(x),
                _ => PathBuf::from(std::env::var_os("HOME")?).join(".cache"),
            };
            base.join("ietsp")
        };
        let root = base.join("ir");
        crate::cachefs::prepare(&root, timings);
        Some(IrCache {
            root,
            min_size: set.ir_min,
            stats: set.ir_stats,
            swept: std::sync::atomic::AtomicBool::new(false),
        })
    }

    pub fn root(&self) -> &std::path::Path {
        &self.root
    }

    /// `lint_key` is `lint::Config::compile_key`: the compile-time lint
    /// levels, whose warnings the entry carries.
    pub fn key(
        &self,
        path: &[u8],
        src: &[u8],
        system: &str,
        store_dir: &[u8],
        lint_key: &str,
    ) -> String {
        let mut h = blake3::Hasher::new();
        for part in [
            crate::trace::key::BUILD_ID.as_bytes(),
            path,
            system.as_bytes(),
            store_dir,
            lint_key.as_bytes(),
        ] {
            h.update(part);
            h.update(b"\0");
        }
        h.update(src);
        h.finalize().to_hex().to_string()
    }

    fn entry(&self, key: &str) -> PathBuf {
        self.root.join(&key[..2]).join(&key[2..])
    }

    /// A hit marks the entry as used from the same `fstat` that sizes the
    /// read, and only when the mark is due (`cachefs::touch_open`).
    pub fn read(&self, key: &str) -> Option<Vec<u8>> {
        use std::io::Read;
        let f = std::fs::File::open(self.entry(key)).ok()?;
        let md = f.metadata().ok()?;
        let mut bytes = Vec::with_capacity(md.len() as usize + 1);
        let mut r = &f;
        r.read_to_end(&mut bytes).ok()?;
        crate::cachefs::touch_open(&f, md.modified().ok());
        Some(bytes)
    }

    /// Skips the write when a sibling process holds the store lock: the file
    /// is compiled again next run, which is cheaper than waiting here.
    pub fn write(&self, key: &str, bytes: &[u8]) {
        use std::sync::atomic::Ordering;
        let path = self.entry(key);
        let dir = path.parent().expect("entry has a parent");
        let _ = std::fs::create_dir_all(dir);
        let name = &key[2..];
        if let Some(_lock) = crate::cachefs::write_lock(&self.root) {
            let _ = crate::cachefs::write_atomic(dir, name, crate::cachefs::tag(), bytes);
        } else if self.stats {
            crate::errln!("ir-cache skip {name} (a sibling holds the store lock)");
        }
        if !self.swept.swap(true, Ordering::Relaxed) {
            crate::cachefs::maybe_sweep(
                &self.root,
                crate::cachefs::Shape::ShardedFiles,
                self.stats,
            );
        }
    }

    /// Poison and wipe, never repair: an entry that does not decode goes.
    pub fn discard(&self, key: &str) {
        let _ = std::fs::remove_file(self.entry(key));
    }
}

// ---- encoding ---------------------------------------------------------------

fn put_u32(out: &mut Vec<u8>, mut v: u32) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn put_u64(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn put_bytes(out: &mut Vec<u8>, b: &[u8]) {
    put_u32(out, b.len() as u32);
    out.extend_from_slice(b);
}

struct Ser {
    out: Vec<u8>,
    syms: FxHashMap<Sym, u32>,
    names: Vec<Sym>,
    base: u32,
    /// Heap constant word -> (is a global, its name); built from the live tables.
    rev: FxHashMap<u64, (bool, Sym)>,
    ok: bool,
}

/// The entry for `ir`, the lint warnings its compile produced and the
/// file's `# ietsp: allow` table, or `None` when a constant cannot be named.
pub fn serialize(
    ev: &Evaluator,
    ir: IrRef,
    base: u32,
    diags: &[crate::lint::Diag],
    allows: &crate::lint::allow::Allows,
) -> Option<Vec<u8>> {
    let mut rev = FxHashMap::default();
    if let Some(b) = ev.globals.builtins {
        for a in b.iter() {
            rev.insert(a.cell.get().0, (false, a.sym));
        }
    }
    for (sym, w) in &ev.globals.table {
        rev.insert(w.0, (true, *sym));
    }
    let mut s = Ser {
        out: Vec::with_capacity(1 << 16),
        syms: FxHashMap::default(),
        names: Vec::new(),
        base,
        rev,
        ok: true,
    };
    s.node(ir);
    if !s.ok {
        return None;
    }
    put_u32(&mut s.out, diags.len() as u32);
    for d in diags {
        s.out.push(d.id as u8);
        s.pos(d.pos);
        put_bytes(&mut s.out, d.msg.as_bytes());
    }
    put_u32(&mut s.out, allows.lines().len() as u32);
    for l in allows.lines() {
        put_u32(&mut s.out, l.start);
        put_u32(&mut s.out, l.end);
        put_u64(&mut s.out, l.mask);
    }
    let mut file = Vec::with_capacity(s.out.len() + 16 * s.names.len() + 16);
    file.extend_from_slice(MAGIC);
    put_u32(&mut file, s.names.len() as u32);
    for sym in &s.names {
        put_bytes(&mut file, ev.interner.name(*sym));
    }
    file.extend_from_slice(&s.out);
    Some(file)
}

impl Ser {
    fn sym(&mut self, s: Sym) {
        let n = self.names.len() as u32;
        let id = *self.syms.entry(s).or_insert_with(|| {
            self.names.push(s);
            n
        });
        put_u32(&mut self.out, id);
    }

    fn pos(&mut self, p: Pos) {
        let v = if p.is_none() {
            u32::MAX
        } else {
            debug_assert!(p.0 >= self.base);
            p.0 - self.base
        };
        put_u32(&mut self.out, v);
    }

    fn opt_sym(&mut self, s: Option<Sym>) {
        match s {
            None => self.out.push(0),
            Some(s) => {
                self.out.push(1);
                self.sym(s);
            }
        }
    }

    fn opt_node(&mut self, ir: Option<IrRef>) {
        match ir {
            None => self.out.push(0),
            Some(ir) => {
                self.out.push(1);
                self.node(ir);
            }
        }
    }

    fn nodes(&mut self, irs: &[IrRef]) {
        put_u32(&mut self.out, irs.len() as u32);
        for ir in irs {
            self.node(ir);
        }
    }

    fn elems(&mut self, path: &[AttrElemIr]) {
        put_u32(&mut self.out, path.len() as u32);
        for e in path {
            match e {
                AttrElemIr::Static(s) => {
                    self.out.push(0);
                    self.sym(*s);
                }
                AttrElemIr::Dynamic(ir) => {
                    self.out.push(1);
                    self.node(ir);
                }
            }
        }
    }

    fn word(&mut self, w: Word) {
        if w.is_ptr() {
            match w.heap_kind() {
                Some(Kind::String) => {
                    let s = w.as_string().expect("string kind");
                    if !s.ctx.is_empty() {
                        self.ok = false;
                        return;
                    }
                    self.out.push(1);
                    put_bytes(&mut self.out, s.bytes);
                    return;
                }
                Some(Kind::Path) => {
                    let p = match w.view() {
                        crate::runtime::Value::Path(p) => p,
                        _ => unreachable!(),
                    };
                    self.out.push(2);
                    put_bytes(&mut self.out, p.path);
                    return;
                }
                Some(Kind::Float) => {
                    let f = match w.view() {
                        crate::runtime::Value::Float(f) => f,
                        _ => unreachable!(),
                    };
                    self.out.push(3);
                    self.out.extend_from_slice(&f.to_bits().to_le_bytes());
                    return;
                }
                Some(Kind::Int) => {
                    let i = w.as_int().expect("int kind");
                    self.out.push(4);
                    self.out.extend_from_slice(&i.to_le_bytes());
                    return;
                }
                _ => {}
            }
        } else if w.tag() == crate::runtime::word::TAG_INT
            || w.tag() == crate::runtime::word::TAG_IMM
        {
            self.out.push(0);
            put_u64(&mut self.out, w.0);
            return;
        }
        match self.rev.get(&w.0) {
            Some(&(global, sym)) => {
                self.out.push(if global { 5 } else { 6 });
                self.sym(sym);
            }
            None => self.ok = false,
        }
    }

    /// A binding frame: the headers, the `from` count, then the values and
    /// the `from` expressions. The loader needs both counts before it reads
    /// a value, so it can size the slot permutation.
    fn binds(&mut self, binds: &[BindIr], from: &[IrRef]) {
        put_u32(&mut self.out, binds.len() as u32);
        for b in binds {
            self.sym(b.sym);
            self.pos(b.pos);
            self.out.push(match b.scope {
                BindScope::Own => 0,
                BindScope::Outer => 1,
                BindScope::From => 2,
            });
        }
        put_u32(&mut self.out, from.len() as u32);
        for b in binds {
            self.node(b.value);
        }
        for f in from {
            self.node(f);
        }
    }

    fn node(&mut self, ir: IrRef) {
        if !self.ok {
            return;
        }
        match ir {
            Ir::Const(w) => {
                self.out.push(0);
                self.word(*w);
            }
            Ir::Local { depth, idx } => {
                self.out.push(1);
                put_u32(&mut self.out, *depth);
                put_u32(&mut self.out, *idx);
            }
            Ir::WithVar { sym, withs, pos } => {
                self.out.push(2);
                self.sym(*sym);
                put_u32(&mut self.out, withs.len() as u32);
                for w in withs.iter() {
                    put_u32(&mut self.out, *w);
                }
                self.pos(*pos);
            }
            Ir::CurPos(pos) => {
                self.out.push(3);
                self.pos(*pos);
            }
            Ir::Concat {
                force_string,
                parts,
                pos,
            } => {
                self.out.push(4);
                self.out.push(*force_string as u8);
                self.nodes(parts);
                self.pos(*pos);
            }
            Ir::SearchPath { name, pos } => {
                self.out.push(5);
                self.node(name);
                self.pos(*pos);
            }
            Ir::Select(s) => {
                self.out.push(6);
                self.node(s.subject);
                self.elems(s.path);
                self.opt_node(s.default);
                self.pos(s.pos);
            }
            Ir::Select1 {
                subject,
                sym,
                default,
                pos,
            } => {
                self.out.push(7);
                self.node(subject);
                self.sym(*sym);
                self.opt_node(*default);
                self.pos(*pos);
            }
            Ir::HasAttr { subject, path } => {
                self.out.push(8);
                self.node(subject);
                self.elems(path);
            }
            Ir::Attrs(a) => {
                self.out.push(9);
                self.out.push(a.rec as u8);
                self.pos(a.pos);
                self.binds(a.binds, a.from);
                put_u32(&mut self.out, a.dynamic.len() as u32);
                for d in a.dynamic.iter() {
                    self.node(d.name);
                    self.node(d.value);
                    self.pos(d.pos);
                }
            }
            Ir::List(items) => {
                self.out.push(10);
                self.nodes(items);
            }
            Ir::Lambda(l) => {
                self.out.push(11);
                match &l.param {
                    ParamIr::Ident => self.out.push(0),
                    ParamIr::Formals {
                        has_bind,
                        formals,
                        ellipsis,
                    } => {
                        self.out.push(1);
                        self.out.push(*has_bind as u8);
                        self.out.push(*ellipsis as u8);
                        put_u32(&mut self.out, formals.len() as u32);
                        for f in formals.iter() {
                            self.sym(f.sym);
                            self.pos(f.pos);
                        }
                        for f in formals.iter() {
                            self.opt_node(f.default);
                        }
                    }
                }
                self.node(l.body);
                self.pos(l.pos);
                self.opt_sym(l.name);
                self.opt_sym(l.arg);
                self.out.push(l.closed as u8);
            }
            Ir::Apply { func, args, pos } => {
                self.out.push(12);
                self.node(func);
                self.nodes(args);
                self.pos(*pos);
            }
            Ir::Let(l) => {
                self.out.push(13);
                self.binds(l.binds, l.from);
                self.node(l.body);
            }
            Ir::With { scope, body, pos } => {
                self.out.push(14);
                self.node(scope);
                self.node(body);
                self.pos(*pos);
            }
            Ir::If {
                cond,
                then,
                else_,
                pos,
            } => {
                self.out.push(15);
                self.node(cond);
                self.node(then);
                self.node(else_);
                self.pos(*pos);
            }
            Ir::Assert { cond, body, pos } => {
                self.out.push(16);
                self.node(cond);
                self.node(body);
                self.pos(*pos);
            }
            Ir::Not(e, pos) => self.unary(17, e, *pos),
            Ir::Neg(e, pos) => self.unary(18, e, *pos),
            Ir::Eq(l, r, pos) => self.binary(19, l, r, *pos),
            Ir::Ne(l, r, pos) => self.binary(20, l, r, *pos),
            Ir::Lt(l, r, pos) => self.binary(21, l, r, *pos),
            Ir::Gt(l, r, pos) => self.binary(22, l, r, *pos),
            Ir::Le(l, r, pos) => self.binary(23, l, r, *pos),
            Ir::Ge(l, r, pos) => self.binary(24, l, r, *pos),
            Ir::And(l, r, pos) => self.binary(25, l, r, *pos),
            Ir::Or(l, r, pos) => self.binary(26, l, r, *pos),
            Ir::Impl(l, r, pos) => self.binary(27, l, r, *pos),
            Ir::Update(l, r, pos) => self.binary(28, l, r, *pos),
            Ir::ConcatLists(l, r, pos) => self.binary(29, l, r, *pos),
            Ir::Sub(l, r, pos) => self.binary(30, l, r, *pos),
            Ir::Mul(l, r, pos) => self.binary(31, l, r, *pos),
            Ir::Div(l, r, pos) => self.binary(32, l, r, *pos),
        }
    }

    fn unary(&mut self, tag: u8, e: IrRef, pos: Pos) {
        self.out.push(tag);
        self.node(e);
        self.pos(pos);
    }

    fn binary(&mut self, tag: u8, l: IrRef, r: IrRef, pos: Pos) {
        self.out.push(tag);
        self.node(l);
        self.node(r);
        self.pos(pos);
    }
}

// ---- decoding ---------------------------------------------------------------

struct De<'a, 'e> {
    ev: &'e mut Evaluator,
    data: &'a [u8],
    at: usize,
    syms: Vec<Sym>,
    base: u32,
    /// One entry per scope the compiler pushed, innermost last: the slot
    /// permutation of a re-sorted frame, or `None` for an unchanged one.
    frames: Vec<Option<Vec<u32>>>,
    ok: bool,
}

/// The IR of an entry, allocated in the evaluator's arena with positions
/// rebased at `base`, plus its lint warnings and the file's allow table;
/// `None` on a malformed entry.
pub fn deserialize(
    ev: &mut Evaluator,
    data: &[u8],
    base: u32,
    stats: bool,
) -> Option<(IrRef, Vec<crate::lint::Diag>, crate::lint::allow::Allows)> {
    let t0 = std::time::Instant::now();
    if data.len() < MAGIC.len() || &data[..MAGIC.len()] != MAGIC {
        return None;
    }
    let mut d = De {
        ev,
        data,
        at: MAGIC.len(),
        syms: Vec::new(),
        base,
        frames: Vec::new(),
        ok: true,
    };
    let n = d.u32() as usize;
    d.syms.reserve(n);
    for _ in 0..n {
        let len = d.u32() as usize;
        let end = d.at.saturating_add(len);
        if end > data.len() {
            return None;
        }
        let s = d.ev.interner.intern(&data[d.at..end]);
        d.at = end;
        d.syms.push(s);
    }
    let t1 = t0.elapsed();
    let ir = d.node();
    let ndiags = d.u32() as usize;
    let mut diags = Vec::with_capacity(ndiags.min(1024));
    for _ in 0..ndiags {
        if !d.ok {
            break;
        }
        let id = crate::lint::LintId::from_u8(d.u8());
        let pos = d.pos();
        let msg = String::from_utf8_lossy(d.bytes()).into_owned();
        match id {
            Some(id) => diags.push(crate::lint::Diag { id, pos, msg }),
            None => d.ok = false,
        }
    }
    let nallows = d.u32() as usize;
    let mut allows = crate::lint::allow::Allows::default();
    for _ in 0..nallows {
        if !d.ok {
            break;
        }
        let start = d.u32();
        let end = d.u32();
        let mask = d.u64();
        allows.add(start, end, mask);
    }
    if stats {
        crate::errln!(
            "ir-cache   syms={} {}us nodes {}us",
            n,
            t1.as_micros(),
            (t0.elapsed() - t1).as_micros()
        );
    }
    if d.ok && d.at == data.len() {
        Some((ir, diags, allows))
    } else {
        None
    }
}

impl De<'_, '_> {
    fn u8(&mut self) -> u8 {
        match self.data.get(self.at) {
            Some(b) => {
                self.at += 1;
                *b
            }
            None => {
                self.ok = false;
                0
            }
        }
    }

    fn u64(&mut self) -> u64 {
        let mut v = 0u64;
        let mut shift = 0;
        loop {
            let b = self.u8();
            v |= u64::from(b & 0x7f) << shift;
            if b < 0x80 || !self.ok || shift >= 63 {
                return v;
            }
            shift += 7;
        }
    }

    fn u32(&mut self) -> u32 {
        self.u64() as u32
    }

    fn bytes(&mut self) -> &[u8] {
        let n = self.u32() as usize;
        let end = self.at.saturating_add(n);
        if end > self.data.len() {
            self.ok = false;
            return &[];
        }
        let b = &self.data[self.at..end];
        self.at = end;
        b
    }

    fn sym(&mut self) -> Sym {
        let i = self.u32() as usize;
        match self.syms.get(i) {
            Some(s) => *s,
            None => {
                self.ok = false;
                Sym(0)
            }
        }
    }

    fn pos(&mut self) -> Pos {
        let v = self.u32();
        if v == u32::MAX {
            Pos::NONE
        } else {
            Pos(self.base + v)
        }
    }

    fn opt_sym(&mut self) -> Option<Sym> {
        if self.u8() == 0 {
            None
        } else {
            Some(self.sym())
        }
    }

    fn opt_node(&mut self) -> Option<IrRef> {
        if self.u8() == 0 {
            None
        } else {
            Some(self.node())
        }
    }

    /// A count that a run of items follows; capped by the bytes left so a
    /// corrupt entry cannot reserve gigabytes.
    fn count(&mut self) -> usize {
        let n = self.u32() as usize;
        n.min(self.data.len() - self.at.min(self.data.len()))
    }

    /// The arena slice is reserved first and filled as the items decode,
    /// so a list costs no scratch vector.
    fn fill<T>(&mut self, n: usize, mut item: impl FnMut(&mut Self) -> T) -> &'static [T] {
        if n == 0 {
            return &[];
        }
        let p = self.ev.arena.alloc_raw(n * size_of::<T>(), align_of::<T>()) as *mut T;
        // SAFETY: fresh allocation of n elements; every slot is written
        // below before the slice is made (a failed decode still writes a
        // value; the entry is dropped by the caller).
        unsafe {
            for i in 0..n {
                p.add(i).write(item(self));
            }
            std::slice::from_raw_parts(p, n)
        }
    }

    fn nodes(&mut self) -> &'static [IrRef] {
        let n = self.count();
        self.fill(n, |d| d.node())
    }

    fn elems(&mut self) -> &'static [AttrElemIr] {
        let n = self.count();
        self.fill(n, |d| {
            if d.u8() == 0 {
                AttrElemIr::Static(d.sym())
            } else {
                AttrElemIr::Dynamic(d.node())
            }
        })
    }

    fn word(&mut self) -> Word {
        match self.u8() {
            0 => Word(self.u64()),
            1 => {
                let data = self.data;
                let b = self.bytes();
                let b = &data[b.as_ptr() as usize - data.as_ptr() as usize..][..b.len()];
                self.ev.arena.mk_string(b)
            }
            2 => {
                let data = self.data;
                let b = self.bytes();
                let b = &data[b.as_ptr() as usize - data.as_ptr() as usize..][..b.len()];
                self.ev.arena.mk_path(b)
            }
            3 => {
                let mut raw = [0u8; 8];
                for r in raw.iter_mut() {
                    *r = self.u8();
                }
                self.ev
                    .arena
                    .mk_float(f64::from_bits(u64::from_le_bytes(raw)))
            }
            4 => {
                let mut raw = [0u8; 8];
                for r in raw.iter_mut() {
                    *r = self.u8();
                }
                self.ev.arena.mk_int(i64::from_le_bytes(raw))
            }
            5 => {
                let s = self.sym();
                match self.ev.globals.table.get(&s) {
                    Some(w) => *w,
                    None => {
                        self.ok = false;
                        Word::NULL
                    }
                }
            }
            6 => {
                let s = self.sym();
                match self.ev.globals.builtins.and_then(|b| b.get(s)) {
                    Some(a) => a.cell.get(),
                    None => {
                        self.ok = false;
                        Word::NULL
                    }
                }
            }
            _ => {
                self.ok = false;
                Word::NULL
            }
        }
    }

    /// The permutation that moves entries from their stored order to
    /// `Sym` order: `perm[old] = new`. `extra` identity slots follow.
    fn perm(syms: &[Sym], extra: usize) -> Option<Vec<u32>> {
        if syms.windows(2).all(|w| w[0] < w[1]) {
            return None;
        }
        let mut order: Vec<u32> = (0..syms.len() as u32).collect();
        order.sort_by_key(|&i| syms[i as usize]);
        let mut perm = vec![0u32; syms.len() + extra];
        for (new, old) in order.iter().enumerate() {
            perm[*old as usize] = new as u32;
        }
        for (i, p) in perm.iter_mut().enumerate().skip(syms.len()) {
            *p = i as u32;
        }
        Some(perm)
    }

    fn frame_of(&self, depth: u32) -> Option<&Vec<u32>> {
        let n = self.frames.len();
        let i = n.checked_sub(depth as usize + 1)?;
        self.frames[i].as_ref()
    }

    /// A binding frame: `(binds, from)` with the binds in `Sym` order.
    /// `sorted` frames (`let`, `rec`) are pushed while their values and
    /// `from` expressions load; a plain set's `inherit (e)` selects load
    /// under the anonymous from-env instead.
    fn binds(&mut self, sorted: bool) -> (&'static [BindIr], &'static [IrRef]) {
        let n = self.count();
        let mut syms = Vec::with_capacity(n);
        let mut poss = Vec::with_capacity(n);
        let mut scopes = Vec::with_capacity(n);
        for _ in 0..n {
            syms.push(self.sym());
            poss.push(self.pos());
            scopes.push(match self.u8() {
                0 => BindScope::Own,
                1 => BindScope::Outer,
                _ => BindScope::From,
            });
        }
        let nfrom = self.count();
        let perm = Self::perm(&syms, if sorted { nfrom } else { 0 });
        if sorted {
            self.frames.push(perm.clone());
        }
        let out = self.fill(n, |_| BindIr {
            sym: Sym(0),
            value: &Ir::Const(Word::NULL),
            pos: Pos::NONE,
            scope: BindScope::Own,
        });
        // SAFETY: `fill` handed out a fresh arena slice that nothing else
        // holds yet; the entries are rewritten in place below.
        let out_mut = unsafe { std::slice::from_raw_parts_mut(out.as_ptr() as *mut BindIr, n) };
        for i in 0..n {
            if !self.ok {
                break;
            }
            let value = match scopes[i] {
                BindScope::Outer if sorted => {
                    let f = self.frames.pop();
                    let v = self.node();
                    self.frames.extend(f);
                    v
                }
                BindScope::From if !sorted => {
                    self.frames.push(None);
                    let v = self.node();
                    self.frames.pop();
                    v
                }
                _ => self.node(),
            };
            let new = perm.as_ref().map_or(i, |p| p[i] as usize);
            out_mut[new] = BindIr {
                sym: syms[i],
                value,
                pos: poss[i],
                scope: scopes[i],
            };
        }
        let from = self.fill(nfrom, |d| d.node());
        (out, from)
    }

    fn node(&mut self) -> IrRef {
        if !self.ok {
            return &Ir::Const(Word::NULL);
        }
        let ir = match self.u8() {
            0 => Ir::Const(self.word()),
            1 => {
                let depth = self.u32();
                let idx = self.u32();
                let idx = match self.frame_of(depth) {
                    Some(perm) => match perm.get(idx as usize) {
                        Some(i) => *i,
                        None => {
                            self.ok = false;
                            0
                        }
                    },
                    None => idx,
                };
                Ir::Local { depth, idx }
            }
            2 => {
                let sym = self.sym();
                let n = self.count();
                let withs = self.fill(n, |d| d.u32());
                let pos = self.pos();
                Ir::WithVar { sym, withs, pos }
            }
            3 => Ir::CurPos(self.pos()),
            4 => {
                let force_string = self.u8() != 0;
                let parts = self.nodes();
                let pos = self.pos();
                Ir::Concat {
                    force_string,
                    parts,
                    pos,
                }
            }
            5 => {
                let name = self.node();
                let pos = self.pos();
                Ir::SearchPath { name, pos }
            }
            6 => {
                let subject = self.node();
                let path = self.elems();
                let default = self.opt_node();
                let pos = self.pos();
                Ir::Select(self.ev.arena.alloc(SelectIr {
                    subject,
                    path,
                    default,
                    pos,
                }))
            }
            7 => {
                let subject = self.node();
                let sym = self.sym();
                let default = self.opt_node();
                let pos = self.pos();
                Ir::Select1 {
                    subject,
                    sym,
                    default,
                    pos,
                }
            }
            8 => {
                let subject = self.node();
                let path = self.elems();
                Ir::HasAttr { subject, path }
            }
            9 => {
                let rec = self.u8() != 0;
                let pos = self.pos();
                let (binds, from) = self.binds(rec);
                let ndyn = self.count();
                let dynamic = self.fill(ndyn, |d| {
                    let name = d.node();
                    let value = d.node();
                    let pos = d.pos();
                    DynBindIr { name, value, pos }
                });
                if rec {
                    self.frames.pop();
                }
                let overrides = if rec {
                    let ov = self.ev.interner.intern(b"__overrides");
                    binds.iter().position(|b| b.sym == ov).map(|i| i as u32)
                } else {
                    None
                };
                Ir::Attrs(self.ev.arena.alloc(AttrsIr {
                    rec,
                    binds,
                    from,
                    dynamic,
                    pos,
                    overrides,
                }))
            }
            10 => Ir::List(self.nodes()),
            11 => {
                let param = if self.u8() == 0 {
                    self.frames.push(None);
                    ParamIr::Ident
                } else {
                    let has_bind = self.u8() != 0;
                    let ellipsis = self.u8() != 0;
                    let n = self.count();
                    let mut syms = Vec::with_capacity(n);
                    let mut poss = Vec::with_capacity(n);
                    for _ in 0..n {
                        syms.push(self.sym());
                        poss.push(self.pos());
                    }
                    let fperm = Self::perm(&syms, 0);
                    let off = u32::from(has_bind);
                    self.frames.push(fperm.as_ref().map(|fp| {
                        let mut perm = Vec::with_capacity(n + off as usize);
                        if has_bind {
                            perm.push(0);
                        }
                        perm.extend(fp.iter().map(|p| p + off));
                        perm
                    }));
                    let formals = self.fill(n, |_| FormalIr {
                        sym: Sym(0),
                        default: None,
                        pos: Pos::NONE,
                    });
                    // SAFETY: as in `binds`: a fresh slice, rewritten in place.
                    let fm = unsafe {
                        std::slice::from_raw_parts_mut(formals.as_ptr() as *mut FormalIr, n)
                    };
                    for i in 0..n {
                        let default = self.opt_node();
                        let new = fperm.as_ref().map_or(i, |p| p[i] as usize);
                        fm[new] = FormalIr {
                            sym: syms[i],
                            default,
                            pos: poss[i],
                        };
                    }
                    ParamIr::Formals {
                        has_bind,
                        formals,
                        ellipsis,
                    }
                };
                let body = self.node();
                self.frames.pop();
                let pos = self.pos();
                let name = self.opt_sym();
                let arg = self.opt_sym();
                let closed = self.u8() != 0;
                Ir::Lambda(self.ev.arena.alloc(LambdaIr {
                    param,
                    body,
                    pos,
                    name,
                    arg,
                    closed,
                }))
            }
            12 => {
                let func = self.node();
                let args = self.nodes();
                let pos = self.pos();
                Ir::Apply { func, args, pos }
            }
            13 => {
                let (binds, from) = self.binds(true);
                let body = self.node();
                self.frames.pop();
                Ir::Let(self.ev.arena.alloc(LetIr { binds, from, body }))
            }
            14 => {
                let scope = self.node();
                self.frames.push(None);
                let body = self.node();
                self.frames.pop();
                let pos = self.pos();
                Ir::With { scope, body, pos }
            }
            15 => {
                let cond = self.node();
                let then = self.node();
                let else_ = self.node();
                let pos = self.pos();
                Ir::If {
                    cond,
                    then,
                    else_,
                    pos,
                }
            }
            16 => {
                let cond = self.node();
                let body = self.node();
                let pos = self.pos();
                Ir::Assert { cond, body, pos }
            }
            17 => {
                let e = self.node();
                Ir::Not(e, self.pos())
            }
            18 => {
                let e = self.node();
                Ir::Neg(e, self.pos())
            }
            t @ 19..=32 => {
                let l = self.node();
                let r = self.node();
                let pos = self.pos();
                match t {
                    19 => Ir::Eq(l, r, pos),
                    20 => Ir::Ne(l, r, pos),
                    21 => Ir::Lt(l, r, pos),
                    22 => Ir::Gt(l, r, pos),
                    23 => Ir::Le(l, r, pos),
                    24 => Ir::Ge(l, r, pos),
                    25 => Ir::And(l, r, pos),
                    26 => Ir::Or(l, r, pos),
                    27 => Ir::Impl(l, r, pos),
                    28 => Ir::Update(l, r, pos),
                    29 => Ir::ConcatLists(l, r, pos),
                    30 => Ir::Sub(l, r, pos),
                    31 => Ir::Mul(l, r, pos),
                    _ => Ir::Div(l, r, pos),
                }
            }
            _ => {
                self.ok = false;
                Ir::Const(Word::NULL)
            }
        };
        self.ev.arena.alloc(ir)
    }
}

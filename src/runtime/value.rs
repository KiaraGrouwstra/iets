//! Heap objects. Every object starts with a one-byte `Kind` so a `Word` with
//! the pointer tag can be classified with one load. Variable-length objects
//! (lists, attrs, envs) keep their elements inline after a fixed header.

use super::arena::{Arena, Stat};
use super::word::{
    Cell, TAG_IMM, TAG_INT, TAG_LAMBDA, TAG_PTR, TAG_THUNK, TAG_THUNK_APP, TAG_THUNK_APP2, Word,
};
use crate::compile::ir::{Ir, LambdaIr};
use crate::eval::Pos;
use crate::syntax::symbol::Sym;

#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Int = 1,
    Float,
    String,
    Path,
    List,
    Attrs,
    Lambda,
    PrimOp,
    PrimOpApp,
    AttrsUnion,
    Env,
    Thunk,
    ThunkApp,
    ThunkApp2,
}

/// Type of a forced value, as Nix sees it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ValueType {
    Int,
    Float,
    Bool,
    Null,
    String,
    Path,
    List,
    Attrs,
    Function,
}

impl ValueType {
    pub fn name(self) -> &'static str {
        match self {
            ValueType::Int => "an integer",
            ValueType::Float => "a float",
            ValueType::Bool => "a Boolean",
            ValueType::Null => "null",
            ValueType::String => "a string",
            ValueType::Path => "a path",
            ValueType::List => "a list",
            ValueType::Attrs => "a set",
            ValueType::Function => "a function",
        }
    }

    pub fn type_of(self) -> &'static str {
        match self {
            ValueType::Int => "int",
            ValueType::Float => "float",
            ValueType::Bool => "bool",
            ValueType::Null => "null",
            ValueType::String => "string",
            ValueType::Path => "path",
            ValueType::List => "list",
            ValueType::Attrs => "set",
            ValueType::Function => "lambda",
        }
    }
}

#[repr(C)]
pub struct IntObj {
    pub kind: Kind,
    pub val: i64,
}

#[repr(C)]
pub struct FloatObj {
    pub kind: Kind,
    pub val: f64,
}

/// String context element. Ordered so contexts can be kept as sorted,
/// deduplicated slices and merged linearly.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub enum Ctx {
    /// A store path referenced as-is.
    Opaque(&'static [u8]),
    /// `=drv`: the whole derivation closure.
    DrvDeep(&'static [u8]),
    /// `!out!drv`: one output of a derivation.
    Built {
        drv: &'static [u8],
        output: &'static [u8],
    },
}

#[repr(C)]
pub struct StringObj {
    pub kind: Kind,
    /// Census only (`stats2`): set once the string was viewed.
    pub flags: u8,
    pub bytes: &'static [u8],
    pub ctx: &'static [Ctx],
}

#[repr(C)]
pub struct PathObj {
    pub kind: Kind,
    pub path: &'static [u8],
}

#[repr(C)]
pub struct ListObj {
    pub kind: Kind,
    /// Census only (`stats2`): set once the cells were read.
    pub flags: u8,
    pub len: u32,
    // followed by `len` Cells
}

#[repr(C)]
pub struct Attr {
    pub sym: Sym,
    pub pos: Pos,
    pub cell: Cell,
}

#[repr(C)]
pub struct AttrsObj {
    /// `Kind::Attrs`: `len` entries follow. `Kind::AttrsUnion`: a
    /// `UnionTail` follows and the entries are those of `b` over `a`.
    pub kind: Kind,
    /// Producer, see `census::ORIGINS`; written by every build.
    pub origin: u8,
    /// Census only (`stats2`): selections (low 7 bits, saturating) and
    /// bit 7 once the entries were iterated.
    pub flags: u8,
    /// Flat slices an iteration merges: 1 for a flat set.
    pub leaves: u8,
    /// Entry count; `u32::MAX` on a union whose count was not needed yet.
    pub len: std::sync::atomic::AtomicU32,
    // followed by `len` Attrs sorted by sym, or by a `UnionTail`
}

/// `a // b` without the copy: the flat leaves of both sides, lowest
/// priority first. Lookups probe the leaves from the last; an iteration
/// merges them; `flat` caches the copy once someone needs the entries as
/// one slice.
#[repr(C)]
pub struct UnionTail {
    pub flat: Cell,
    // followed by `leaves` pointers to flat sets
}

/// Leaves a union may hold before `//` flattens its left side first:
/// bounds a lookup miss to this many probes.
pub const MAX_LEAVES: u8 = 8;
/// Left operands with fewer entries are merged flat: the copy is cheaper
/// than the union plus an extra probe.
pub const UNION_MIN: usize = 8;

/// Flat leaves a set contributes to a union: itself, its cached flat
/// copy, or its own leaves.
#[inline]
pub fn for_each_leaf(a: &'static AttrsObj, mut f: impl FnMut(&'static AttrsObj)) {
    if a.kind == Kind::Attrs {
        f(a)
    } else if let Some(flat) = a.flattened() {
        f(flat)
    } else {
        for leaf in a.leaf_sets() {
            f(leaf)
        }
    }
}

#[inline]
pub fn leaf_count(a: &'static AttrsObj) -> usize {
    if a.kind == Kind::Attrs || a.flattened().is_some() {
        1
    } else {
        a.leaves as usize
    }
}

/// Envs are reached only through `up`, lambdas and thunks, never through a
/// `Word`, so they carry no `Kind`; slot indices come from the compiler, so
/// they carry no length either. Header is one word.
#[repr(C)]
pub struct EnvObj {
    pub up: *const EnvObj,
    // followed by the slots
}

/// Closures are the third most common object; `TAG_LAMBDA` on the word
/// replaces the Kind byte so the object is two words.
#[repr(C)]
pub struct LambdaObj {
    pub env: &'static EnvObj,
    pub lam: &'static LambdaIr,
}

pub type PrimFn =
    fn(&mut crate::eval::Evaluator, &[Word], Pos) -> Result<Word, Box<crate::eval::Error>>;

#[repr(C)]
pub struct PrimOpObj {
    pub kind: Kind,
    pub arity: u8,
    pub name: &'static str,
    pub f: PrimFn,
}

#[repr(C)]
pub struct PrimOpAppObj {
    pub kind: Kind,
    pub op: &'static PrimOpObj,
    pub args: &'static [Word],
}

/// A suspended `ir` in `env`. `result` is EMPTY until forced, BLACKHOLE while
/// being forced, then the forced value. Thunk objects carry no `Kind` byte:
/// the word tag (`TAG_THUNK` vs `TAG_THUNK_APP`) tells the two layouts apart.
#[repr(C)]
pub struct ThunkObj {
    pub result: Cell,
    pub env: &'static EnvObj,
    pub ir: &'static Ir,
}

/// A suspended application `func arg`, as created by `map`, `genList`, etc.
#[repr(C)]
pub struct ThunkAppObj {
    pub result: Cell,
    pub func: Word,
    pub arg: Word,
}

/// A suspended two-argument application `func a b`, as created by `mapAttrs`.
#[repr(C)]
pub struct ThunkApp2Obj {
    pub result: Cell,
    pub func: Word,
    pub arg1: Word,
    pub arg2: Word,
}

const _: () = assert!(size_of::<ListObj>() == 8);
const _: () = assert!(size_of::<AttrsObj>() == 8);
const _: () = assert!(size_of::<EnvObj>() == 8);
const _: () = assert!(size_of::<Attr>() == 16);
const _: () = assert!(size_of::<ThunkObj>() == 24);
const _: () = assert!(size_of::<ThunkAppObj>() == 24);
const _: () = assert!(size_of::<ThunkApp2Obj>() == 32);
const _: () = assert!(size_of::<LambdaObj>() == 16);

/// Census marks (`stats2`): a byte in header padding is written through a
/// raw pointer. Single-worker measurement runs only.
#[cfg(feature = "stats2")]
#[inline]
fn mark_byte(p: *const u8, f: impl Fn(u8) -> u8) {
    use std::sync::atomic::{AtomicU8, Ordering};
    // SAFETY: `p` is a flags byte inside a live arena object, viewed as an
    // atomic byte.
    let a = unsafe { &*(p as *const AtomicU8) };
    let _ = a.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| Some(f(v)));
}

impl ListObj {
    #[inline]
    pub fn cells(&self) -> &'static [Cell] {
        #[cfg(feature = "stats2")]
        mark_byte(&self.flags, |_| 1);
        // SAFETY: the allocator placed `len` cells directly after the header.
        unsafe {
            let p = (self as *const ListObj).add(1) as *const Cell;
            std::slice::from_raw_parts(p, self.len as usize)
        }
    }
}

impl AttrsObj {
    /// The entries as one sorted slice. A union must have been flattened
    /// (`Arena::flat`) by the caller; iteration without a copy is `iter()`.
    #[inline]
    pub fn attrs(&self) -> &'static [Attr] {
        #[cfg(feature = "stats2")]
        mark_byte(&self.flags, |f| f | 0x80);
        self.attrs_raw()
    }

    #[inline]
    pub fn attrs_raw(&self) -> &'static [Attr] {
        if self.kind == Kind::AttrsUnion {
            return self
                .flattened()
                .expect("attrs() on a union: flatten first")
                .attrs_raw();
        }
        // SAFETY: the allocator placed `len` attrs directly after the header.
        unsafe {
            let p = (self as *const AttrsObj).add(1) as *const Attr;
            std::slice::from_raw_parts(p, self.len_raw() as usize)
        }
    }

    #[inline]
    fn len_raw(&self) -> u32 {
        self.len.load(std::sync::atomic::Ordering::Relaxed)
    }

    #[inline]
    pub fn is_union(&self) -> bool {
        self.kind == Kind::AttrsUnion
    }

    /// The `UnionTail` of a union.
    #[inline]
    pub fn union(&self) -> &'static UnionTail {
        debug_assert!(self.is_union());
        // SAFETY: `alloc_union` placed the tail directly after the header.
        unsafe { &*((self as *const AttrsObj).add(1) as *const UnionTail) }
    }

    /// The flat sets a union merges, lowest priority first.
    #[inline]
    pub fn leaf_sets(&self) -> &'static [&'static AttrsObj] {
        debug_assert!(self.is_union());
        // SAFETY: `alloc_union` placed `leaves` pointers after the tail.
        unsafe {
            let p = (self.union() as *const UnionTail).add(1) as *const &'static AttrsObj;
            std::slice::from_raw_parts(p, self.leaves as usize)
        }
    }

    /// The cached flat copy of a union, once made.
    #[inline]
    pub fn flattened(&self) -> Option<&'static AttrsObj> {
        let w = self.union().flat.get();
        if w == Word::EMPTY {
            return None;
        }
        // SAFETY: `flat` holds a pointer to a flat arena set or EMPTY.
        Some(unsafe { &*w.ptr::<AttrsObj>() })
    }

    #[inline]
    pub fn len(&self) -> usize {
        let n = self.len_raw();
        if n != u32::MAX {
            return n as usize;
        }
        let n = self.iter().count();
        self.len
            .store(n as u32, std::sync::atomic::Ordering::Relaxed);
        n
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        // a union never holds two empty sides
        self.kind == Kind::Attrs && self.len_raw() == 0
    }

    /// Binary search over symbol ids; linear for small sets. On a union
    /// the leaves are probed from the last (the right side of the last
    /// `//`) without a copy.
    #[inline]
    pub fn get(&self, sym: Sym) -> Option<&'static Attr> {
        #[cfg(feature = "stats2")]
        mark_byte(&self.flags, |f| if f & 0x7f < 0x7f { f + 1 } else { f });
        if self.kind == Kind::Attrs {
            return self.get_flat(sym);
        }
        if let Some(flat) = self.flattened() {
            return flat.get_flat(sym);
        }
        for leaf in self.leaf_sets().iter().rev() {
            if let Some(a) = leaf.get_flat(sym) {
                return Some(a);
            }
        }
        None
    }

    #[inline]
    fn get_flat(&self, sym: Sym) -> Option<&'static Attr> {
        let attrs = self.attrs_raw();
        if attrs.len() <= 8 {
            attrs.iter().find(|a| a.sym == sym)
        } else {
            attrs
                .binary_search_by(|a| a.sym.cmp(&sym))
                .ok()
                .map(|i| &attrs[i])
        }
    }

    /// The entries in sym order, merging a union's leaves on the fly.
    #[inline]
    pub fn iter(&self) -> AttrIter {
        #[cfg(feature = "stats2")]
        mark_byte(&self.flags, |f| f | 0x80);
        let mut it = AttrIter {
            leaves: [&[]; MAX_LEAVES as usize],
            n: 1,
        };
        if self.kind == Kind::Attrs {
            it.leaves[0] = self.attrs_raw();
        } else if let Some(flat) = self.flattened() {
            it.leaves[0] = flat.attrs_raw();
        } else {
            let sets = self.leaf_sets();
            for (i, s) in sets.iter().enumerate() {
                it.leaves[i] = s.attrs_raw();
            }
            it.n = sets.len();
        }
        it
    }
}

/// Merging iterator over the flat leaves of a set, lowest priority first;
/// on equal syms the last leaf wins (`b` over `a`).
pub struct AttrIter {
    leaves: [&'static [Attr]; MAX_LEAVES as usize],
    n: usize,
}

impl Iterator for AttrIter {
    type Item = &'static Attr;

    #[inline]
    fn next(&mut self) -> Option<&'static Attr> {
        if self.n == 1 {
            let (first, rest) = self.leaves[0].split_first()?;
            self.leaves[0] = rest;
            return Some(first);
        }
        let mut best: Option<(Sym, usize)> = None;
        for (i, l) in self.leaves[..self.n].iter().enumerate() {
            if let Some(a) = l.first() {
                best = match best {
                    Some((s, _)) if s < a.sym => best,
                    _ => Some((a.sym, i)),
                };
            }
        }
        let (sym, win) = best?;
        let item = &self.leaves[win][0];
        for l in self.leaves[..self.n].iter_mut() {
            if l.first().is_some_and(|a| a.sym == sym) {
                *l = &l[1..];
            }
        }
        Some(item)
    }
}

impl EnvObj {
    /// Slot `i`. The compiler resolved `i` against this env's binder, so it
    /// is in range by construction.
    #[inline]
    pub fn slot(&self, i: usize) -> &'static Cell {
        // SAFETY: `alloc_env` placed the slots directly after the header and
        // every index the evaluator uses was resolved at compile time.
        unsafe { &*((self as *const EnvObj).add(1) as *const Cell).add(i) }
    }

    /// The first `n` slots, for filling a fresh env of `n` slots.
    #[inline]
    pub fn slots_n(&self, n: usize) -> &'static [Cell] {
        // SAFETY: as for `slot`; the caller passes the `n` it allocated.
        unsafe {
            let p = (self as *const EnvObj).add(1) as *const Cell;
            std::slice::from_raw_parts(p, n)
        }
    }

    #[inline]
    pub fn up(&self) -> Option<&'static EnvObj> {
        #[cfg(feature = "stats2")]
        let up = ((self.up as usize) & !7) as *const EnvObj;
        #[cfg(not(feature = "stats2"))]
        let up = self.up;
        // SAFETY: `up` is either null or an arena env that lives forever.
        unsafe { up.as_ref() }
    }

    /// Census (`stats2`): a slot of this env was read.
    #[inline]
    pub fn mark_read(&self) {
        #[cfg(feature = "stats2")]
        self.mark_bit(1);
    }

    /// Census (`stats2`): a thunk, closure or child env holds this env.
    #[inline]
    pub fn mark_captured(&self) {
        #[cfg(feature = "stats2")]
        self.mark_bit(2);
    }

    #[cfg(feature = "stats2")]
    #[inline]
    fn mark_bit(&self, bit: usize) {
        // SAFETY: `up` is a pointer-sized, pointer-aligned word viewed as an
        // atomic; the low bits are free (8-byte aligned) and masked by `up()`.
        unsafe {
            let a = &*(&self.up as *const *const EnvObj as *const std::sync::atomic::AtomicUsize);
            a.fetch_or(bit, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

/// Forced-value view of a `Word`.
pub enum Value {
    Int(i64),
    Float(f64),
    Bool(bool),
    Null,
    String(&'static StringObj),
    Path(&'static PathObj),
    List(&'static ListObj),
    Attrs(&'static AttrsObj),
    Lambda(&'static LambdaObj),
    PrimOp(&'static PrimOpObj),
    PrimOpApp(&'static PrimOpAppObj),
}

impl Word {
    /// Classify a forced word. Panics on thunks and EMPTY: callers force first.
    #[inline]
    pub fn view(self) -> Value {
        match self.tag() {
            TAG_INT => Value::Int(self.imm_int_value()),
            TAG_IMM => match self {
                Word::NULL => Value::Null,
                Word::TRUE => Value::Bool(true),
                Word::FALSE => Value::Bool(false),
                _ => panic!("view: blackhole"),
            },
            TAG_THUNK | TAG_THUNK_APP | TAG_THUNK_APP2 => panic!("view: unforced thunk"),
            // SAFETY: lambda-tagged words point at an arena LambdaObj.
            TAG_LAMBDA => Value::Lambda(unsafe { &*self.ptr::<LambdaObj>() }),
            _ => {
                debug_assert!(self.0 != 0, "view: empty word");
                // SAFETY: pointer-tagged words always point at an arena
                // object that starts with a Kind byte.
                unsafe {
                    let p: *const u8 = self.ptr();
                    match *(p as *const Kind) {
                        Kind::Int => Value::Int((*(p as *const IntObj)).val),
                        Kind::Float => Value::Float((*(p as *const FloatObj)).val),
                        Kind::String => {
                            #[cfg(feature = "stats2")]
                            mark_byte(&(*(p as *const StringObj)).flags, |_| 1);
                            Value::String(&*(p as *const StringObj))
                        }
                        Kind::Path => Value::Path(&*(p as *const PathObj)),
                        Kind::List => Value::List(&*(p as *const ListObj)),
                        Kind::Attrs | Kind::AttrsUnion => Value::Attrs(&*(p as *const AttrsObj)),
                        Kind::PrimOp => Value::PrimOp(&*(p as *const PrimOpObj)),
                        Kind::PrimOpApp => Value::PrimOpApp(&*(p as *const PrimOpAppObj)),
                        k => panic!("view: not a value object: {k:?}"),
                    }
                }
            }
        }
    }

    #[inline]
    pub fn value_type(self) -> ValueType {
        match self.view() {
            Value::Int(_) => ValueType::Int,
            Value::Float(_) => ValueType::Float,
            Value::Bool(_) => ValueType::Bool,
            Value::Null => ValueType::Null,
            Value::String(_) => ValueType::String,
            Value::Path(_) => ValueType::Path,
            Value::List(_) => ValueType::List,
            Value::Attrs(_) => ValueType::Attrs,
            Value::Lambda(_) | Value::PrimOp(_) | Value::PrimOpApp(_) => ValueType::Function,
        }
    }

    #[inline]
    pub fn heap_kind(self) -> Option<Kind> {
        if self.tag() == TAG_LAMBDA {
            return Some(Kind::Lambda);
        }
        if self.tag() == TAG_PTR && self.0 != 0 {
            // SAFETY: see `view`.
            Some(unsafe { *(self.ptr::<u8>() as *const Kind) })
        } else {
            None
        }
    }

    #[inline]
    pub fn as_attrs(self) -> Option<&'static AttrsObj> {
        match self.view() {
            Value::Attrs(a) => Some(a),
            _ => None,
        }
    }

    #[inline]
    pub fn as_list(self) -> Option<&'static ListObj> {
        match self.view() {
            Value::List(l) => Some(l),
            _ => None,
        }
    }

    #[inline]
    pub fn as_string(self) -> Option<&'static StringObj> {
        match self.view() {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    #[inline]
    pub fn as_int(self) -> Option<i64> {
        match self.view() {
            Value::Int(i) => Some(i),
            _ => None,
        }
    }

    /// Which thunk layout a THUNK-tagged word points at.
    #[inline]
    pub fn thunk_kind(self) -> Kind {
        debug_assert!(self.is_thunk());
        match self.tag() {
            TAG_THUNK_APP => Kind::ThunkApp,
            TAG_THUNK_APP2 => Kind::ThunkApp2,
            _ => Kind::Thunk,
        }
    }

    #[inline]
    pub fn thunk_result_cell(self) -> &'static Cell {
        // SAFETY: both thunk layouts place `result` right after `kind`.
        unsafe { &(*self.ptr::<ThunkObj>()).result }
    }

    #[inline]
    pub fn as_thunk(self) -> &'static ThunkObj {
        // SAFETY: caller checked `thunk_kind() == Kind::Thunk`.
        unsafe { &*self.ptr::<ThunkObj>() }
    }

    #[inline]
    pub fn as_thunk_app(self) -> &'static ThunkAppObj {
        // SAFETY: caller checked `thunk_kind() == Kind::ThunkApp`.
        unsafe { &*self.ptr::<ThunkAppObj>() }
    }

    #[inline]
    pub fn as_thunk_app2(self) -> &'static ThunkApp2Obj {
        // SAFETY: caller checked `thunk_kind() == Kind::ThunkApp2`.
        unsafe { &*self.ptr::<ThunkApp2Obj>() }
    }
}

// Constructors.

impl Arena {
    #[inline]
    pub fn mk_int(&mut self, v: i64) -> Word {
        if Word::fits_imm(v) {
            Word::imm_int(v)
        } else {
            self.stats.note(Stat::BigInt, size_of::<IntObj>());
            Word::from_ptr(self.alloc(IntObj {
                kind: Kind::Int,
                val: v,
            }))
        }
    }

    #[inline]
    pub fn mk_float(&mut self, v: f64) -> Word {
        self.stats.note(Stat::Float, size_of::<FloatObj>());
        Word::from_ptr(self.alloc(FloatObj {
            kind: Kind::Float,
            val: v,
        }))
    }

    pub fn mk_string_static(&mut self, bytes: &'static [u8], ctx: &'static [Ctx]) -> Word {
        self.stats.note(Stat::StrObj, size_of::<StringObj>());
        let s = self.alloc(StringObj {
            kind: Kind::String,
            flags: 0,
            bytes,
            ctx,
        });
        #[cfg(feature = "stats2")]
        {
            self.stats
                .census
                .note_string(s as *const StringObj, bytes, size_of_val(ctx));
            self.stats.census.note_ctx(ctx);
        }
        Word::from_ptr(s)
    }

    pub fn mk_string(&mut self, bytes: &[u8]) -> Word {
        self.stats.note(Stat::Bytes, bytes.len());
        let b = self.alloc_bytes(bytes);
        self.mk_string_static(b, &[])
    }

    pub fn mk_string_ctx(&mut self, bytes: &[u8], ctx: &[Ctx]) -> Word {
        self.stats.note(Stat::Bytes, bytes.len());
        self.stats.note(Stat::Ctx, size_of_val(ctx));
        let b = self.alloc_bytes(bytes);
        let c = self.alloc_slice(ctx);
        self.mk_string_static(b, c)
    }

    pub fn mk_path(&mut self, path: &[u8]) -> Word {
        self.stats
            .note(Stat::Path, size_of::<PathObj>() + path.len());
        let p = self.alloc_bytes(path);
        Word::from_ptr(self.alloc(PathObj {
            kind: Kind::Path,
            path: p,
        }))
    }

    /// Allocate a list of `n` cells, all EMPTY. Fill through `cells()`.
    pub fn alloc_list(&mut self, n: usize) -> &'static ListObj {
        self.alloc_list_as(n, Stat::List)
    }

    /// `alloc_list` with a stats bucket naming the producer.
    pub fn alloc_list_as(&mut self, n: usize, bucket: Stat) -> &'static ListObj {
        self.stats
            .note(bucket, size_of::<ListObj>() + n * size_of::<Cell>());
        let p = self.alloc_raw(size_of::<ListObj>() + n * size_of::<Cell>(), 8) as *mut ListObj;
        // SAFETY: fresh allocation sized for header plus n cells.
        unsafe {
            p.write(ListObj {
                kind: Kind::List,
                flags: 0,
                len: n as u32,
            });
            let cells = p.add(1) as *mut u64;
            std::ptr::write_bytes(cells, 0, n);
            #[cfg(feature = "stats2")]
            self.stats.census.note_list(p);
            &*p
        }
    }

    pub fn mk_list(&mut self, items: &[Word]) -> Word {
        self.mk_list_as(items, Stat::List)
    }

    pub fn mk_list_as(&mut self, items: &[Word], bucket: Stat) -> Word {
        let l = self.alloc_list_as(items.len(), bucket);
        // SAFETY: `alloc_list_as` placed `len` cells after the header; this
        // is the fill, not a read, so the census read mark stays clear.
        let cells = unsafe {
            std::slice::from_raw_parts((l as *const ListObj).add(1) as *const Cell, items.len())
        };
        for (c, w) in cells.iter().zip(items) {
            c.set(*w);
        }
        Word::from_ptr(l)
    }

    /// Allocate attrs with `n` uninitialised entries. Caller fills them in
    /// sorted order through `attrs_mut`.
    pub fn alloc_attrs(&mut self, n: usize) -> &'static mut AttrsObj {
        self.alloc_attrs_as(n, Stat::Attrs)
    }

    /// `alloc_attrs` with a stats bucket naming the producer.
    pub fn alloc_attrs_as(&mut self, n: usize, bucket: Stat) -> &'static mut AttrsObj {
        self.stats
            .note(bucket, size_of::<AttrsObj>() + n * size_of::<Attr>());
        let p = self.alloc_raw(size_of::<AttrsObj>() + n * size_of::<Attr>(), 8) as *mut AttrsObj;
        // SAFETY: fresh allocation sized for header plus n attrs.
        unsafe {
            p.write(AttrsObj {
                kind: Kind::Attrs,
                origin: bucket.attrs_origin(),
                flags: 0,
                leaves: 1,
                len: std::sync::atomic::AtomicU32::new(n as u32),
            });
            #[cfg(feature = "stats2")]
            self.stats.census.note_attrs(p, bucket.attrs_origin(), n);
            &mut *p
        }
    }

    /// `a // b` as a union over the flat leaves of both sides; the caller
    /// checked the leaf budget (`leaf_count`).
    pub fn alloc_union(&mut self, a: &'static AttrsObj, b: &'static AttrsObj) -> Word {
        let n = leaf_count(a) + leaf_count(b);
        debug_assert!(n <= MAX_LEAVES as usize);
        let size = size_of::<AttrsObj>() + size_of::<UnionTail>() + n * size_of::<&AttrsObj>();
        self.stats.note(Stat::AttrsUpd, size);
        let p = self.alloc_raw(size, 8) as *mut AttrsObj;
        // SAFETY: fresh allocation sized for header, tail and n pointers.
        unsafe {
            p.write(AttrsObj {
                kind: Kind::AttrsUnion,
                origin: Stat::AttrsUpd.attrs_origin(),
                flags: 0,
                leaves: n as u8,
                len: std::sync::atomic::AtomicU32::new(u32::MAX),
            });
            let tail = p.add(1) as *mut UnionTail;
            tail.write(UnionTail {
                flat: Cell::new(Word::EMPTY),
            });
            let mut leaves = tail.add(1) as *mut &'static AttrsObj;
            for side in [a, b] {
                for_each_leaf(side, |leaf| {
                    leaves.write(leaf);
                    leaves = leaves.add(1);
                });
            }
            #[cfg(feature = "stats2")]
            self.stats
                .census
                .note_attrs(p, Stat::AttrsUpd.attrs_origin(), a.len() + b.len());
            Word::from_ptr(p as *const AttrsObj)
        }
    }

    /// The set as one flat slice: itself for a flat set, else the cached
    /// or freshly merged copy of the union.
    pub fn flat(&mut self, a: &'static AttrsObj) -> &'static AttrsObj {
        if a.kind == Kind::Attrs {
            return a;
        }
        let u = a.union();
        if let Some(f) = a.flattened() {
            return f;
        }
        let n = a.len();
        let out = self.alloc_attrs_as(n, Stat::AttrsUpd);
        for (i, e) in a.iter().enumerate() {
            out.init_from(i, e);
        }
        let out: &'static AttrsObj = out;
        u.flat.set(Word::from_ptr(out as *const AttrsObj));
        out
    }

    /// Build attrs from `(sym, pos, word)` triples; sorts and rejects duplicates
    /// (returns the duplicate sym).
    pub fn mk_attrs(&mut self, entries: &mut [(Sym, Pos, Word)]) -> Result<Word, Sym> {
        entries.sort_by_key(|e| e.0);
        for w in entries.windows(2) {
            if w[0].0 == w[1].0 {
                return Err(w[0].0);
            }
        }
        Ok(self.mk_attrs_sorted(entries))
    }

    /// Entries must already be sorted by sym and unique.
    pub fn mk_attrs_sorted(&mut self, entries: &[(Sym, Pos, Word)]) -> Word {
        self.mk_attrs_sorted_as(entries, Stat::Attrs)
    }

    pub fn mk_attrs_sorted_as(&mut self, entries: &[(Sym, Pos, Word)], bucket: Stat) -> Word {
        let a = self.alloc_attrs_as(entries.len(), bucket);
        let slots = a.attrs_mut();
        for (slot, (sym, pos, w)) in slots.iter_mut().zip(entries) {
            // SAFETY: writing into uninitialised arena memory of the right type.
            unsafe {
                std::ptr::write(
                    slot,
                    Attr {
                        sym: *sym,
                        pos: *pos,
                        cell: Cell::new(*w),
                    },
                )
            };
        }
        Word::from_ptr(a as *const AttrsObj)
    }

    pub fn alloc_env(&mut self, up: Option<&'static EnvObj>, n: usize) -> &'static EnvObj {
        self.stats
            .note(Stat::Env, size_of::<EnvObj>() + n * size_of::<Cell>());
        #[cfg(feature = "stats2")]
        if let Some(u) = up {
            u.mark_captured();
        }
        let p = self.alloc_raw(size_of::<EnvObj>() + n * size_of::<Cell>(), 8) as *mut EnvObj;
        // SAFETY: fresh allocation sized for header plus n cells.
        unsafe {
            p.write(EnvObj {
                up: up.map_or(std::ptr::null(), |e| e as *const EnvObj),
            });
            let cells = p.add(1) as *mut u64;
            std::ptr::write_bytes(cells, 0, n);
            #[cfg(feature = "stats2")]
            self.stats.census.note_env(p, n);
            &*p
        }
    }

    #[inline]
    pub fn mk_thunk(&mut self, env: &'static EnvObj, ir: &'static Ir) -> Word {
        self.stats.note(Stat::Thunk, size_of::<ThunkObj>());
        env.mark_captured();
        let t = self.alloc(ThunkObj {
            result: Cell::new(Word::EMPTY),
            env,
            ir,
        });
        Word::thunk_from_ptr(t as *const ThunkObj)
    }

    #[inline]
    pub fn mk_thunk_app(&mut self, func: Word, arg: Word) -> Word {
        self.stats.note(Stat::ThunkApp, size_of::<ThunkAppObj>());
        let t = self.alloc(ThunkAppObj {
            result: Cell::new(Word::EMPTY),
            func,
            arg,
        });
        Word::thunk_app_from_ptr(t as *const ThunkAppObj)
    }

    #[inline]
    pub fn mk_thunk_app2(&mut self, func: Word, arg1: Word, arg2: Word) -> Word {
        self.stats.note(Stat::ThunkApp2, size_of::<ThunkApp2Obj>());
        let t = self.alloc(ThunkApp2Obj {
            result: Cell::new(Word::EMPTY),
            func,
            arg1,
            arg2,
        });
        Word::thunk_app2_from_ptr(t as *const ThunkApp2Obj)
    }

    pub fn mk_lambda(&mut self, env: &'static EnvObj, lam: &'static LambdaIr) -> Word {
        self.stats.note(Stat::Lambda, size_of::<LambdaObj>());
        env.mark_captured();
        #[cfg(feature = "stats2")]
        {
            self.stats.census.lambda_closed += lam.closed as u64;
        }
        Word::lambda_from_ptr(self.alloc(LambdaObj { env, lam }) as *const LambdaObj)
    }

    pub fn mk_primop(&mut self, name: &'static str, arity: u8, f: PrimFn) -> Word {
        Word::from_ptr(self.alloc(PrimOpObj {
            kind: Kind::PrimOp,
            arity,
            name,
            f,
        }))
    }

    pub fn mk_primop_app(&mut self, op: &'static PrimOpObj, args: &[Word]) -> Word {
        self.stats.note(
            Stat::PrimOpApp,
            size_of::<PrimOpAppObj>() + size_of_val(args),
        );
        let args = self.alloc_slice(args);
        Word::from_ptr(self.alloc(PrimOpAppObj {
            kind: Kind::PrimOpApp,
            op,
            args,
        }))
    }
}

impl AttrsObj {
    #[inline]
    pub fn attrs_mut(&mut self) -> &'static mut [Attr] {
        // SAFETY: see `attrs`; exclusive access through `&mut self`.
        unsafe {
            let p = (self as *mut AttrsObj).add(1) as *mut Attr;
            std::slice::from_raw_parts_mut(p, self.len_raw() as usize)
        }
    }
}

/// Merge two sorted, deduplicated context slices.
pub fn merge_ctx(arena: &mut Arena, a: &'static [Ctx], b: &'static [Ctx]) -> &'static [Ctx] {
    if a.is_empty() {
        return b;
    }
    if b.is_empty() {
        return a;
    }
    let mut out: Vec<Ctx> = Vec::with_capacity(a.len() + b.len());
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => {
                out.push(a[i]);
                i += 1;
            }
            std::cmp::Ordering::Greater => {
                out.push(b[j]);
                j += 1;
            }
            std::cmp::Ordering::Equal => {
                out.push(a[i]);
                i += 1;
                j += 1;
            }
        }
    }
    out.extend_from_slice(&a[i..]);
    out.extend_from_slice(&b[j..]);
    arena.alloc_slice(&out)
}

/// Sort and dedup a context vector, then move it to the arena.
pub fn ctx_to_arena(arena: &mut Arena, mut v: Vec<Ctx>) -> &'static [Ctx] {
    if v.is_empty() {
        return &[];
    }
    v.sort();
    v.dedup();
    arena.alloc_slice(&v)
}

impl AttrsObj {
    /// Write entry `i` of a set from `alloc_attrs`. Callers fill every entry
    /// in sorted order before handing the set out.
    #[inline]
    pub fn init(&mut self, i: usize, sym: Sym, pos: Pos, w: Word) {
        debug_assert!(i < self.len_raw() as usize);
        // SAFETY: `alloc_attrs` reserved `len` entries after the header;
        // `write` does not read the uninitialised slot.
        unsafe {
            let p = (self as *mut AttrsObj).add(1) as *mut Attr;
            std::ptr::write(
                p.add(i),
                Attr {
                    sym,
                    pos,
                    cell: Cell::new(w),
                },
            )
        }
    }

    #[inline]
    pub fn init_from(&mut self, i: usize, a: &Attr) {
        self.init(i, a.sym, a.pos, a.cell.get())
    }
}

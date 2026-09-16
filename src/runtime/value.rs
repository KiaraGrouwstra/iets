//! Heap objects. Every object starts with a one-byte `Kind` so a `Word` with
//! the pointer tag can be classified with one load. Variable-length objects
//! (lists, attrs, envs) keep their elements inline after a fixed header.

use super::arena::{Arena, Stat};
use super::word::{Cell, TAG_IMM, TAG_INT, TAG_PTR, TAG_THUNK, TAG_THUNK_APP, TAG_THUNK_APP2, Word};
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
    pub kind: Kind,
    pub len: u32,
    // followed by `len` Attrs sorted by sym
}

#[repr(C)]
pub struct EnvObj {
    pub kind: Kind,
    pub len: u32,
    pub up: *const EnvObj,
    // followed by `len` Cells
}

#[repr(C)]
pub struct LambdaObj {
    pub kind: Kind,
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
const _: () = assert!(size_of::<EnvObj>() == 16);
const _: () = assert!(size_of::<Attr>() == 16);
const _: () = assert!(size_of::<ThunkObj>() == 24);
const _: () = assert!(size_of::<ThunkAppObj>() == 24);
const _: () = assert!(size_of::<ThunkApp2Obj>() == 32);

impl ListObj {
    #[inline]
    pub fn cells(&self) -> &'static [Cell] {
        // SAFETY: the allocator placed `len` cells directly after the header.
        unsafe {
            let p = (self as *const ListObj).add(1) as *const Cell;
            std::slice::from_raw_parts(p, self.len as usize)
        }
    }
}

impl AttrsObj {
    #[inline]
    pub fn attrs(&self) -> &'static [Attr] {
        // SAFETY: the allocator placed `len` attrs directly after the header.
        unsafe {
            let p = (self as *const AttrsObj).add(1) as *const Attr;
            std::slice::from_raw_parts(p, self.len as usize)
        }
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.len as usize
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Binary search over symbol ids; linear for small sets.
    #[inline]
    pub fn get(&self, sym: Sym) -> Option<&'static Attr> {
        let attrs = self.attrs();
        if attrs.len() <= 8 {
            attrs.iter().find(|a| a.sym == sym)
        } else {
            attrs
                .binary_search_by(|a| a.sym.cmp(&sym))
                .ok()
                .map(|i| &attrs[i])
        }
    }
}

impl EnvObj {
    #[inline]
    pub fn slots(&self) -> &'static [Cell] {
        // SAFETY: the allocator placed `len` cells directly after the header.
        unsafe {
            let p = (self as *const EnvObj).add(1) as *const Cell;
            std::slice::from_raw_parts(p, self.len as usize)
        }
    }

    #[inline]
    pub fn up(&self) -> Option<&'static EnvObj> {
        // SAFETY: `up` is either null or an arena env that lives forever.
        unsafe { self.up.as_ref() }
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
            _ => {
                debug_assert!(self.0 != 0, "view: empty word");
                // SAFETY: pointer-tagged words always point at an arena
                // object that starts with a Kind byte.
                unsafe {
                    let p: *const u8 = self.ptr();
                    match *(p as *const Kind) {
                        Kind::Int => Value::Int((*(p as *const IntObj)).val),
                        Kind::Float => Value::Float((*(p as *const FloatObj)).val),
                        Kind::String => Value::String(&*(p as *const StringObj)),
                        Kind::Path => Value::Path(&*(p as *const PathObj)),
                        Kind::List => Value::List(&*(p as *const ListObj)),
                        Kind::Attrs => Value::Attrs(&*(p as *const AttrsObj)),
                        Kind::Lambda => Value::Lambda(&*(p as *const LambdaObj)),
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
            Word::from_ptr(self.alloc(IntObj {
                kind: Kind::Int,
                val: v,
            }))
        }
    }

    #[inline]
    pub fn mk_float(&mut self, v: f64) -> Word {
        Word::from_ptr(self.alloc(FloatObj {
            kind: Kind::Float,
            val: v,
        }))
    }

    pub fn mk_string_static(&mut self, bytes: &'static [u8], ctx: &'static [Ctx]) -> Word {
        self.stats.note(Stat::StrObj, size_of::<StringObj>());
        Word::from_ptr(self.alloc(StringObj {
            kind: Kind::String,
            bytes,
            ctx,
        }))
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
        let p = self.alloc_bytes(path);
        Word::from_ptr(self.alloc(PathObj {
            kind: Kind::Path,
            path: p,
        }))
    }

    /// Allocate a list of `n` cells, all EMPTY. Fill through `cells()`.
    pub fn alloc_list(&mut self, n: usize) -> &'static ListObj {
        self.stats.note(Stat::List, size_of::<ListObj>() + n * size_of::<Cell>());
        let p = self.alloc_raw(size_of::<ListObj>() + n * size_of::<Cell>(), 8) as *mut ListObj;
        // SAFETY: fresh allocation sized for header plus n cells.
        unsafe {
            p.write(ListObj {
                kind: Kind::List,
                len: n as u32,
            });
            let cells = p.add(1) as *mut u64;
            std::ptr::write_bytes(cells, 0, n);
            &*p
        }
    }

    pub fn mk_list(&mut self, items: &[Word]) -> Word {
        let l = self.alloc_list(items.len());
        for (c, w) in l.cells().iter().zip(items) {
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
        self.stats.note(bucket, size_of::<AttrsObj>() + n * size_of::<Attr>());
        let p = self.alloc_raw(size_of::<AttrsObj>() + n * size_of::<Attr>(), 8) as *mut AttrsObj;
        // SAFETY: fresh allocation sized for header plus n attrs.
        unsafe {
            p.write(AttrsObj {
                kind: Kind::Attrs,
                len: n as u32,
            });
            &mut *p
        }
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
        let a = self.alloc_attrs(entries.len());
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
        self.stats.note(Stat::Env, size_of::<EnvObj>() + n * size_of::<Cell>());
        let p = self.alloc_raw(size_of::<EnvObj>() + n * size_of::<Cell>(), 8) as *mut EnvObj;
        // SAFETY: fresh allocation sized for header plus n cells.
        unsafe {
            p.write(EnvObj {
                kind: Kind::Env,
                len: n as u32,
                up: up.map_or(std::ptr::null(), |e| e as *const EnvObj),
            });
            let cells = p.add(1) as *mut u64;
            std::ptr::write_bytes(cells, 0, n);
            &*p
        }
    }

    #[inline]
    pub fn mk_thunk(&mut self, env: &'static EnvObj, ir: &'static Ir) -> Word {
        self.stats.note(Stat::Thunk, size_of::<ThunkObj>());
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
        self.stats.note(Stat::ThunkApp, size_of::<ThunkApp2Obj>());
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
        Word::from_ptr(self.alloc(LambdaObj {
            kind: Kind::Lambda,
            env,
            lam,
        }))
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
        self.stats.note(Stat::PrimOpApp, size_of::<PrimOpAppObj>() + size_of_val(args));
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
            std::slice::from_raw_parts_mut(p, self.len as usize)
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
        debug_assert!(i < self.len as usize);
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

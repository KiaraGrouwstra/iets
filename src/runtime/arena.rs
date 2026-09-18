//! Bump allocator. Memory is never freed: the evaluator's whole heap lives
//! until process exit, so every allocation hands out a `'static` reference.
//! That is what makes graph sharing between cells trivially sound.

use std::ptr;

use super::word::Word;
use crate::syntax::symbol::{FxHashMap, FxHashSet};

const CHUNK: usize = 32 << 20;
/// A 32 MiB chunk populates in ~1 ms with huge pages and ~16 ms with 4 KiB
/// pages; anything slower than this is a compaction stall.
const THP_STALL: std::time::Duration = std::time::Duration::from_millis(6);

pub struct Arena {
    ptr: *mut u8,
    end: *mut u8,
    pub bytes_allocated: usize,
    /// Total chunk bytes mapped (and populated) so far.
    pub mapped: usize,
    pub stats: Stats,
    /// Advise huge pages until a chunk populate stalls in compaction.
    thp: bool,
    /// String bytes by content, and the context-free string object over
    /// them once one was made (EMPTY until then). nixpkgs builds the same
    /// text millions of times (docs/memory.md).
    pub strings: FxHashMap<u64, StrEntry>,
    /// Context slices by content.
    pub ctxs: FxHashSet<&'static [super::value::Ctx]>,
}

/// Position of the bump pointer, to give back the last allocation.
#[derive(Clone, Copy)]
pub struct Mark {
    ptr: *mut u8,
    end: *mut u8,
    bytes: usize,
}

impl Default for Arena {
    fn default() -> Self {
        Self::new()
    }
}

impl Arena {
    pub fn new() -> Self {
        Arena {
            ptr: ptr::null_mut(),
            end: ptr::null_mut(),
            bytes_allocated: 0,
            mapped: 0,
            stats: Stats::default(),
            thp: true,
            strings: FxHashMap::with_capacity_and_hasher(1 << 16, Default::default()),
            ctxs: FxHashSet::default(),
        }
    }

    #[inline]
    pub fn mark(&self) -> Mark {
        Mark {
            ptr: self.ptr,
            end: self.end,
            bytes: self.bytes_allocated,
        }
    }

    /// Give back everything allocated since `m`, when it is still the
    /// tail of the current chunk. The caller holds no reference to it.
    #[inline]
    pub fn release(&mut self, m: Mark) {
        if self.end == m.end {
            self.ptr = m.ptr;
            self.bytes_allocated = m.bytes;
        }
    }

    #[inline]
    pub fn alloc_raw(&mut self, size: usize, align: usize) -> *mut u8 {
        debug_assert!(align <= 16);
        let start = (self.ptr as usize + align - 1) & !(align - 1);
        let new_end = start + size;
        if new_end > self.end as usize || self.ptr.is_null() {
            return self.alloc_slow(size, align);
        }
        self.ptr = new_end as *mut u8;
        self.bytes_allocated += size;
        start as *mut u8
    }

    #[cold]
    fn alloc_slow(&mut self, size: usize, align: usize) -> *mut u8 {
        let chunk = CHUNK.max((size + align + CHUNK - 1) & !(CHUNK - 1));
        // Anonymous mmap, prefaulted in one syscall. Transparent huge pages
        // were tried first: on a fragmented box every huge fault that misses
        // a free 2 MiB block stalls in direct compaction and then falls back
        // to 4 KiB pages anyway, which cost ~1 s of system time per GiB.
        // MADV_POPULATE_WRITE zeroes the chunk inside the kernel without one
        // trap per page. The chunk is intentionally leaked.
        let base = unsafe {
            let p = libc::mmap(
                std::ptr::null_mut(),
                chunk,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            );
            assert!(p != libc::MAP_FAILED, "arena: mmap failed");
            let t0 = std::time::Instant::now();
            if self.thp {
                libc::madvise(p, chunk, libc::MADV_HUGEPAGE);
            }
            libc::madvise(p, chunk, libc::MADV_POPULATE_WRITE);
            if self.thp && t0.elapsed() > THP_STALL {
                self.thp = false;
            }
            p as *mut u8
        };
        self.mapped += chunk;
        self.ptr = base;
        self.end = unsafe { base.add(chunk) };
        let start = (self.ptr as usize + align - 1) & !(align - 1);
        self.ptr = (start + size) as *mut u8;
        self.bytes_allocated += size;
        start as *mut u8
    }

    #[inline]
    pub fn alloc<T>(&mut self, v: T) -> &'static mut T {
        let p = self.alloc_raw(size_of::<T>(), align_of::<T>()) as *mut T;
        // SAFETY: p is fresh, aligned, and sized for T.
        unsafe {
            p.write(v);
            &mut *p
        }
    }

    #[inline]
    pub fn alloc_slice<T: Copy>(&mut self, items: &[T]) -> &'static mut [T] {
        if items.is_empty() {
            return &mut [];
        }
        let p = self.alloc_raw(size_of_val(items), align_of::<T>()) as *mut T;
        // SAFETY: p is fresh and sized for items.len() elements of T.
        unsafe {
            ptr::copy_nonoverlapping(items.as_ptr(), p, items.len());
            std::slice::from_raw_parts_mut(p, items.len())
        }
    }

    /// Move the elements of `v` into one arena slice.
    pub fn alloc_vec<T>(&mut self, v: Vec<T>) -> &'static [T] {
        let n = v.len();
        if n == 0 {
            return &[];
        }
        let p = self.alloc_raw(n * size_of::<T>(), align_of::<T>()) as *mut T;
        // SAFETY: fresh allocation of n elements; values moved in.
        unsafe {
            for (i, e) in v.into_iter().enumerate() {
                p.add(i).write(e);
            }
            std::slice::from_raw_parts(p, n)
        }
    }

    #[inline]
    pub fn alloc_bytes(&mut self, b: &[u8]) -> &'static [u8] {
        self.alloc_slice(b)
    }

    pub fn alloc_str(&mut self, s: &str) -> &'static str {
        // SAFETY: bytes copied from a valid str.
        unsafe { std::str::from_utf8_unchecked(self.alloc_bytes(s.as_bytes())) }
    }

    /// Concatenate byte slices into one arena allocation.
    pub fn alloc_concat(&mut self, parts: &[&[u8]]) -> &'static [u8] {
        let len: usize = parts.iter().map(|p| p.len()).sum();
        if len == 0 {
            return &[];
        }
        let p = self.alloc_raw(len, 1);
        let mut off = 0;
        for part in parts {
            // SAFETY: total length was computed from the same parts.
            unsafe { ptr::copy_nonoverlapping(part.as_ptr(), p.add(off), part.len()) };
            off += part.len();
        }
        // SAFETY: fully initialised above.
        unsafe { std::slice::from_raw_parts(p, len) }
    }
}

/// Allocation kind for `IETSP_STATS`.
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
pub enum Stat {
    Thunk,
    ThunkApp,
    ThunkApp2,
    Env,
    ListLit,
    ListMap,
    ListGen,
    ListConcat,
    List,
    Attrs,
    AttrsLit,
    AttrsUpd,
    AttrsMap,
    AttrsListTo,
    AttrsRemove,
    AttrsZip,
    AttrsInter,
    StrObj,
    Bytes,
    Ctx,
    Path,
    Float,
    BigInt,
    Lambda,
    PrimOpApp,
    ThunkMap,
    Ir,
}

pub const NSTATS: usize = 27;

pub const STAT_NAMES: [&str; NSTATS] = [
    "thunk",
    "thunk_app",
    "thunk_app2",
    "env",
    "list_lit",
    "list_map",
    "list_gen",
    "list_concat",
    "list_other",
    "attrs_other",
    "attrs_lit",
    "attrs_upd",
    "attrs_map",
    "attrs_listto",
    "attrs_remove",
    "attrs_zip",
    "attrs_inter",
    "str_obj",
    "str_bytes",
    "ctx",
    "path",
    "float",
    "bigint",
    "lambda",
    "primop_app",
    "thunk_map",
    "ir",
];

impl Stat {
    /// The census origin byte of an attrset producer.
    pub fn attrs_origin(self) -> u8 {
        match self {
            Stat::AttrsLit => 1,
            Stat::AttrsUpd => 2,
            Stat::AttrsMap => 3,
            Stat::AttrsListTo => 4,
            Stat::AttrsRemove => 5,
            Stat::AttrsZip => 6,
            Stat::AttrsInter => 7,
            _ => 0,
        }
    }
}

#[derive(Default)]
pub struct Stats {
    pub counts: [u64; NSTATS],
    pub bytes: [u64; NSTATS],
    #[cfg(feature = "stats2")]
    pub census: super::census::Census,
}

impl Stats {
    #[inline]
    pub fn note(&mut self, s: Stat, bytes: usize) {
        self.counts[s as usize] += 1;
        self.bytes[s as usize] += bytes as u64;
    }
}

/// One distinct string content: its arena bytes and the context-free
/// string object over them once one was made (EMPTY until then).
#[derive(Clone, Copy)]
pub struct StrEntry {
    pub bytes: &'static [u8],
    pub word: Word,
}

/// What `Arena::intern_bytes` hands back: the shared bytes, the shared
/// context-free object if any, and the table key (0 when the content is
/// not in the table because of a hash collision).
#[derive(Clone, Copy)]
pub struct Interned {
    pub bytes: &'static [u8],
    pub word: Word,
    pub hash: u64,
}

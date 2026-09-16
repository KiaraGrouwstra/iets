//! Bump allocator. Memory is never freed: the evaluator's whole heap lives
//! until process exit, so every allocation hands out a `'static` reference.
//! That is what makes graph sharing between cells trivially sound.

use std::ptr;

const CHUNK: usize = 32 << 20;
/// A 32 MiB chunk populates in ~1 ms with huge pages and ~16 ms with 4 KiB
/// pages; anything slower than this is a compaction stall.
const THP_STALL: std::time::Duration = std::time::Duration::from_millis(6);



pub struct Arena {
    ptr: *mut u8,
    end: *mut u8,
    pub bytes_allocated: usize,
    pub stats: Stats,
    /// Advise huge pages until a chunk populate stalls in compaction.
    thp: bool,
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
            stats: Stats::default(),
            thp: true,
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
#[derive(Clone, Copy)]
#[repr(usize)]
pub enum Stat {
    Thunk,
    ThunkApp,
    Env,
    List,
    Attrs,
    StrObj,
    Bytes,
    Ctx,
    Lambda,
    PrimOpApp,
    Ir,
}

pub const STAT_NAMES: [&str; 11] = [
    "thunk",
    "thunk_app",
    "env",
    "list",
    "attrs",
    "str_obj",
    "bytes",
    "ctx",
    "lambda",
    "primop_app",
    "ir",
];

#[derive(Default)]
pub struct Stats {
    pub counts: [u64; 11],
    pub bytes: [u64; 11],
}

impl Stats {
    #[inline]
    pub fn note(&mut self, s: Stat, bytes: usize) {
        self.counts[s as usize] += 1;
        self.bytes[s as usize] += bytes as u64;
    }
}

//! Bump allocator. Memory is never freed: the evaluator's whole heap lives
//! until process exit, so every allocation hands out a `'static` reference.
//! That is what makes graph sharing between cells trivially sound.

use std::alloc::{Layout, alloc};
use std::ptr;

const CHUNK: usize = 8 << 20;

pub struct Arena {
    ptr: *mut u8,
    end: *mut u8,
    pub bytes_allocated: usize,
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
        let chunk = CHUNK.max(size + align);
        // SAFETY: layout is non-zero sized and 16-aligned; the chunk is
        // intentionally leaked.
        let base = unsafe { alloc(Layout::from_size_align(chunk, 16).unwrap()) };
        assert!(!base.is_null(), "arena: out of memory");
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

//! Shared symbol interner: sharded read-write locked maps for lookup, a
//! lock-free id -> name table for `name`. Cloning shares the table.

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};
use std::sync::atomic::{AtomicPtr, AtomicU32, Ordering};
use std::sync::{Arc, RwLock};

use crate::syntax::symbol::Sym;

const SHARDS: usize = 64;
const MAX_SYMS: usize = 1 << 24;

/// FxHash: fast on short identifier-like keys.
#[derive(Default, Clone, Copy)]
pub struct Fx(u64);

impl Hasher for Fx {
    #[inline]
    fn finish(&self) -> u64 {
        self.0
    }
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        let mut h = self.0;
        let mut b = bytes;
        while b.len() >= 8 {
            let w = u64::from_le_bytes(b[..8].try_into().unwrap());
            h = (h.rotate_left(5) ^ w).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
            b = &b[8..];
        }
        if b.len() >= 4 {
            let w = u32::from_le_bytes(b[..4].try_into().unwrap()) as u64;
            h = (h.rotate_left(5) ^ w).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
            b = &b[4..];
        }
        for &x in b {
            h = (h.rotate_left(5) ^ x as u64).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
        }
        self.0 = h;
    }
    #[inline]
    fn write_u8(&mut self, i: u8) {
        self.0 = (self.0.rotate_left(5) ^ i as u64).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
    }
    #[inline]
    fn write_usize(&mut self, i: usize) {
        self.0 = (self.0.rotate_left(5) ^ i as u64).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
    }
}

pub type FxBuild = BuildHasherDefault<Fx>;

#[inline]
fn hash_bytes(b: &[u8]) -> u64 {
    let mut h = Fx(0);
    h.write(b);
    h.finish()
}

struct Inner {
    shards: Vec<RwLock<HashMap<&'static [u8], Sym, FxBuild>>>,
    /// `names[id]` points at a leaked `&'static [u8]`. Written once,
    /// before the id is published through a shard.
    names: *mut AtomicPtr<&'static [u8]>,
    next: AtomicU32,
}

// SAFETY: the name table is append-only; each entry is written once with
// Release before its id escapes and read with Acquire.
unsafe impl Sync for Inner {}
unsafe impl Send for Inner {}

#[derive(Clone)]
pub struct Interner(Arc<Inner>);

impl Default for Interner {
    fn default() -> Self {
        Self::new()
    }
}

impl Interner {
    pub fn new() -> Interner {
        let bytes = MAX_SYMS * size_of::<AtomicPtr<&'static [u8]>>();
        // SAFETY: anonymous zero-filled mapping, paged in on demand.
        let names = unsafe {
            let p = libc::mmap(
                std::ptr::null_mut(),
                bytes,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE,
                -1,
                0,
            );
            assert!(p != libc::MAP_FAILED, "interner: mmap failed");
            p as *mut AtomicPtr<&'static [u8]>
        };
        Interner(Arc::new(Inner {
            shards: (0..SHARDS)
                .map(|_| RwLock::new(HashMap::with_hasher(FxBuild::default())))
                .collect(),
            names,
            next: AtomicU32::new(0),
        }))
    }

    #[inline]
    fn shard(&self, name: &[u8]) -> &RwLock<HashMap<&'static [u8], Sym, FxBuild>> {
        &self.0.shards[(hash_bytes(name) >> 58) as usize % SHARDS]
    }

    pub fn intern(&self, name: &[u8]) -> Sym {
        let shard = self.shard(name);
        if let Some(&s) = shard.read().unwrap().get(name) {
            return s;
        }
        let mut w = shard.write().unwrap();
        if let Some(&s) = w.get(name) {
            return s;
        }
        let leaked: &'static [u8] = Box::leak(name.to_vec().into_boxed_slice());
        let id = self.0.next.fetch_add(1, Ordering::Relaxed);
        assert!((id as usize) < MAX_SYMS, "too many symbols");
        let entry: *mut &'static [u8] = Box::into_raw(Box::new(leaked));
        // SAFETY: id < MAX_SYMS indexes the mapping.
        unsafe { (*self.0.names.add(id as usize)).store(entry, Ordering::Release) };
        w.insert(leaked, Sym(id));
        Sym(id)
    }

    pub fn get(&self, name: &[u8]) -> Option<Sym> {
        self.shard(name).read().unwrap().get(name).copied()
    }

    pub fn name(&self, s: Sym) -> &'static [u8] {
        assert!((s.0 as usize) < MAX_SYMS);
        // SAFETY: ids come from `intern`, whose entry store happened before
        // the id was published.
        unsafe {
            let p = (*self.0.names.add(s.0 as usize)).load(Ordering::Acquire);
            assert!(!p.is_null(), "unknown symbol {}", s.0);
            *p
        }
    }

    pub fn name_str(&self, s: Sym) -> &'static str {
        std::str::from_utf8(self.name(s)).unwrap_or("<non-utf8>")
    }

    pub fn len(&self) -> usize {
        self.0.next.load(Ordering::Relaxed) as usize
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intern_is_stable_and_shared() {
        let i = Interner::new();
        let a = i.intern(b"foo");
        let b = i.intern(b"bar");
        assert_ne!(a, b);
        assert_eq!(i.intern(b"foo"), a);
        assert_eq!(i.name(a), b"foo");
        assert_eq!(i.get(b"bar"), Some(b));
        assert_eq!(i.get(b"baz"), None);
        let j = i.clone();
        let c = j.intern(b"baz");
        assert_eq!(i.get(b"baz"), Some(c));
        assert_eq!(i.len(), 3);
    }

    #[test]
    fn concurrent_intern_agrees() {
        let i = Interner::new();
        let hs: Vec<_> = (0..8)
            .map(|t| {
                let i = i.clone();
                std::thread::spawn(move || {
                    let mut v = Vec::new();
                    for k in 0..5000u32 {
                        let name = format!("sym{}", (k * 7 + t) % 3000);
                        v.push((name.clone(), i.intern(name.as_bytes())));
                    }
                    v
                })
            })
            .collect();
        for h in hs {
            for (name, s) in h.join().unwrap() {
                assert_eq!(i.name(s), name.as_bytes());
                assert_eq!(i.get(name.as_bytes()), Some(s));
            }
        }
        assert_eq!(i.len(), 3000);
    }
}

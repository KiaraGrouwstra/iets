//! Shared-by-handle containers for the evaluator state that every fiber
//! must see: file table, import cache, source-to-store map, derivation
//! tables. Cloning shares.

use std::collections::HashMap;
use std::hash::Hash;
use std::ops::Index;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use crate::eval::{FileInfo, Pos};
use crate::runtime::Word;
use crate::syntax::ast::FileId;
use crate::syntax::symbol::Sym;

pub struct SharedMap<K, V>(Arc<Mutex<HashMap<K, V>>>);

impl<K, V> Clone for SharedMap<K, V> {
    fn clone(&self) -> Self {
        SharedMap(self.0.clone())
    }
}

impl<K, V> Default for SharedMap<K, V> {
    fn default() -> Self {
        SharedMap(Arc::new(Mutex::new(HashMap::new())))
    }
}

impl<K: Hash + Eq, V: Clone> SharedMap<K, V> {
    pub fn get<Q: ?Sized + Hash + Eq>(&self, k: &Q) -> Option<V>
    where
        K: std::borrow::Borrow<Q>,
    {
        self.0.lock().unwrap().get(k).cloned()
    }

    pub fn insert(&self, k: K, v: V) -> Option<V> {
        self.0.lock().unwrap().insert(k, v)
    }

    /// Insert `v` unless `k` is present; return the value now stored.
    pub fn get_or_insert(&self, k: K, v: V) -> V {
        self.0.lock().unwrap().entry(k).or_insert(v).clone()
    }

    pub fn len(&self) -> usize {
        self.0.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The map itself, for callers that read it whole (`ietsp build`).
    pub fn lock(&self) -> MutexGuard<'_, HashMap<K, V>> {
        self.0.lock().unwrap()
    }

    /// Snapshot of all entries.
    pub fn iter(&self) -> std::vec::IntoIter<(K, V)>
    where
        K: Clone,
    {
        self.0
            .lock()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect::<Vec<_>>()
            .into_iter()
    }
}

struct FilesInner {
    list: Vec<&'static FileInfo>,
    ids: HashMap<Vec<u8>, FileId>,
    /// Global source offset the next file starts at (`Pos` space).
    next_base: u32,
}

/// Append-only table of loaded files: path, global base offset and line
/// table only, never the source text. Entries are leaked so references
/// outlive every lock.
#[derive(Clone)]
pub struct Files(Arc<Mutex<FilesInner>>);

impl Default for Files {
    fn default() -> Self {
        Self::new()
    }
}

impl Files {
    pub fn new() -> Files {
        Files(Arc::new(Mutex::new(FilesInner {
            list: Vec::new(),
            ids: HashMap::new(),
            next_base: 0,
        })))
    }

    /// Register `src` under `path`, or return the existing id.
    pub fn add(&self, path: &[u8], src: &[u8]) -> FileId {
        let mut g = self.0.lock().unwrap();
        if let Some(id) = g.ids.get(path) {
            return *id;
        }
        let id = FileId(g.list.len() as u32);
        let base = g.next_base;
        g.next_base = base
            .checked_add(src.len() as u32 + 1)
            .expect("sources exceed 4 GiB");
        g.list
            .push(Box::leak(Box::new(FileInfo::new(path.to_vec(), src, base))));
        g.ids.insert(path.to_vec(), id);
        id
    }

    pub fn get(&self, i: usize) -> Option<&'static FileInfo> {
        self.0.lock().unwrap().list.get(i).copied()
    }

    /// The file whose base range holds `pos`, and the offset inside it.
    pub fn file_of(&self, pos: Pos) -> Option<(&'static FileInfo, u32)> {
        if pos.is_none() {
            return None;
        }
        let g = self.0.lock().unwrap();
        let i = g.list.partition_point(|f| f.base <= pos.0);
        let f = *g.list.get(i.checked_sub(1)?)?;
        Some((f, pos.0 - f.base))
    }

    pub fn len(&self) -> usize {
        self.0.lock().unwrap().list.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Index<usize> for Files {
    type Output = FileInfo;
    fn index(&self, i: usize) -> &FileInfo {
        self.get(i).expect("file id out of range")
    }
}

/// Lock-free `Sym -> Word` table for the string object of a symbol's name
/// (`Evaluator::sym_string`), shared by every fiber. A per-evaluator
/// `Vec` cost each new fiber a copy or a re-zeroing of one word per
/// symbol (~1 MB with nixpkgs loaded). Entries are written at most a few
/// times: two fibers racing on one symbol both store a valid string, and
/// either one serves.
#[derive(Clone)]
pub struct SymWords(Arc<SymWordsInner>);

struct SymWordsInner(*mut AtomicU64);

// SAFETY: the table holds atomics only; the mapping lives for the process.
unsafe impl Sync for SymWordsInner {}
unsafe impl Send for SymWordsInner {}

impl Default for SymWords {
    fn default() -> Self {
        Self::new()
    }
}

impl SymWords {
    pub fn new() -> SymWords {
        let bytes = super::interner::MAX_SYMS * size_of::<AtomicU64>();
        // SAFETY: anonymous zero-filled mapping (zero is `Word::EMPTY`),
        // paged in on demand.
        let p = unsafe {
            let p = libc::mmap(
                std::ptr::null_mut(),
                bytes,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE,
                -1,
                0,
            );
            assert!(p != libc::MAP_FAILED, "sym words: mmap failed");
            p as *mut AtomicU64
        };
        SymWords(Arc::new(SymWordsInner(p)))
    }

    #[inline]
    fn slot(&self, s: Sym) -> &AtomicU64 {
        assert!((s.0 as usize) < super::interner::MAX_SYMS);
        // SAFETY: in bounds of the mapping.
        unsafe { &*self.0.0.add(s.0 as usize) }
    }

    /// `Word::EMPTY` when no string was stored yet.
    #[inline]
    pub fn get(&self, s: Sym) -> Word {
        // SAFETY: the slot holds EMPTY or a word `set` stored.
        unsafe { Word::from_bits(self.slot(s).load(Ordering::Acquire)) }
    }

    #[inline]
    pub fn set(&self, s: Sym, w: Word) {
        self.slot(s).store(w.bits(), Ordering::Release);
    }
}

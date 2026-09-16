//! Shared-by-handle containers for the evaluator state that every fiber
//! must see: file table, import cache, source-to-store map, derivation
//! tables. Cloning shares.

use std::collections::HashMap;
use std::hash::Hash;
use std::ops::Index;
use std::sync::{Arc, Mutex, MutexGuard};

use crate::eval::FileInfo;
use crate::syntax::ast::FileId;

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
    pub fn new() -> Self {
        Self::default()
    }

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

/// `Mutex`-wrapped value with the same handle semantics.
pub struct Shared<T>(Arc<Mutex<T>>);

impl<T> Clone for Shared<T> {
    fn clone(&self) -> Self {
        Shared(self.0.clone())
    }
}

impl<T: Default> Default for Shared<T> {
    fn default() -> Self {
        Shared(Arc::new(Mutex::new(T::default())))
    }
}

impl<T> Shared<T> {
    pub fn lock(&self) -> MutexGuard<'_, T> {
        self.0.lock().unwrap()
    }
}

struct FilesInner {
    list: Vec<&'static FileInfo>,
    ids: HashMap<Vec<u8>, FileId>,
}

/// Append-only table of loaded files. Entries are leaked so references
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
        })))
    }

    /// Register `src` under `path`, or return the existing id.
    pub fn add(&self, path: &[u8], src: Vec<u8>) -> FileId {
        let mut g = self.0.lock().unwrap();
        if let Some(id) = g.ids.get(path) {
            return *id;
        }
        let id = FileId(g.list.len() as u32);
        let src: &'static [u8] = Box::leak(src.into_boxed_slice());
        g.list
            .push(Box::leak(Box::new(FileInfo::new(path.to_vec(), src))));
        g.ids.insert(path.to_vec(), id);
        id
    }

    pub fn get(&self, i: usize) -> Option<&'static FileInfo> {
        self.0.lock().unwrap().list.get(i).copied()
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

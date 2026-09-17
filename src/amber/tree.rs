//! A local tree as amber objects (core-rs `ingest`, minus xattrs and special files, which a
//! NAR cannot carry) and the inverse: a tree restored from an object getter.

use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::store::nar::FileType;

use super::cdc::{BYTE_OPTS, ITEM_BITS, ItemChunker, split_bytes};
use super::key::{Key, Type};
use super::objects::{
    DirBuilder, Entry, IndexBuilder, Object, S_IFDIR, S_IFLNK, S_IFREG, decode_dir_leaf,
    decode_dir_node, decode_file_node, encode_blob,
};

const MAX_DEPTH: usize = 64;

/// Which POSIX fields go into a directory entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Meta {
    /// What Nix leaves on a registered store path, so the keys equal those of an
    /// `amberfs publish` of the same path: uid 0, gid 0, mtime 1 s, mode 0444/0555, dirs
    /// 0555, symlinks 0777. The keys then depend on the content alone.
    Canonical,
    /// The fields `lstat` reports; the differential test uses it on trees outside the store.
    Lstat,
}

pub const CANONICAL_MTIME_NS: i64 = 1_000_000_000;

/// The top-level node of a store path: a NAR is a directory, a regular file, or a symlink.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Root {
    Dir(Key),
    File { key: Key, executable: bool },
    Symlink(Vec<u8>),
}

impl Root {
    pub fn key(&self) -> Option<&Key> {
        match self {
            Root::Dir(k) | Root::File { key: k, .. } => Some(k),
            Root::Symlink(_) => None,
        }
    }
}

/// Receives every built object; called from several threads at once.
pub trait Sink: Sync {
    fn emit(&self, o: Object);
}

impl<F: Fn(Object) + Sync> Sink for F {
    fn emit(&self, o: Object) {
        self(o)
    }
}

/// Collects the keys of every emitted object, children before parents.
#[derive(Default)]
pub struct KeySink(pub Mutex<Vec<Key>>);

impl Sink for KeySink {
    fn emit(&self, o: Object) {
        self.0.lock().unwrap().push(o.key);
    }
}

/// Decides which directory entries a walk includes, as `builtins.filterSource` does.
pub type Filter<'a> = dyn Fn(&Path, FileType) -> bool + Sync + 'a;

pub fn no_filter(_: &Path, _: FileType) -> bool {
    true
}

/// Chunks `data` into Blobs and returns the file's root key (a Blob, or a FileNode above
/// several).
pub fn ingest_bytes(data: &[u8], sink: &dyn Sink) -> Key {
    ingest_reader(data, sink).expect("in-memory read")
}

fn ingest_reader(r: impl Read, sink: &dyn Sink) -> io::Result<Key> {
    let mut ib = IndexBuilder::new_file(ItemChunker::new(ITEM_BITS));
    let mut emit = |o: Object| sink.emit(o);
    let mut saw = false;
    split_bytes(r, &BYTE_OPTS, |chunk| {
        saw = true;
        let obj = encode_blob(&chunk);
        let k = obj.key;
        emit(obj);
        ib.add_child(&mut emit, k, &[]);
        Ok(())
    })?;
    if !saw {
        let obj = encode_blob(&[]);
        let k = obj.key;
        emit(obj);
        ib.add_child(&mut emit, k, &[]);
    }
    Ok(ib.finish(&mut emit).expect("one child at least"))
}

pub fn ingest_file(path: &Path, sink: &dyn Sink) -> io::Result<Key> {
    ingest_reader(fs::File::open(path)?, sink)
}

struct Sem(AtomicUsize);

impl Sem {
    fn try_acquire(&self) -> bool {
        self.0
            .fetch_update(Ordering::Acquire, Ordering::Relaxed, |v| v.checked_sub(1))
            .is_ok()
    }

    fn release(&self) {
        self.0.fetch_add(1, Ordering::Release);
    }
}

struct Walker<'a> {
    filter: &'a Filter<'a>,
    meta: Meta,
    sink: &'a dyn Sink,
    sem: Sem,
}

/// Builds the tree at `path` and emits every object, subtrees in parallel. Directory
/// entries the filter rejects are skipped, as `store::nar::dump_path` skips them.
pub fn ingest_path(
    path: &Path,
    filter: &Filter<'_>,
    meta: Meta,
    sink: &dyn Sink,
) -> io::Result<Root> {
    let jobs = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    let w = Walker {
        filter,
        meta,
        sink,
        sem: Sem(AtomicUsize::new(jobs)),
    };
    let st = fs::symlink_metadata(path)?;
    let ft = st.file_type();
    if ft.is_dir() {
        Ok(Root::Dir(w.build_dir(path, 0)?))
    } else if ft.is_file() {
        Ok(Root::File {
            key: ingest_file(path, sink)?,
            executable: st.permissions().mode() & 0o100 != 0,
        })
    } else if ft.is_symlink() {
        Ok(Root::Symlink(
            fs::read_link(path)?.as_os_str().as_bytes().to_vec(),
        ))
    } else {
        Err(io::Error::other(format!(
            "file '{}' has an unsupported type",
            path.display()
        )))
    }
}

impl Walker<'_> {
    fn build_dir(&self, dir: &Path, depth: usize) -> io::Result<Key> {
        if depth >= MAX_DEPTH {
            return Err(io::Error::other(format!(
                "path '{}' exceeds maximum directory depth of {MAX_DEPTH}",
                dir.display()
            )));
        }
        let mut ents: Vec<(Vec<u8>, fs::FileType)> = fs::read_dir(dir)?
            .map(|e| {
                let e = e?;
                Ok((e.file_name().as_bytes().to_vec(), e.file_type()?))
            })
            .collect::<io::Result<_>>()?;
        ents.sort_by(|a, b| a.0.cmp(&b.0));
        let slots: Vec<Mutex<Option<io::Result<Entry>>>> =
            ents.iter().map(|_| Mutex::new(None)).collect();
        std::thread::scope(|s| {
            for ((name, ft), slot) in ents.iter().zip(&slots) {
                let child = dir.join(std::ffi::OsStr::from_bytes(name));
                if !(self.filter)(&child, FileType::of(*ft)) {
                    continue;
                }
                let work = move || {
                    *slot.lock().unwrap() = Some(self.build_entry(&child, name, depth));
                };
                if self.sem.try_acquire() {
                    s.spawn(move || {
                        work();
                        self.sem.release();
                    });
                } else {
                    work();
                }
            }
        });
        let mut db = DirBuilder::new(ItemChunker::new(ITEM_BITS));
        let mut emit = |o: Object| self.sink.emit(o);
        for slot in slots {
            if let Some(r) = slot.into_inner().unwrap() {
                db.add_entry(&mut emit, r?);
            }
        }
        Ok(db.finish(&mut emit))
    }

    fn build_entry(&self, full: &Path, name: &[u8], depth: usize) -> io::Result<Entry> {
        let md = fs::symlink_metadata(full)?;
        let ft = md.file_type();
        let mut e = Entry {
            name: name.to_vec(),
            ..Default::default()
        };
        let exec = md.permissions().mode() & 0o100 != 0;
        let kind = if ft.is_file() {
            e.content_key = Some(ingest_file(full, self.sink)?);
            S_IFREG
        } else if ft.is_dir() {
            e.content_key = Some(self.build_dir(full, depth + 1)?);
            S_IFDIR
        } else if ft.is_symlink() {
            e.link_target = fs::read_link(full)?.as_os_str().as_bytes().to_vec();
            S_IFLNK
        } else {
            return Err(io::Error::other(format!(
                "file '{}' has an unsupported type",
                full.display()
            )));
        };
        match self.meta {
            Meta::Canonical => {
                e.mode = kind
                    | match kind {
                        S_IFREG if exec => 0o555,
                        S_IFREG => 0o444,
                        S_IFDIR => 0o555,
                        _ => 0o777,
                    };
                e.mtime = CANONICAL_MTIME_NS;
            }
            Meta::Lstat => {
                e.mode = u64::from(md.mode());
                e.uid = u64::from(md.uid());
                e.gid = u64::from(md.gid());
                e.mtime = md
                    .mtime()
                    .wrapping_mul(1_000_000_000)
                    .wrapping_add(md.mtime_nsec());
            }
        }
        Ok(e)
    }
}

// ---- restore ----

/// Fetches an object's bytes by key.
pub type Get<'a> = dyn Fn(&Key) -> io::Result<Vec<u8>> + 'a;

fn bad(msg: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

/// Writes a file's content (a Blob or a FileNode tree) into `w`.
pub fn write_file(get: &Get<'_>, key: &Key, w: &mut dyn Write) -> io::Result<()> {
    let bytes = get(key)?;
    match key.type_() {
        Type::Blob => w.write_all(&bytes),
        Type::FileNode => {
            for child in decode_file_node(&bytes).map_err(bad)? {
                write_file(get, &child, w)?;
            }
            Ok(())
        }
        t => Err(bad(format!("object {key} is a {t}, not file content"))),
    }
}

/// Reads a whole file's content into memory.
pub fn read_file(get: &Get<'_>, key: &Key) -> io::Result<Vec<u8>> {
    let mut out = Vec::with_capacity(key.length() as usize);
    write_file(get, key, &mut out)?;
    Ok(out)
}

/// The entries of a directory (a DirLeaf or a DirNode tree), in name order.
pub fn entries(get: &Get<'_>, key: &Key) -> io::Result<Vec<Entry>> {
    let mut out = Vec::new();
    collect_entries(get, key, &mut out, 0)?;
    Ok(out)
}

fn collect_entries(get: &Get<'_>, key: &Key, out: &mut Vec<Entry>, depth: usize) -> io::Result<()> {
    if depth >= MAX_DEPTH {
        return Err(bad("directory index too deep".into()));
    }
    let bytes = get(key)?;
    match key.type_() {
        Type::DirLeaf => out.extend(decode_dir_leaf(&bytes).map_err(bad)?),
        Type::DirNode => {
            for (_, child) in decode_dir_node(&bytes).map_err(bad)? {
                collect_entries(get, &child, out, depth + 1)?;
            }
        }
        t => Err(bad(format!("object {key} is a {t}, not a directory")))?,
    }
    Ok(())
}

/// Restores `root` at `dest`, which must not exist. Only the executable bit, the symlink
/// targets and the names come back, as with `store::nar::restore_path`.
pub fn restore(get: &Get<'_>, root: &Root, dest: &Path) -> io::Result<()> {
    match root {
        Root::Dir(k) => restore_dir(get, k, dest, 0),
        Root::File { key, executable } => restore_file(get, key, dest, *executable),
        Root::Symlink(target) => {
            std::os::unix::fs::symlink(std::ffi::OsStr::from_bytes(target), dest)
        }
    }
}

fn restore_file(get: &Get<'_>, key: &Key, dest: &Path, executable: bool) -> io::Result<()> {
    let mut f = fs::File::create(dest)?;
    write_file(get, key, &mut f)?;
    if executable {
        f.set_permissions(fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

fn restore_dir(get: &Get<'_>, key: &Key, dest: &Path, depth: usize) -> io::Result<()> {
    if depth >= MAX_DEPTH {
        return Err(bad("directory nesting too deep".into()));
    }
    fs::create_dir(dest)?;
    let mut prev: Vec<u8> = Vec::new();
    for e in entries(get, key)? {
        if e.name.is_empty()
            || e.name == b"."
            || e.name == b".."
            || e.name.contains(&b'/')
            || e.name.contains(&0)
        {
            return Err(bad("bad entry name".into()));
        }
        if e.name <= prev {
            return Err(bad("directory entries out of order".into()));
        }
        prev.clone_from(&e.name);
        let child = dest.join(std::ffi::OsStr::from_bytes(&e.name));
        match e.file_type() {
            S_IFREG => {
                let k = e
                    .content_key
                    .ok_or_else(|| bad("file without content".into()))?;
                restore_file(get, &k, &child, e.mode & 0o100 != 0)?;
            }
            S_IFDIR => {
                let k = e
                    .content_key
                    .ok_or_else(|| bad("dir without content".into()))?;
                restore_dir(get, &k, &child, depth + 1)?;
            }
            S_IFLNK => {
                if e.link_target.is_empty() {
                    return Err(bad("symlink without target".into()));
                }
                std::os::unix::fs::symlink(std::ffi::OsStr::from_bytes(&e.link_target), &child)?;
            }
            t => return Err(bad(format!("entry type {t:o} cannot be restored"))),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// Keeps every object; `get` reads from it.
    #[derive(Default)]
    pub struct MapSink(pub Mutex<HashMap<Key, Vec<u8>>>);

    impl Sink for MapSink {
        fn emit(&self, o: Object) {
            self.0.lock().unwrap().insert(o.key, o.bytes);
        }
    }

    #[test]
    fn roundtrip_fixture_tree() {
        let tree = crate::store::nar::tests::fixture_tree("amber");
        let sink = MapSink::default();
        let root = ingest_path(&tree, &no_filter, Meta::Canonical, &sink).unwrap();
        let objs = sink.0.into_inner().unwrap();
        assert!(matches!(root, Root::Dir(_)));
        let get = |k: &Key| {
            objs.get(k)
                .cloned()
                .ok_or_else(|| io::Error::other("missing"))
        };
        let out = tree.with_file_name("restored");
        restore(&get, &root, &out).unwrap();
        assert_eq!(
            crate::store::nar::nar_hash(&out).unwrap(),
            crate::store::nar::nar_hash(&tree).unwrap()
        );
        let root_key = *root.key().unwrap();
        assert_eq!(root_key.type_(), Type::DirLeaf);
        let ents = entries(&get, &root_key).unwrap();
        let names: Vec<&[u8]> = ents.iter().map(|e| e.name.as_slice()).collect();
        assert_eq!(names, [b"a.txt".as_slice(), b"link", b"run.sh", b"sub"]);
        assert_eq!(ents[2].mode, S_IFREG | 0o555);
        assert_eq!(ents[0].mode, S_IFREG | 0o444);
        assert_eq!(ents[0].mtime, CANONICAL_MTIME_NS);
        assert_eq!(ents[1].link_target, b"a.txt");
        assert_eq!(ents[3].mode, S_IFDIR | 0o555);
    }

    #[test]
    fn filtered_walk_skips_entries() {
        let tree = crate::store::nar::tests::fixture_tree("amber-filter");
        let sink = MapSink::default();
        let keep = tree.join("a.txt");
        let root = ingest_path(&tree, &|p: &Path, _| p == keep, Meta::Canonical, &sink).unwrap();
        let objs = sink.0.into_inner().unwrap();
        let get = |k: &Key| Ok(objs[k].clone());
        assert_eq!(entries(&get, root.key().unwrap()).unwrap().len(), 1);
    }

    #[test]
    fn bytes_and_file_agree() {
        let tree = crate::store::nar::tests::fixture_tree("amber-bytes");
        let data = vec![0x5au8; 300_000];
        fs::write(tree.join("big"), &data).unwrap();
        let sink = MapSink::default();
        let a = ingest_bytes(&data, &sink);
        let b = ingest_file(&tree.join("big"), &sink).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.type_(), Type::FileNode);
        assert_eq!(a.length(), 300_000);
        let objs = sink.0.into_inner().unwrap();
        let get = |k: &Key| Ok(objs[k].clone());
        assert_eq!(read_file(&get, &a).unwrap(), data);
        let empty = ingest_bytes(&[], &|_| {});
        assert_eq!(empty.length(), 0);
    }
}

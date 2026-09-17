//! The amber object encodings (core-rs `src/fstree/`, `architecture/fstree.md`) and the
//! bottom-up builders. Every structured object is deterministic CBOR (RFC 8949 4.2, the
//! bytes fxamacker/cbor emits for the Go implementation); a `Blob` is raw bytes.
//!
//! - `Blob` (0): file content chunk; key length = byte count.
//! - `FileNode` (1): array of child keys; key length = the file size (sum of children).
//! - `DirLeaf` (2): array of entry maps with integer keys 0 name, 1 mode, 2 uid, 3 gid,
//!   4 mtime (ns), 5 content key, 6 symlink target; key length = own bytes + sum of the
//!   content keys' lengths.
//! - `DirNode` (3): array of `[separator name, child key]` pairs; key length = own bytes +
//!   sum of the children's lengths.

use super::cdc::ItemChunker;
use super::key::{Key, SIZE, Type};

const MAJOR_UINT: u8 = 0;
const MAJOR_NEGINT: u8 = 1;
const MAJOR_BSTR: u8 = 2;
const MAJOR_ARRAY: u8 = 4;
const MAJOR_MAP: u8 = 5;

pub fn append_head(b: &mut Vec<u8>, major: u8, n: u64) {
    let h = major << 5;
    if n < 24 {
        b.push(h | n as u8);
    } else if n < 1 << 8 {
        b.push(h | 24);
        b.push(n as u8);
    } else if n < 1 << 16 {
        b.push(h | 25);
        b.extend_from_slice(&(n as u16).to_be_bytes());
    } else if n < 1 << 32 {
        b.push(h | 26);
        b.extend_from_slice(&(n as u32).to_be_bytes());
    } else {
        b.push(h | 27);
        b.extend_from_slice(&n.to_be_bytes());
    }
}

pub fn append_bstr(b: &mut Vec<u8>, s: &[u8]) {
    append_head(b, MAJOR_BSTR, s.len() as u64);
    b.extend_from_slice(s);
}

fn append_int(b: &mut Vec<u8>, v: i64) {
    if v >= 0 {
        append_head(b, MAJOR_UINT, v as u64);
    } else {
        append_head(b, MAJOR_NEGINT, !v as u64);
    }
}

/// A built object: its key and its bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Object {
    pub key: Key,
    pub bytes: Vec<u8>,
}

/// The POSIX file-type bits an entry's `mode` carries.
pub const S_IFMT: u64 = 0o170000;
pub const S_IFREG: u64 = 0o100000;
pub const S_IFDIR: u64 = 0o040000;
pub const S_IFLNK: u64 = 0o120000;

/// A directory entry: what a NAR represents, with the POSIX fields amber stores. Device and
/// xattr payloads (map keys 7-9) are never produced here.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Entry {
    pub name: Vec<u8>,
    pub mode: u64,
    pub uid: u64,
    pub gid: u64,
    pub mtime: i64,
    /// Regular files and directories.
    pub content_key: Option<Key>,
    /// Symlinks.
    pub link_target: Vec<u8>,
}

impl Entry {
    pub fn file_type(&self) -> u64 {
        self.mode & S_IFMT
    }
}

fn append_entry(out: &mut Vec<u8>, e: &Entry) {
    let pairs = 5 + u64::from(e.content_key.is_some()) + u64::from(!e.link_target.is_empty());
    append_head(out, MAJOR_MAP, pairs);
    append_head(out, MAJOR_UINT, 0);
    append_bstr(out, &e.name);
    append_head(out, MAJOR_UINT, 1);
    append_head(out, MAJOR_UINT, e.mode);
    append_head(out, MAJOR_UINT, 2);
    append_head(out, MAJOR_UINT, e.uid);
    append_head(out, MAJOR_UINT, 3);
    append_head(out, MAJOR_UINT, e.gid);
    append_head(out, MAJOR_UINT, 4);
    append_int(out, e.mtime);
    if let Some(k) = &e.content_key {
        append_head(out, MAJOR_UINT, 5);
        append_bstr(out, k.as_bytes());
    }
    if !e.link_target.is_empty() {
        append_head(out, MAJOR_UINT, 6);
        append_bstr(out, &e.link_target);
    }
}

fn marshal_entries(entries: &[Entry]) -> Vec<u8> {
    let mut b = Vec::new();
    append_head(&mut b, MAJOR_ARRAY, entries.len() as u64);
    for e in entries {
        append_entry(&mut b, e);
    }
    b
}

fn marshal_pairs(pairs: &[(Vec<u8>, Key)]) -> Vec<u8> {
    let mut b = Vec::new();
    append_head(&mut b, MAJOR_ARRAY, pairs.len() as u64);
    for (sep, key) in pairs {
        append_head(&mut b, MAJOR_ARRAY, 2);
        append_bstr(&mut b, sep);
        append_bstr(&mut b, key.as_bytes());
    }
    b
}

pub fn encode_blob(data: &[u8]) -> Object {
    Object {
        key: Key::new(Type::Blob, data.len() as u64, data),
        bytes: data.to_vec(),
    }
}

pub fn encode_file_node(children: &[Key]) -> Object {
    let mut b = Vec::with_capacity(1 + children.len() * (SIZE + 2));
    append_head(&mut b, MAJOR_ARRAY, children.len() as u64);
    let mut sum = 0u64;
    for c in children {
        append_bstr(&mut b, c.as_bytes());
        sum = sum.wrapping_add(c.length());
    }
    Object {
        key: Key::new(Type::FileNode, sum, &b),
        bytes: b,
    }
}

pub fn encode_dir_leaf(entries: &[Entry]) -> Object {
    let b = marshal_entries(entries);
    let sub: u64 = entries
        .iter()
        .filter_map(|e| e.content_key.as_ref())
        .fold(0u64, |acc, k| acc.wrapping_add(k.length()));
    Object {
        key: Key::new(Type::DirLeaf, (b.len() as u64).wrapping_add(sub), &b),
        bytes: b,
    }
}

pub fn encode_dir_node(pairs: &[(Vec<u8>, Key)]) -> Object {
    let b = marshal_pairs(pairs);
    let sub = pairs
        .iter()
        .fold(0u64, |acc, (_, k)| acc.wrapping_add(k.length()));
    Object {
        key: Key::new(Type::DirNode, (b.len() as u64).wrapping_add(sub), &b),
        bytes: b,
    }
}

// ---- decoding (what the builder restores from) ----

fn bad(msg: &str) -> String {
    format!("bad amber object: {msg}")
}

fn read_head(b: &[u8]) -> Result<(u8, u64, &[u8]), String> {
    let (&first, b) = b.split_first().ok_or_else(|| bad("truncated"))?;
    let major = first >> 5;
    let ai = first & 0x1f;
    let take = |n: usize| -> Result<(u64, &[u8]), String> {
        if b.len() < n {
            return Err(bad("truncated"));
        }
        let mut buf = [0u8; 8];
        buf[8 - n..].copy_from_slice(&b[..n]);
        Ok((u64::from_be_bytes(buf), &b[n..]))
    };
    match ai {
        0..=23 => Ok((major, u64::from(ai), b)),
        24 => take(1).map(|(n, r)| (major, n, r)),
        25 => take(2).map(|(n, r)| (major, n, r)),
        26 => take(4).map(|(n, r)| (major, n, r)),
        27 => take(8).map(|(n, r)| (major, n, r)),
        _ => Err(bad("indefinite or reserved head")),
    }
}

fn read_bstr(b: &[u8]) -> Result<(&[u8], &[u8]), String> {
    let (major, n, b) = read_head(b)?;
    if major != MAJOR_BSTR {
        return Err(bad("expected byte string"));
    }
    if (b.len() as u64) < n {
        return Err(bad("truncated"));
    }
    let n = n as usize;
    Ok((&b[..n], &b[n..]))
}

fn read_uint(b: &[u8]) -> Result<(u64, &[u8]), String> {
    let (major, n, b) = read_head(b)?;
    if major != MAJOR_UINT {
        return Err(bad("expected unsigned integer"));
    }
    Ok((n, b))
}

fn read_int(b: &[u8]) -> Result<(i64, &[u8]), String> {
    let (major, n, b) = read_head(b)?;
    match major {
        MAJOR_UINT => Ok((n as i64, b)),
        MAJOR_NEGINT => Ok((!(n as i64), b)),
        _ => Err(bad("expected integer")),
    }
}

fn read_key(b: &[u8]) -> Result<(Key, &[u8]), String> {
    let (raw, rest) = read_bstr(b)?;
    Ok((Key::parse(raw)?, rest))
}

fn read_array(b: &[u8]) -> Result<(u64, &[u8]), String> {
    let (major, n, b) = read_head(b)?;
    if major != MAJOR_ARRAY {
        return Err(bad("expected array"));
    }
    Ok((n, b))
}

pub fn decode_file_node(bytes: &[u8]) -> Result<Vec<Key>, String> {
    let (n, mut b) = read_array(bytes)?;
    if n > bytes.len() as u64 {
        return Err(bad("array count exceeds size"));
    }
    let mut keys = Vec::with_capacity(n as usize);
    for _ in 0..n {
        let (k, rest) = read_key(b)?;
        keys.push(k);
        b = rest;
    }
    if !b.is_empty() {
        return Err(bad("trailing bytes"));
    }
    Ok(keys)
}

pub fn decode_dir_leaf(bytes: &[u8]) -> Result<Vec<Entry>, String> {
    let (n, mut b) = read_array(bytes)?;
    if n > bytes.len() as u64 {
        return Err(bad("array count exceeds size"));
    }
    let mut entries = Vec::with_capacity(n as usize);
    for _ in 0..n {
        let (major, pairs, mut rest) = read_head(b)?;
        if major != MAJOR_MAP {
            return Err(bad("expected entry map"));
        }
        let mut e = Entry::default();
        for _ in 0..pairs {
            let (k, r) = read_uint(rest)?;
            rest = match k {
                0 => {
                    let (v, r) = read_bstr(r)?;
                    e.name = v.to_vec();
                    r
                }
                1 => {
                    let (v, r) = read_uint(r)?;
                    e.mode = v;
                    r
                }
                2 => {
                    let (v, r) = read_uint(r)?;
                    e.uid = v;
                    r
                }
                3 => {
                    let (v, r) = read_uint(r)?;
                    e.gid = v;
                    r
                }
                4 => {
                    let (v, r) = read_int(r)?;
                    e.mtime = v;
                    r
                }
                5 => {
                    let (v, r) = read_key(r)?;
                    e.content_key = Some(v);
                    r
                }
                6 => {
                    let (v, r) = read_bstr(r)?;
                    e.link_target = v.to_vec();
                    r
                }
                _ => return Err(bad("entry carries a device or xattr payload")),
            };
        }
        entries.push(e);
        b = rest;
    }
    if !b.is_empty() {
        return Err(bad("trailing bytes"));
    }
    Ok(entries)
}

pub fn decode_dir_node(bytes: &[u8]) -> Result<Vec<(Vec<u8>, Key)>, String> {
    let (n, mut b) = read_array(bytes)?;
    if n > bytes.len() as u64 {
        return Err(bad("array count exceeds size"));
    }
    let mut pairs = Vec::with_capacity(n as usize);
    for _ in 0..n {
        let (two, rest) = read_array(b)?;
        if two != 2 {
            return Err(bad("expected [sepName, childKey]"));
        }
        let (sep, rest) = read_bstr(rest)?;
        let (k, rest) = read_key(rest)?;
        pairs.push((sep.to_vec(), k));
        b = rest;
    }
    if !b.is_empty() {
        return Err(bad("trailing bytes"));
    }
    Ok(pairs)
}

/// Checks that `bytes` is the object `key` names: the key is recomputed from the bytes,
/// including the logical length, which needs the children for the structured types.
pub fn verify(key: &Key, bytes: &[u8]) -> Result<(), String> {
    let computed = match key.type_() {
        Type::Blob => Key::new(Type::Blob, bytes.len() as u64, bytes),
        Type::FileNode => encode_file_node(&decode_file_node(bytes)?).key,
        Type::DirLeaf => encode_dir_leaf(&decode_dir_leaf(bytes)?).key,
        Type::DirNode => encode_dir_node(&decode_dir_node(bytes)?).key,
        Type::XattrSet => return Err("xattr objects are not accepted".into()),
    };
    if &computed != key {
        return Err(format!("object bytes hash to {computed}, not {key}"));
    }
    Ok(())
}

// ---- builders (core-rs `fstree/builder.rs`) ----

pub type Emit<'a> = dyn FnMut(Object) + 'a;

#[derive(Default)]
struct Level {
    keys: Vec<Key>,
    seps: Vec<Vec<u8>>,
    run_len: usize,
}

/// Builds the index levels above a leaf stream: `FileNode`s over Blob keys, `DirNode`s over
/// DirLeaf keys. Objects are emitted children before parents; the root comes last. A single
/// child is returned as the root without a node around it.
pub struct IndexBuilder {
    ic: ItemChunker,
    is_dir: bool,
    levels: Vec<Level>,
}

impl IndexBuilder {
    pub fn new_file(ic: ItemChunker) -> IndexBuilder {
        IndexBuilder {
            ic,
            is_dir: false,
            levels: Vec::new(),
        }
    }

    fn new_dir(ic: ItemChunker) -> IndexBuilder {
        IndexBuilder {
            ic,
            is_dir: true,
            levels: Vec::new(),
        }
    }

    fn has_any(&self) -> bool {
        !self.levels.is_empty()
    }

    pub fn add_child(&mut self, emit: &mut Emit<'_>, key: Key, sep: &[u8]) {
        self.add(emit, 0, key, sep)
    }

    fn add(&mut self, emit: &mut Emit<'_>, l: usize, key: Key, sep: &[u8]) {
        while self.levels.len() <= l {
            self.levels.push(Level::default());
        }
        let level = &mut self.levels[l];
        level.keys.push(key);
        if self.is_dir {
            level.seps.push(sep.to_vec());
        }
        level.run_len += 1;
        let run_len = level.run_len;
        let boundary = if self.is_dir {
            let framed = marshal_pairs(&[(sep.to_vec(), key)]);
            self.ic.is_boundary(&framed[1..], run_len)
        } else {
            self.ic.is_boundary(key.as_bytes(), run_len)
        };
        if boundary {
            let (obj, sep) = self.build_node(l);
            self.reset(l);
            let k = obj.key;
            emit(obj);
            self.add(emit, l + 1, k, &sep);
        }
    }

    fn reset(&mut self, l: usize) {
        let level = &mut self.levels[l];
        level.keys.clear();
        level.seps.clear();
        level.run_len = 0;
    }

    fn build_node(&self, l: usize) -> (Object, Vec<u8>) {
        let level = &self.levels[l];
        if self.is_dir {
            let pairs: Vec<(Vec<u8>, Key)> = level
                .seps
                .iter()
                .cloned()
                .zip(level.keys.iter().copied())
                .collect();
            let sep = level.seps.last().cloned().unwrap_or_default();
            (encode_dir_node(&pairs), sep)
        } else {
            (encode_file_node(&level.keys), Vec::new())
        }
    }

    /// Collapses the open runs bottom-up and returns the root key. `None` without children.
    pub fn finish(mut self, emit: &mut Emit<'_>) -> Option<Key> {
        let mut l = 0;
        while l < self.levels.len() {
            let is_top = l == self.levels.len() - 1;
            match self.levels[l].run_len {
                0 => {}
                1 => {
                    let key = self.levels[l].keys[0];
                    let sep = if self.is_dir {
                        std::mem::take(&mut self.levels[l].seps[0])
                    } else {
                        Vec::new()
                    };
                    self.reset(l);
                    if is_top {
                        return Some(key);
                    }
                    self.add(emit, l + 1, key, &sep);
                }
                _ => {
                    let (obj, sep) = self.build_node(l);
                    self.reset(l);
                    let k = obj.key;
                    emit(obj);
                    self.add(emit, l + 1, k, &sep);
                }
            }
            l += 1;
        }
        None
    }
}

/// Builds one directory from its entries, given sorted bytewise by name: `DirLeaf` runs,
/// then `DirNode` levels above them.
pub struct DirBuilder {
    ic: ItemChunker,
    idx: IndexBuilder,
    leaf: Vec<Entry>,
    leaf_max: Vec<u8>,
    run_len: usize,
}

impl DirBuilder {
    pub fn new(ic: ItemChunker) -> DirBuilder {
        DirBuilder {
            ic,
            idx: IndexBuilder::new_dir(ic),
            leaf: Vec::new(),
            leaf_max: Vec::new(),
            run_len: 0,
        }
    }

    pub fn add_entry(&mut self, emit: &mut Emit<'_>, e: Entry) {
        let framed = marshal_entries(std::slice::from_ref(&e));
        let enc = &framed[1..];
        self.leaf_max = e.name.clone();
        self.leaf.push(e);
        self.run_len += 1;
        if self.ic.is_boundary(enc, self.run_len) {
            self.close_leaf(emit);
        }
    }

    fn close_leaf(&mut self, emit: &mut Emit<'_>) {
        let obj = encode_dir_leaf(&self.leaf);
        let sep = std::mem::take(&mut self.leaf_max);
        self.leaf.clear();
        self.run_len = 0;
        let k = obj.key;
        emit(obj);
        self.idx.add(emit, 0, k, &sep);
    }

    /// Closes the last run (an empty directory is one empty `DirLeaf`) and returns the root.
    pub fn finish(mut self, emit: &mut Emit<'_>) -> Key {
        if self.run_len > 0 || !self.idx.has_any() {
            self.close_leaf(emit);
        }
        self.idx
            .finish(emit)
            .expect("a directory has at least one leaf")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::amber::cdc::ITEM_BITS;

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    fn k1() -> Key {
        Key::new(Type::Blob, 100, b"x")
    }

    const K1_BSTR: &str = "582000643ae7d805f6789a6402acb70ad4096a85a56bf6804eaf25c0493ac697548d";

    // Go oracle outputs from core-rs encode.rs tests.
    #[test]
    fn entries_match_fxamacker() {
        let e = Entry {
            name: b"f".to_vec(),
            mode: 0o100644,
            uid: 1000,
            gid: 1000,
            mtime: 1700000000000000000,
            content_key: Some(k1()),
            link_target: Vec::new(),
        };
        assert_eq!(
            hex(&marshal_entries(&[e])),
            format!("81a6004166011981a4021903e8031903e8041b17979cfe362a000005{K1_BSTR}")
        );
        let neg = Entry {
            name: b"f".to_vec(),
            mtime: -1,
            ..Default::default()
        };
        assert_eq!(hex(&marshal_entries(&[neg])), "81a50041660100020003000420");
        let min = Entry {
            name: b"f".to_vec(),
            mtime: i64::MIN,
            ..Default::default()
        };
        assert_eq!(
            hex(&marshal_entries(&[min])),
            "81a5004166010002000300043b7fffffffffffffff"
        );
        let link = Entry {
            link_target: b"../small.txt".to_vec(),
            ..Default::default()
        };
        assert_eq!(
            hex(&marshal_entries(&[link])),
            "81a600400100020003000400064c2e2e2f736d616c6c2e747874"
        );
        assert_eq!(hex(&marshal_entries(&[])), "80");
        assert_eq!(
            hex(&marshal_entries(&[Entry::default()])),
            "81a500400100020003000400"
        );
    }

    #[test]
    fn nodes_match_go() {
        let k = k1();
        let o = encode_file_node(&[k, k]);
        assert_eq!(hex(&o.bytes), format!("82{K1_BSTR}{K1_BSTR}"));
        assert_eq!(o.key.length(), 200);
        let o = encode_dir_node(&[(b"a".to_vec(), k)]);
        assert_eq!(hex(&o.bytes), format!("81824161{K1_BSTR}"));
        assert_eq!(o.key.length(), o.bytes.len() as u64 + 100);
        let leaf = encode_dir_leaf(&[Entry {
            name: b"f".to_vec(),
            mode: 0o100644,
            content_key: Some(encode_blob(&[0u8; 1000]).key),
            ..Default::default()
        }]);
        assert_eq!(leaf.key.length(), leaf.bytes.len() as u64 + 1000);
    }

    #[test]
    fn empty_blob_key() {
        assert_eq!(
            encode_blob(&[]).key.to_string(),
            "0000af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f"
        );
    }

    fn collect<'a>(
        objs: &'a mut Vec<Object>,
        digest: &'a mut blake3::Hasher,
    ) -> impl FnMut(Object) + 'a {
        move |o| {
            digest.update(o.key.as_bytes());
            digest.update(&o.bytes);
            objs.push(o);
        }
    }

    // Go oracle: root key, object count, BLAKE3 over key ‖ bytes of every emission in order.
    #[test]
    fn dir_builder_matches_go_oracle() {
        let mut objs = Vec::new();
        let mut digest = blake3::Hasher::new();
        let mut db = DirBuilder::new(ItemChunker::new(2));
        {
            let mut emit = collect(&mut objs, &mut digest);
            for i in 0..5000 {
                db.add_entry(
                    &mut emit,
                    Entry {
                        name: format!("{i:06}").into_bytes(),
                        mode: 0o100644,
                        ..Default::default()
                    },
                );
            }
        }
        let root = db.finish(&mut collect(&mut objs, &mut digest));
        assert_eq!(
            root.to_string(),
            "3202461152e56c8a9ed1aa2ad5656f2b117ebc7e685f539c432f815c05a51f37"
        );
        assert_eq!(objs.len(), 1257);
        assert_eq!(
            hex(digest.finalize().as_bytes()),
            "4a9b0c0846cb507a873425ab4b34cc0ee9cdf56d0a6c18b37fbe5054c1962b38"
        );

        let mut objs = Vec::new();
        let mut digest = blake3::Hasher::new();
        let mut db = DirBuilder::new(ItemChunker::new(ITEM_BITS));
        for n in [b"a", b"b", b"c"] {
            db.add_entry(
                &mut collect(&mut objs, &mut digest),
                Entry {
                    name: n.to_vec(),
                    mode: 0o100644,
                    ..Default::default()
                },
            );
        }
        let root = db.finish(&mut collect(&mut objs, &mut digest));
        assert_eq!(
            root.to_string(),
            "202b28b4dde93f1191318271713dc55c16a0fa32522a1913cb83066b7309b7ae"
        );
        assert_eq!(objs.len(), 1);
        assert_eq!(
            hex(digest.finalize().as_bytes()),
            "550c64f87004407800260e3efffaee6423c15fb7e3b35c9ad191d695933be778"
        );
    }

    #[test]
    fn file_index_matches_go_oracle() {
        let mut objs = Vec::new();
        let mut digest = blake3::Hasher::new();
        let mut ib = IndexBuilder::new_file(ItemChunker::new(2));
        for i in 0..2000 {
            let k = encode_blob(&vec![0u8; i + 1]).key;
            ib.add_child(&mut collect(&mut objs, &mut digest), k, &[]);
        }
        let root = ib.finish(&mut collect(&mut objs, &mut digest)).unwrap();
        assert_eq!(
            root.to_string(),
            "121e886843c684a13438af8c12ece3155512d7631aa0ab48dc0e05fb9a57e4ce"
        );
        assert_eq!(root.length(), 2_001_000);
        assert_eq!(objs.len(), 482);
        assert_eq!(
            hex(digest.finalize().as_bytes()),
            "ebc6d63710b1c5308b777729f1ea20fe9431e874b1be3aeac2e51fb558838fb6"
        );

        let single = encode_blob(b"one").key;
        let mut ib = IndexBuilder::new_file(ItemChunker::new(ITEM_BITS));
        let mut n = 0;
        ib.add_child(&mut |_| n += 1, single, &[]);
        assert_eq!(ib.finish(&mut |_| n += 1), Some(single));
        assert_eq!(n, 0);
    }

    #[test]
    fn decode_roundtrip() {
        let k = k1();
        let entries = vec![
            Entry {
                name: b"a".to_vec(),
                mode: 0o100444,
                uid: 0,
                gid: 0,
                mtime: 1_000_000_000,
                content_key: Some(k),
                link_target: Vec::new(),
            },
            Entry {
                name: "\u{e9}".as_bytes().to_vec(),
                mode: 0o120777,
                mtime: -5,
                link_target: b"a".to_vec(),
                ..Default::default()
            },
        ];
        let leaf = encode_dir_leaf(&entries);
        assert_eq!(decode_dir_leaf(&leaf.bytes).unwrap(), entries);
        let node = encode_file_node(&[k, encode_blob(b"z").key]);
        assert_eq!(decode_file_node(&node.bytes).unwrap().len(), 2);
        let dn = encode_dir_node(&[(b"z".to_vec(), leaf.key)]);
        assert_eq!(
            decode_dir_node(&dn.bytes).unwrap(),
            vec![(b"z".to_vec(), leaf.key)]
        );
        assert!(decode_dir_leaf(&node.bytes[..1]).is_err());
    }
}

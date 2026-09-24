//! The NAR of a downloaded archive, without unpacking it.
//!
//! A tar is in archive order; a NAR needs every directory's entries sorted by
//! name and nested inside it. `fetch::unpack` gets there by writing a tree and
//! walking it, which costs a second copy of every byte on disk. This module
//! spools the decompressed archive into one file as it arrives, reads it once to
//! record where each member's bytes are, and then writes the NAR out of that
//! index, reading the bytes back by offset. Peak memory is the index plus one
//! copy buffer; nothing holds a member whole.
//!
//! The rules are `fetch::unpack`'s, which are tarfile.cc's and
//! git-utils.cc:GitFileSystemObjectSinkImpl's. `spool_matches_the_tree` in the
//! tests holds both paths to the same NAR bytes.
//!
//! The same index also answers evaluation. `crate::treestore` keeps the spool and
//! a serialized index in the user-level input store, and every read of a
//! fetched flake input, `import` included, comes back out of the spool by
//! offset instead of out of `/nix/store` (docs/flakes.md).

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::FileExt;
use std::path::Path;

use crate::error::{Error, Result};
use crate::store::hash::{Hash, HashAlgo};
use crate::store::nar::{FileType, NAR_MAX_DEPTH, PathFilter, write_padding, write_str, write_u64};

/// One directory's entries, in the byte order a NAR wants them. A sorted vector,
/// not a map: a full nixpkgs holds 38 000 directories of two or three entries
/// each, and a `BTreeMap` pays for eleven of them per directory it has.
pub type Dir = Vec<(Vec<u8>, Node)>;

/// A member path split into its components.
type Parts = Vec<Vec<u8>>;

/// Bytes moved per `pread` when a member is copied out of the spool.
const CHUNK: usize = 128 * 1024;

#[derive(Debug)]
pub enum Node {
    Dir(Dir),
    File { at: Blob, exec: bool },
    Symlink(Vec<u8>),
}

/// Where a file's bytes are: in the spooled archive, or in the side file that
/// holds the expanded GNU sparse members.
#[derive(Clone, Copy, Debug)]
pub struct Blob {
    offset: u64,
    size: u64,
    sparse: bool,
}

impl Node {
    pub fn file_type(&self) -> FileType {
        match self {
            Node::Dir(_) => FileType::Directory,
            Node::File { .. } => FileType::Regular,
            Node::Symlink(_) => FileType::Symlink,
        }
    }

    /// What a hard link to this member becomes: the target's blob and mode
    /// (git-utils.cc:1348-1364). A directory has none.
    fn copy(&self) -> Option<Node> {
        match self {
            Node::Dir(_) => None,
            Node::File { at, exec } => Some(Node::File {
                at: *at,
                exec: *exec,
            }),
            Node::Symlink(t) => Some(Node::Symlink(t.clone())),
        }
    }
}

/// A spooled archive and the index of its members.
#[derive(Debug)]
pub struct Tree {
    spool: File,
    sparse: Option<File>,
    root: Node,
}

/// Version byte of the serialized index. A file that starts with anything else
/// is not read; the entry holding it is wiped (`crate::treestore`).
pub const INDEX_VERSION: u8 = 1;

fn bad(why: String) -> Error {
    Error::Store(why)
}

fn show(parts: &[Vec<u8>]) -> String {
    parts
        .iter()
        .map(|p| String::from_utf8_lossy(p).into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// A file in TMPDIR, unlinked as soon as it is open: the bytes live exactly as
/// long as the handle, whatever happens to the process.
pub fn scratch(tag: &str) -> Result<File> {
    let path = std::env::temp_dir().join(format!("ietsp-spool-{}-{tag}", std::process::id()));
    let file = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)
        .map_err(|e| Error::io(format!("cannot create {}", path.display()), e))?;
    std::fs::remove_file(&path)
        .map_err(|e| Error::io(format!("cannot unlink {}", path.display()), e))?;
    Ok(file)
}

fn parts_of(rel: &Path) -> Parts {
    rel.components()
        .map(|c| c.as_os_str().as_bytes().to_vec())
        .collect()
}

fn find(d: &Dir, name: &[u8]) -> std::result::Result<usize, usize> {
    d.binary_search_by(|(k, _)| k.as_slice().cmp(name))
}

/// The last copy of a member wins (git-utils.cc:1213), except that a directory
/// listed again keeps what is already under it.
fn put(d: &mut Dir, name: &[u8], node: Node) {
    match find(d, name) {
        Ok(i) => {
            if !(matches!(node, Node::Dir(_)) && matches!(d[i].1, Node::Dir(_))) {
                d[i].1 = node;
            }
        }
        Err(i) => d.insert(i, (name.to_vec(), node)),
    }
}

/// Puts `node` at `parts`, with the missing parents created. A parent that is
/// not a directory is replaced, as `make_parents` replaces it; one that is a
/// symlink is refused (`ARCHIVE_EXTRACT_SECURE_SYMLINKS`).
fn insert(at: &mut Dir, parts: &[Vec<u8>], full: &[Vec<u8>], node: Node) -> Result<()> {
    let (name, rest) = parts.split_first().expect("a non-empty member path");
    if rest.is_empty() {
        put(at, name, node);
        return Ok(());
    }
    let i = match find(at, name) {
        Ok(i) => i,
        Err(i) => {
            at.insert(i, (name.clone(), Node::Dir(Dir::new())));
            i
        }
    };
    if let Node::Symlink(_) = at[i].1 {
        return Err(bad(format!(
            "tarball member '{}' is under the symlink '{}'",
            show(full),
            String::from_utf8_lossy(name)
        )));
    }
    if !matches!(at[i].1, Node::Dir(_)) {
        at[i].1 = Node::Dir(Dir::new());
    }
    let Node::Dir(d) = &mut at[i].1 else {
        unreachable!("a directory")
    };
    insert(d, rest, full, node)
}

fn lookup<'a>(root: &'a Dir, parts: &[Vec<u8>]) -> Option<&'a Node> {
    let (last, parents) = parts.split_last()?;
    let mut at = root;
    for p in parents {
        match find(at, p).ok().map(|i| &at[i].1) {
            Some(Node::Dir(d)) => at = d,
            _ => return None,
        }
    }
    find(at, last).ok().map(|i| &at[i].1)
}

impl Tree {
    /// Indexes the decompressed tar in `spool`, which is read from the start and
    /// kept for `dump`.
    pub fn index(spool: File) -> Result<Tree> {
        let mut root = Dir::new();
        let mut links: Vec<(Parts, Parts)> = Vec::new();
        let mut sparse: Option<File> = None;
        let mut sparse_at = 0u64;
        {
            let mut src = &spool;
            src.rewind()
                .map_err(|e| Error::io("cannot read the spooled tarball", e))?;
            let mut archive = tar::Archive::new(src);
            let entries = archive
                .entries_with_seek()
                .map_err(|e| Error::io("cannot read the tarball", e))?;
            for entry in entries {
                let mut entry = entry.map_err(|e| Error::io("cannot read the tarball", e))?;
                let kind = entry.header().entry_type();
                // libarchive reads the pax global header as archive metadata,
                // not as a member; GitHub's tarballs start with one.
                if kind.is_pax_global_extensions() {
                    continue;
                }
                let name = entry
                    .path()
                    .map_err(|e| Error::io("cannot read a tarball member name", e))?
                    .into_owned();
                let parts = parts_of(&super::unpack::member_path(&name)?);
                if parts.is_empty() {
                    continue;
                }
                if kind.is_dir() {
                    insert(&mut root, &parts, &parts, Node::Dir(Dir::new()))?;
                } else if kind.is_symlink() {
                    let target = link_target(&mut entry, &parts, "symlink")?;
                    insert(&mut root, &parts, &parts, Node::Symlink(target))?;
                } else if kind.is_hard_link() {
                    let target = link_target(&mut entry, &parts, "link")?;
                    let target = super::unpack::member_path(Path::new(
                        std::ffi::OsStr::from_bytes(&target),
                    ))?;
                    links.push((parts, parts_of(&target)));
                } else if kind.is_file() || kind.is_contiguous() || kind.is_gnu_sparse() {
                    let exec = entry.header().mode().unwrap_or(0o644) & 0o100 != 0;
                    let at = if kind.is_gnu_sparse() {
                        // The holes are not in the archive, so this is the one
                        // member whose bytes must be written out to be indexed.
                        let side = match &mut sparse {
                            Some(f) => f,
                            None => sparse.insert(scratch("sparse")?),
                        };
                        side.seek(SeekFrom::Start(sparse_at))
                            .and_then(|_| io::copy(&mut entry, side))
                            .map(|size| {
                                let at = Blob {
                                    offset: sparse_at,
                                    size,
                                    sparse: true,
                                };
                                sparse_at += size;
                                at
                            })
                            .map_err(|e| {
                                Error::io(
                                    format!("cannot read '{}' from the tarball", show(&parts)),
                                    e,
                                )
                            })?
                    } else {
                        Blob {
                            offset: entry.raw_file_position(),
                            size: entry.size(),
                            sparse: false,
                        }
                    };
                    insert(&mut root, &parts, &parts, Node::File { at, exec })?;
                } else {
                    return Err(bad(format!(
                        "file '{}' in tarball has unsupported file type {kind:?}",
                        name.display()
                    )));
                }
            }
        }
        // The tree path resolves the hard links last, so a link wins over a
        // regular member at the same name wherever the two sit in the archive.
        for (parts, target) in links {
            let copy = match lookup(&root, &target) {
                Some(node) => node.copy().ok_or_else(|| {
                    bad(format!(
                        "tarball member '{}' is a hard link to the directory '{}'",
                        show(&parts),
                        show(&target)
                    ))
                })?,
                None => {
                    return Err(bad(format!(
                        "tarball member '{}' is a hard link to '{}', which the archive does not hold",
                        show(&parts),
                        show(&target)
                    )));
                }
            };
            insert(&mut root, &parts, &parts, copy)?;
        }
        Ok(Tree {
            spool,
            sparse,
            root: Node::Dir(dereference(root)),
        })
    }

    fn node(&self, n: &Node, sink: &mut dyn Write, depth: usize) -> io::Result<()> {
        if depth >= NAR_MAX_DEPTH {
            return Err(io::Error::other(format!(
                "the tarball exceeds the maximum NAR directory depth of {NAR_MAX_DEPTH}"
            )));
        }
        write_str(sink, b"(")?;
        write_str(sink, b"type")?;
        match n {
            Node::Dir(d) => {
                write_str(sink, b"directory")?;
                for (name, child) in d {
                    write_str(sink, b"entry")?;
                    write_str(sink, b"(")?;
                    write_str(sink, b"name")?;
                    write_str(sink, name)?;
                    write_str(sink, b"node")?;
                    self.node(child, sink, depth + 1)?;
                    write_str(sink, b")")?;
                }
            }
            Node::File { at, exec } => {
                write_str(sink, b"regular")?;
                if *exec {
                    write_str(sink, b"executable")?;
                    write_str(sink, b"")?;
                }
                write_str(sink, b"contents")?;
                write_u64(sink, at.size)?;
                self.copy_blob(at, sink)?;
                write_padding(sink, at.size)?;
            }
            Node::Symlink(target) => {
                write_str(sink, b"symlink")?;
                write_str(sink, b"target")?;
                write_str(sink, target)?;
            }
        }
        write_str(sink, b")")
    }

    /// A member's bytes, by `pread`: no seek, so two threads may read one tree
    /// at once.
    fn source(&self, at: &Blob) -> &File {
        if at.sparse {
            self.sparse.as_ref().expect("a sparse side file")
        } else {
            &self.spool
        }
    }

    fn copy_blob(&self, at: &Blob, sink: &mut dyn Write) -> io::Result<()> {
        let src = self.source(at);
        let mut buf = vec![0u8; CHUNK.min(at.size.max(1) as usize)];
        let mut done = 0u64;
        while done < at.size {
            let n = ((at.size - done) as usize).min(buf.len());
            src.read_exact_at(&mut buf[..n], at.offset + done)
                .map_err(|_| io::Error::other("the spooled tarball is truncated"))?;
            sink.write_all(&buf[..n])?;
            done += n as u64;
        }
        Ok(())
    }

    // ---- the accessor evaluation reads through ------------------------------

    /// The member at the resolved components `at`, without following a symlink.
    fn at(&self, at: &[Vec<u8>]) -> Option<&Node> {
        let mut n = &self.root;
        for name in at {
            let Node::Dir(d) = n else { return None };
            n = find(d, name).ok().map(|i| &d[i].1)?;
        }
        Some(n)
    }

    /// The member at `rel` (`b""` is the tree itself), with symlinks followed
    /// the way the kernel follows them for a path under `/nix/store`;
    /// `follow_last` is false for an `lstat`. A target that leaves the tree is
    /// not resolved: the spool holds no bytes for it.
    pub fn resolve(&self, rel: &[u8], follow_last: bool) -> Option<&Node> {
        let split = |p: &[u8]| -> Vec<Vec<u8>> {
            p.split(|&b| b == b'/')
                .filter(|c| !c.is_empty() && c != b".")
                .map(<[u8]>::to_vec)
                .collect()
        };
        let mut todo = split(rel);
        todo.reverse();
        let mut at: Vec<Vec<u8>> = Vec::new();
        let mut hops = 0;
        while let Some(c) = todo.pop() {
            if c == b".." {
                at.pop()?;
                continue;
            }
            at.push(c);
            if let Some(Node::Symlink(target)) = self.at(&at) {
                if todo.is_empty() && !follow_last {
                    break;
                }
                hops += 1;
                if hops > 32 || target.starts_with(b"/") {
                    return None;
                }
                at.pop();
                let mut parts = split(target);
                parts.reverse();
                todo.extend(parts);
            }
        }
        self.at(&at)
    }

    /// A regular member's whole contents, in one `pread` into one buffer: no
    /// `open`, no copy through a staging buffer, so a read of a member costs
    /// less than a read of the same file under `/nix/store`.
    pub fn read(&self, at: &Blob) -> io::Result<Vec<u8>> {
        let mut out = vec![0u8; at.size as usize];
        self.source(at)
            .read_exact_at(&mut out, at.offset)
            .map_err(|_| io::Error::other("the spooled tarball is truncated"))?;
        Ok(out)
    }

    /// The NAR of `node`, with `filter` asked about every entry below it under
    /// the path Nix would show (`shown`), as `nar::dump_path` asks.
    pub fn dump_filtered(
        &self,
        node: &Node,
        shown: &Path,
        sink: &mut dyn Write,
        filter: &mut PathFilter<'_>,
    ) -> io::Result<()> {
        write_str(sink, b"nix-archive-1")?;
        self.filtered(node, shown, sink, filter, 0)
    }

    fn filtered(
        &self,
        n: &Node,
        shown: &Path,
        sink: &mut dyn Write,
        filter: &mut PathFilter<'_>,
        depth: usize,
    ) -> io::Result<()> {
        let Node::Dir(d) = n else {
            return self.node(n, sink, depth);
        };
        if depth >= NAR_MAX_DEPTH {
            return Err(io::Error::other(format!(
                "the tarball exceeds the maximum NAR directory depth of {NAR_MAX_DEPTH}"
            )));
        }
        write_str(sink, b"(")?;
        write_str(sink, b"type")?;
        write_str(sink, b"directory")?;
        for (name, child) in d {
            let child_path = shown.join(std::ffi::OsStr::from_bytes(name));
            if !filter(&child_path, child.file_type()) {
                continue;
            }
            write_str(sink, b"entry")?;
            write_str(sink, b"(")?;
            write_str(sink, b"name")?;
            write_str(sink, name)?;
            write_str(sink, b"node")?;
            self.filtered(child, &child_path, sink, filter, depth + 1)?;
            write_str(sink, b")")?;
        }
        write_str(sink, b")")
    }

    // ---- the persisted index -----------------------------------------------

    /// Writes `index` into `dir`, and the expanded sparse members beside it when
    /// the archive had any. The spool itself is `dir/spool`, written by the
    /// caller as the download arrived. `nar` is the NAR hash and size the fetch
    /// verified against the lock, so no reader ever hashes the tree again.
    pub fn persist(&self, dir: &Path, nar: (&Hash, u64)) -> io::Result<()> {
        if let Some(sparse) = &self.sparse {
            let mut src = sparse.try_clone()?;
            src.rewind()?;
            let mut out = File::create(dir.join(SPARSE))?;
            io::copy(&mut src, &mut out)?;
            out.sync_all()?;
        }
        let bytes = self.encode_index(self.spool.metadata()?.len(), nar);
        std::fs::write(dir.join(INDEX), bytes)
    }

    fn encode_index(&self, spool_len: u64, nar: (&Hash, u64)) -> Vec<u8> {
        let mut e = Vec::with_capacity(1 << 16);
        e.push(INDEX_VERSION);
        put_bytes(&mut e, nar.0.bytes());
        put_u64(&mut e, nar.1);
        put_u64(&mut e, spool_len);
        match &self.sparse {
            Some(f) => {
                e.push(1);
                put_u64(&mut e, f.metadata().map_or(0, |m| m.len()));
            }
            None => e.push(0),
        }
        put_node(&mut e, &self.root);
        e
    }

    /// Reopens a persisted entry: the spool, its sparse side file and the index,
    /// with the NAR hash and size the fetch recorded. `None` on anything
    /// unexpected, so the caller wipes the entry rather than repairing it.
    pub fn reopen(dir: &Path) -> Option<(Tree, Hash, u64)> {
        let bytes = std::fs::read(dir.join(INDEX)).ok()?;
        let mut d = Idx { b: &bytes, i: 0 };
        if d.u8()? != INDEX_VERSION {
            return None;
        }
        let nar_hash = Hash::from_bytes(HashAlgo::Sha256, &d.bytes()?).ok()?;
        let nar_size = d.u64()?;
        let spool_len = d.u64()?;
        let sparse_len = match d.u8()? {
            0 => None,
            1 => Some(d.u64()?),
            _ => return None,
        };
        let root = get_node(&mut d, 0)?;
        if d.i != bytes.len() {
            return None;
        }
        let spool = File::open(dir.join(SPOOL)).ok()?;
        if spool.metadata().ok()?.len() != spool_len {
            return None;
        }
        let sparse = match sparse_len {
            None => None,
            Some(len) => {
                let f = File::open(dir.join(SPARSE)).ok()?;
                if f.metadata().ok()?.len() != len {
                    return None;
                }
                Some(f)
            }
        };
        Some((
            Tree {
                spool,
                sparse,
                root,
            },
            nar_hash,
            nar_size,
        ))
    }
}

/// The three files a persisted entry holds.
pub const SPOOL: &str = "spool";
pub const INDEX: &str = "index";
pub const SPARSE: &str = "sparse";

/// The spool of a staged entry, created for writing and kept for reading.
pub fn spool_file(dir: &Path) -> io::Result<File> {
    File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(dir.join(SPOOL))
}

fn put_u64(e: &mut Vec<u8>, mut n: u64) {
    loop {
        let b = (n & 0x7f) as u8;
        n >>= 7;
        if n == 0 {
            e.push(b);
            return;
        }
        e.push(b | 0x80);
    }
}

fn put_bytes(e: &mut Vec<u8>, b: &[u8]) {
    put_u64(e, b.len() as u64);
    e.extend_from_slice(b);
}

fn put_node(e: &mut Vec<u8>, n: &Node) {
    match n {
        Node::Dir(d) => {
            e.push(0);
            put_u64(e, d.len() as u64);
            for (name, child) in d {
                put_bytes(e, name);
                put_node(e, child);
            }
        }
        Node::File { at, exec } => {
            e.push(1);
            e.push(u8::from(*exec) | (u8::from(at.sparse) << 1));
            put_u64(e, at.offset);
            put_u64(e, at.size);
        }
        Node::Symlink(t) => {
            e.push(2);
            put_bytes(e, t);
        }
    }
}

struct Idx<'a> {
    b: &'a [u8],
    i: usize,
}

impl Idx<'_> {
    fn u8(&mut self) -> Option<u8> {
        let b = *self.b.get(self.i)?;
        self.i += 1;
        Some(b)
    }

    fn u64(&mut self) -> Option<u64> {
        let mut n = 0u64;
        let mut shift = 0;
        loop {
            let b = self.u8()?;
            if shift >= 64 {
                return None;
            }
            n |= u64::from(b & 0x7f) << shift;
            if b & 0x80 == 0 {
                return Some(n);
            }
            shift += 7;
        }
    }

    fn bytes(&mut self) -> Option<Vec<u8>> {
        let n = self.u64()? as usize;
        let s = self.b.get(self.i..self.i.checked_add(n)?)?;
        self.i += n;
        Some(s.to_vec())
    }
}

fn get_node(d: &mut Idx<'_>, depth: usize) -> Option<Node> {
    if depth > NAR_MAX_DEPTH {
        return None;
    }
    Some(match d.u8()? {
        0 => {
            let n = d.u64()? as usize;
            if n > d.b.len().saturating_sub(d.i) {
                return None;
            }
            let mut dir = Dir::with_capacity(n);
            for _ in 0..n {
                let name = d.bytes()?;
                dir.push((name, get_node(d, depth + 1)?));
            }
            if dir.windows(2).any(|w| w[0].0 >= w[1].0) {
                return None;
            }
            Node::Dir(dir)
        }
        1 => {
            let flags = d.u8()?;
            Node::File {
                at: Blob {
                    offset: d.u64()?,
                    size: d.u64()?,
                    sparse: flags & 2 != 0,
                },
                exec: flags & 1 != 0,
            }
        }
        2 => Node::Symlink(d.bytes()?),
        _ => return None,
    })
}

fn link_target(
    entry: &mut tar::Entry<'_, impl Read>,
    parts: &[Vec<u8>],
    what: &str,
) -> Result<Vec<u8>> {
    let target = entry
        .link_name()
        .map_err(|e| Error::io(format!("cannot read a {what} target"), e))?
        .ok_or_else(|| bad(format!("tarball {what} '{}' has no target", show(parts))))?;
    Ok(target.as_os_str().as_bytes().to_vec())
}

/// git-utils.cc:dereferenceSingletonDirectory: one entry, and a directory.
fn dereference(mut root: Dir) -> Dir {
    if root.len() == 1 && matches!(root[0].1, Node::Dir(_)) {
        match root.pop() {
            Some((_, Node::Dir(d))) => d,
            _ => unreachable!("a directory"),
        }
    } else {
        root
    }
}

impl Tree {
    pub fn dump(&self, sink: &mut dyn Write) -> io::Result<()> {
        write_str(sink, b"nix-archive-1")?;
        self.node(&self.root, sink, 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIR: tar::EntryType = tar::EntryType::Directory;
    const REG: tar::EntryType = tar::EntryType::Regular;
    const SYM: tar::EntryType = tar::EntryType::Symlink;
    const HARD: tar::EntryType = tar::EntryType::Link;

    fn tar_gz(entries: &[(&str, tar::EntryType, u32, &[u8])]) -> Vec<u8> {
        let mut b = tar::Builder::new(Vec::new());
        for (name, kind, mode, data) in entries {
            let mut h = tar::Header::new_ustar();
            h.set_entry_type(*kind);
            h.set_mode(*mode);
            h.set_size(if *kind == REG { data.len() as u64 } else { 0 });
            if *kind == SYM || *kind == HARD {
                h.set_link_name(std::str::from_utf8(data).unwrap()).unwrap();
            }
            let body: &[u8] = if *kind == REG { data } else { &[] };
            b.append_data(&mut h, name, body).unwrap();
        }
        let tar = b.into_inner().unwrap();
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut gz, &tar).unwrap();
        gz.finish().unwrap()
    }

    /// The NAR the spool writes for `bytes`, and the NAR the tree path writes
    /// for the same archive.
    fn both(tag: &str, bytes: Vec<u8>) -> (Result<Vec<u8>>, Result<Vec<u8>>) {
        let plain = crate::fetch::http::decompress(bytes.clone()).unwrap();
        let mut spool = scratch(tag).unwrap();
        spool.write_all(&plain).unwrap();
        let streamed = Tree::index(spool).and_then(|t| {
            let mut nar = Vec::new();
            t.dump(&mut nar)
                .map_err(|e| Error::io("cannot dump the spool", e))?;
            Ok(nar)
        });

        let dest =
            std::env::temp_dir().join(format!("ietsp-spool-tree-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dest);
        let tree = crate::fetch::unpack::unpack(plain, &dest).and_then(|root| {
            let mut nar = Vec::new();
            crate::store::nar::dump_path(&root, &mut nar, &mut |_, _| true)
                .map_err(|e| Error::io("cannot dump the tree", e))?;
            Ok(nar)
        });
        let _ = std::fs::remove_dir_all(&dest);
        (streamed, tree)
    }

    fn same(tag: &str, entries: &[(&str, tar::EntryType, u32, &[u8])]) -> Vec<u8> {
        let (streamed, tree) = both(tag, tar_gz(entries));
        let streamed = streamed.unwrap_or_else(|e| panic!("{tag}: the spool path failed: {e}"));
        let tree = tree.unwrap_or_else(|e| panic!("{tag}: the tree path failed: {e}"));
        assert_eq!(streamed, tree, "{tag}: the two NARs differ");
        streamed
    }

    /// Every rule the git sink applies, both ways, byte for byte.
    #[test]
    fn spool_matches_the_tree() {
        let big = vec![b'z'; 70 * 1024];
        same(
            "rules",
            &[
                (
                    "pax_global_header",
                    tar::EntryType::XGlobalHeader,
                    0o644,
                    b"",
                ),
                ("repo-abc/", DIR, 0o755, b""),
                ("repo-abc/flake.nix", REG, 0o644, b"{}\n"),
                ("repo-abc/run", REG, 0o755, b"#!/bin/sh\n"),
                ("repo-abc/empty/", DIR, 0o700, b""),
                ("repo-abc/link", SYM, 0o777, b"flake.nix"),
                ("repo-abc/dup", REG, 0o644, b"first"),
                ("repo-abc/dup", REG, 0o755, b"second"),
                ("repo-abc/same", HARD, 0o644, b"repo-abc/run"),
                ("repo-abc/deep/a/b/c", REG, 0o644, b"nested\n"),
                ("repo-abc/big", REG, 0o644, &big),
            ],
        );
    }

    /// One leading directory is stripped; two top-level entries are not.
    #[test]
    fn spool_matches_the_tree_on_the_top_level() {
        let one = same(
            "single",
            &[("t/", DIR, 0o755, b""), ("t/f", REG, 0o644, b"x")],
        );
        let two = same(
            "double",
            &[
                ("t/", DIR, 0o755, b""),
                ("t/f", REG, 0o644, b"x"),
                ("g", REG, 0o644, b"y"),
            ],
        );
        assert_ne!(one, two);
        same("file-at-top", &[("only", REG, 0o644, b"x")]);
    }

    /// An empty archive is the NAR of an empty directory.
    #[test]
    fn an_empty_archive_is_an_empty_directory() {
        let nar = same("empty", &[]);
        let dest = std::env::temp_dir().join(format!("ietsp-spool-bare-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dest);
        std::fs::create_dir_all(&dest).unwrap();
        let mut want = Vec::new();
        crate::store::nar::dump_path(&dest, &mut want, &mut |_, _| true).unwrap();
        assert_eq!(nar, want);
        std::fs::remove_dir_all(&dest).unwrap();
    }

    /// The same errors as the tree path, for the same archives.
    #[test]
    fn the_errors_match() {
        for (tag, entries, why) in [
            (
                "fifo",
                vec![("dev", tar::EntryType::Fifo, 0o644u32, &b""[..])],
                "unsupported file type",
            ),
            (
                "dangling-link",
                vec![("l", HARD, 0o644u32, &b"missing"[..])],
                "hard link",
            ),
            (
                "under-symlink",
                vec![
                    ("t", SYM, 0o777u32, &b"/etc"[..]),
                    ("t/f", REG, 0o644, &b"x"[..]),
                ],
                "symlink",
            ),
            (
                "link-to-dir",
                vec![
                    ("d/", DIR, 0o755u32, &b""[..]),
                    ("l", HARD, 0o644, &b"d"[..]),
                    ("k", REG, 0o644, &b"x"[..]),
                ],
                "hard link",
            ),
        ] {
            let (streamed, tree) = both(tag, tar_gz(&entries));
            let e = streamed.unwrap_err().to_string();
            assert!(e.contains(why), "{tag}: {e}");
            assert!(tree.is_err(), "{tag}: the tree path accepted it");
        }
    }

    /// The fixture whose `narHash` a lock file records, hashed out of the spool.
    #[test]
    fn nar_hash_of_the_fixture() {
        let bytes = std::fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/tarball/tree.tar.gz"),
        )
        .unwrap();
        let (streamed, tree) = both("golden", bytes);
        let streamed = streamed.unwrap();
        assert_eq!(streamed, tree.unwrap());
        let want = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/tarball/narHash"),
        )
        .unwrap();
        let mut h = crate::store::hash::Hasher::new(crate::store::hash::HashAlgo::Sha256);
        h.write_all(&streamed).unwrap();
        assert_eq!(h.finish().to_sri(), want.trim());
    }
}

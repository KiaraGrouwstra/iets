use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use super::hash::{Hash, HashAlgo, Hasher};

/// The Nix type name passed to `builtins.filterSource` filters (primops.cc:fileTypeToString).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileType {
    Regular,
    Directory,
    Symlink,
    Unknown,
}

impl FileType {
    pub fn nix_name(self) -> &'static str {
        match self {
            FileType::Regular => "regular",
            FileType::Directory => "directory",
            FileType::Symlink => "symlink",
            FileType::Unknown => "unknown",
        }
    }

    pub(crate) fn of(ft: fs::FileType) -> FileType {
        if ft.is_symlink() {
            FileType::Symlink
        } else if ft.is_dir() {
            FileType::Directory
        } else if ft.is_file() {
            FileType::Regular
        } else {
            FileType::Unknown
        }
    }
}

/// Filter called on each directory entry with its absolute path; `false` skips it. Mirrors
/// archive.hh:PathFilter as used by primops.cc:addPath.
pub type PathFilter<'a> = dyn FnMut(&Path, FileType) -> bool + 'a;

const NAR_MAX_DEPTH: usize = 64;

fn write_u64(sink: &mut dyn Write, n: u64) -> io::Result<()> {
    sink.write_all(&n.to_le_bytes())
}

fn write_padding(sink: &mut dyn Write, len: u64) -> io::Result<()> {
    let rem = (len % 8) as usize;
    if rem != 0 {
        sink.write_all(&[0u8; 8][..8 - rem])?;
    }
    Ok(())
}

/// Mirrors serialise.hh:operator<<(Sink, std::string_view): length-prefixed, zero-padded to 8.
fn write_str(sink: &mut dyn Write, s: &[u8]) -> io::Result<()> {
    write_u64(sink, s.len() as u64)?;
    sink.write_all(s)?;
    write_padding(sink, s.len() as u64)
}

/// Mirrors archive.cc:SourceAccessor::dumpPath; writes the NAR of `path` into `sink`.
pub fn dump_path(path: &Path, sink: &mut dyn Write, filter: &mut PathFilter<'_>) -> io::Result<()> {
    write_str(sink, b"nix-archive-1")?;
    dump(path, sink, filter, 0)
}

fn dump(
    path: &Path,
    sink: &mut dyn Write,
    filter: &mut PathFilter<'_>,
    depth: usize,
) -> io::Result<()> {
    if depth >= NAR_MAX_DEPTH {
        return Err(io::Error::other(format!(
            "path '{}' exceeds maximum NAR directory depth of {NAR_MAX_DEPTH}",
            path.display()
        )));
    }
    let st = fs::symlink_metadata(path)?;
    let ft = st.file_type();
    write_str(sink, b"(")?;
    if ft.is_file() {
        write_str(sink, b"type")?;
        write_str(sink, b"regular")?;
        if st.permissions().mode() & 0o100 != 0 {
            write_str(sink, b"executable")?;
            write_str(sink, b"")?;
        }
        write_str(sink, b"contents")?;
        write_u64(sink, st.len())?;
        let mut f = fs::File::open(path)?;
        let copied = io::copy(&mut f, sink)?;
        if copied != st.len() {
            return Err(io::Error::other(format!(
                "file '{}' changed size while reading",
                path.display()
            )));
        }
        write_padding(sink, copied)?;
    } else if ft.is_dir() {
        write_str(sink, b"type")?;
        write_str(sink, b"directory")?;
        let mut entries: Vec<(Vec<u8>, fs::FileType)> = fs::read_dir(path)?
            .map(|e| {
                let e = e?;
                Ok((e.file_name().as_bytes().to_vec(), e.file_type()?))
            })
            .collect::<io::Result<_>>()?;
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        for (name, eft) in entries {
            let child = path.join(std::ffi::OsStr::from_bytes(&name));
            if !filter(&child, FileType::of(eft)) {
                continue;
            }
            write_str(sink, b"entry")?;
            write_str(sink, b"(")?;
            write_str(sink, b"name")?;
            write_str(sink, &name)?;
            write_str(sink, b"node")?;
            dump(&child, sink, filter, depth + 1)?;
            write_str(sink, b")")?;
        }
    } else if ft.is_symlink() {
        write_str(sink, b"type")?;
        write_str(sink, b"symlink")?;
        write_str(sink, b"target")?;
        write_str(sink, fs::read_link(path)?.as_os_str().as_bytes())?;
    } else {
        return Err(io::Error::other(format!(
            "file '{}' has an unsupported type",
            path.display()
        )));
    }
    write_str(sink, b")")
}

/// Mirrors archive.cc:dumpString: the NAR of a regular, non-executable file with `contents`.
pub fn dump_string(contents: &[u8], sink: &mut dyn Write) -> io::Result<()> {
    write_str(sink, b"nix-archive-1")?;
    write_str(sink, b"(")?;
    write_str(sink, b"type")?;
    write_str(sink, b"regular")?;
    write_str(sink, b"contents")?;
    write_str(sink, contents)?;
    write_str(sink, b")")
}

/// Mirrors store-api.cc:hashPath for NixArchive ingestion, with a filter; returns (hash, NAR size).
pub fn nar_hash_filtered(
    path: &Path,
    algo: HashAlgo,
    filter: &mut PathFilter<'_>,
) -> io::Result<(Hash, u64)> {
    let mut h = Hasher::new(algo);
    dump_path(path, &mut h, filter)?;
    let size = h.bytes_written();
    Ok((h.finish(), size))
}

/// `nix hash path PATH`: sha256 of the NAR of `path`.
pub fn nar_hash(path: &Path) -> io::Result<Hash> {
    Ok(nar_hash_filtered(path, HashAlgo::Sha256, &mut |_, _| true)?.0)
}

/// `nix-store --dump PATH | wc -c`.
pub fn nar_size(path: &Path) -> io::Result<u64> {
    Ok(nar_hash_filtered(path, HashAlgo::Sha256, &mut |_, _| true)?.1)
}

/// Mirrors store-api.cc:hashPath for Flat ingestion: hash of the file bytes.
pub fn flat_hash(path: &Path, algo: HashAlgo) -> io::Result<Hash> {
    let mut h = Hasher::new(algo);
    io::copy(&mut fs::File::open(path)?, &mut h)?;
    Ok(h.finish())
}

fn read_u64(source: &mut dyn Read) -> io::Result<u64> {
    let mut b = [0u8; 8];
    source.read_exact(&mut b)?;
    Ok(u64::from_le_bytes(b))
}

fn read_padding(source: &mut dyn Read, len: u64) -> io::Result<()> {
    let rem = (len % 8) as usize;
    if rem != 0 {
        let mut pad = [0u8; 8];
        source.read_exact(&mut pad[..8 - rem])?;
        if pad.iter().any(|&b| b != 0) {
            return Err(bad_nar("non-zero padding"));
        }
    }
    Ok(())
}

fn bad_nar(why: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("bad NAR: {why}"))
}

/// Mirrors serialise.hh:readString with a size cap: length-prefixed, zero-padded to 8.
fn read_str(source: &mut dyn Read, max: u64) -> io::Result<Vec<u8>> {
    let len = read_u64(source)?;
    if len > max {
        return Err(bad_nar("string too long"));
    }
    let mut buf = vec![0u8; len as usize];
    source.read_exact(&mut buf)?;
    read_padding(source, len)?;
    Ok(buf)
}

fn expect_str(source: &mut dyn Read, want: &[u8]) -> io::Result<()> {
    let got = read_str(source, 64)?;
    if got != want {
        return Err(bad_nar(&format!(
            "expected '{}', got '{}'",
            String::from_utf8_lossy(want),
            String::from_utf8_lossy(&got)
        )));
    }
    Ok(())
}

/// Mirrors archive.cc:parseDump with RestoreSink: restores the NAR read from `source` at
/// `dest`, which must not exist. Byte-exact inverse of `dump_path`.
pub fn restore_path(source: &mut dyn Read, dest: &Path) -> io::Result<()> {
    expect_str(source, b"nix-archive-1")?;
    restore(source, dest, 0)
}

fn restore(source: &mut dyn Read, dest: &Path, depth: usize) -> io::Result<()> {
    if depth >= NAR_MAX_DEPTH {
        return Err(bad_nar("directory nesting too deep"));
    }
    expect_str(source, b"(")?;
    expect_str(source, b"type")?;
    let ty = read_str(source, 64)?;
    match ty.as_slice() {
        b"regular" => {
            let mut tag = read_str(source, 64)?;
            let mut executable = false;
            if tag == b"executable" {
                expect_str(source, b"")?;
                executable = true;
                tag = read_str(source, 64)?;
            }
            if tag != b"contents" {
                return Err(bad_nar("expected 'contents'"));
            }
            let len = read_u64(source)?;
            let mut f = fs::File::create(dest)?;
            let copied = io::copy(&mut source.take(len), &mut f)?;
            if copied != len {
                return Err(bad_nar("truncated file contents"));
            }
            read_padding(source, len)?;
            if executable {
                f.set_permissions(fs::Permissions::from_mode(0o755))?;
            }
        }
        b"directory" => {
            fs::create_dir(dest)?;
            let mut prev: Vec<u8> = Vec::new();
            loop {
                let tag = read_str(source, 64)?;
                if tag == b")" {
                    return Ok(());
                }
                if tag != b"entry" {
                    return Err(bad_nar("expected 'entry' or ')'"));
                }
                expect_str(source, b"(")?;
                expect_str(source, b"name")?;
                let name = read_str(source, 4096)?;
                if name.is_empty()
                    || name == b"."
                    || name == b".."
                    || name.contains(&b'/')
                    || name.contains(&0)
                {
                    return Err(bad_nar("bad entry name"));
                }
                if name <= prev {
                    return Err(bad_nar("directory entries out of order"));
                }
                prev = name.clone();
                expect_str(source, b"node")?;
                restore(
                    source,
                    &dest.join(std::ffi::OsStr::from_bytes(&name)),
                    depth + 1,
                )?;
                expect_str(source, b")")?;
            }
        }
        b"symlink" => {
            expect_str(source, b"target")?;
            let target = read_str(source, 4096)?;
            std::os::unix::fs::symlink(std::ffi::OsStr::from_bytes(&target), dest)?;
        }
        _ => return Err(bad_nar("unknown node type")),
    }
    expect_str(source, b")")
}

/// Hashes `contents` as a NAR regular file (what `nix hash path` gives for that file).
pub fn nar_hash_of_string(contents: &[u8]) -> Hash {
    let mut h = Hasher::new(HashAlgo::Sha256);
    dump_string(contents, &mut h).expect("hasher never fails");
    h.finish()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use std::path::PathBuf;

    /// Builds the fixture tree used by the shell commands in the test comments:
    /// mkdir -p tree/sub; printf 'hello world\n' > tree/a.txt;
    /// printf '#!/bin/sh\necho hi\n' > tree/run.sh; chmod +x tree/run.sh;
    /// ln -s a.txt tree/link; : > tree/sub/empty; printf zzz > tree/sub/z
    pub(crate) fn fixture_tree(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ietsp-nar-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let tree = dir.join("tree");
        fs::create_dir_all(tree.join("sub")).unwrap();
        fs::write(tree.join("a.txt"), b"hello world\n").unwrap();
        fs::write(tree.join("run.sh"), b"#!/bin/sh\necho hi\n").unwrap();
        fs::set_permissions(tree.join("run.sh"), fs::Permissions::from_mode(0o755)).unwrap();
        symlink("a.txt", tree.join("link")).unwrap();
        fs::write(tree.join("sub/empty"), b"").unwrap();
        fs::write(tree.join("sub/z"), b"zzz").unwrap();
        tree
    }

    // nix-store --dump tree | sha256sum ; nix-store --dump tree | wc -c
    #[test]
    fn directory_nar() {
        let tree = fixture_tree("dir");
        assert_eq!(
            nar_hash(&tree).unwrap().to_base16(),
            "9db4d4d873838f1fbb8f363841597d61244beebb19bd10b606ae2a6d4b940ede"
        );
        assert_eq!(nar_size(&tree).unwrap(), 1272);
    }

    // nix hash path tree ; nix hash path --base32 tree
    #[test]
    fn directory_nar_hash_formats() {
        let tree = fixture_tree("fmt");
        let h = nar_hash(&tree).unwrap();
        assert_eq!(
            h.to_sri(),
            "sha256-nbTU2HODjx+7jzY4QVl9YSRL7rsZvRC2Bq4qbUuUDt4="
        );
        assert_eq!(
            h.to_nix32(),
            "1phfji5nsamf0sv11g8rpgp4n931gmcl2f1niyxiz3w3fgcd9d4x"
        );
    }

    // nix-store --dump tree/a.txt | sha256sum
    #[test]
    fn regular_file_nar() {
        let tree = fixture_tree("file");
        let expected = "34ca3ac63094d1d5751f741101692a78f95eedf10744b088129fc324dfd0f603";
        assert_eq!(nar_hash(&tree.join("a.txt")).unwrap().to_base16(), expected);
        assert_eq!(nar_hash_of_string(b"hello world\n").to_base16(), expected);
    }

    // nix-store --dump tree/run.sh | sha256sum
    #[test]
    fn executable_file_nar() {
        let tree = fixture_tree("exec");
        assert_eq!(
            nar_hash(&tree.join("run.sh")).unwrap().to_base16(),
            "5e0accf02cedede5e4119ffa15e79e79a5fb1fb9bc43c3d434f33227a14477a0"
        );
    }

    // nix-store --dump tree/link | sha256sum
    #[test]
    fn symlink_nar() {
        let tree = fixture_tree("link");
        assert_eq!(
            nar_hash(&tree.join("link")).unwrap().to_base16(),
            "8d3c00cfa866e4d1b809772afeac240786246221eb2c574d69c4bba168834e81"
        );
    }

    // nix-store --dump tree | nix-store --restore out ; nix-store --dump out | sha256sum
    #[test]
    fn restore_roundtrip() {
        let tree = fixture_tree("restore");
        let mut nar = Vec::new();
        dump_path(&tree, &mut nar, &mut |_, _| true).unwrap();
        let out = tree.with_file_name("restored");
        restore_path(&mut nar.as_slice(), &out).unwrap();
        assert_eq!(nar_hash(&out).unwrap(), nar_hash(&tree).unwrap());
        assert_eq!(
            fs::read_link(out.join("link")).unwrap().as_os_str(),
            "a.txt"
        );
        assert_ne!(
            fs::metadata(out.join("run.sh"))
                .unwrap()
                .permissions()
                .mode()
                & 0o100,
            0
        );
        assert_eq!(
            fs::metadata(out.join("a.txt"))
                .unwrap()
                .permissions()
                .mode()
                & 0o100,
            0
        );
    }

    #[test]
    fn restore_rejects_garbage() {
        let out = std::env::temp_dir().join(format!("ietsp-nar-garbage-{}", std::process::id()));
        assert!(restore_path(&mut &b"nix-archive-2"[..], &out).is_err());
    }

    #[test]
    fn filter_sees_types_and_paths() {
        let tree = fixture_tree("filter");
        let mut seen = Vec::new();
        let (_, _) = nar_hash_filtered(&tree, HashAlgo::Sha256, &mut |p, t| {
            seen.push((p.strip_prefix(&tree).unwrap().to_path_buf(), t));
            true
        })
        .unwrap();
        let expect: Vec<(PathBuf, FileType)> = vec![
            ("a.txt".into(), FileType::Regular),
            ("link".into(), FileType::Symlink),
            ("run.sh".into(), FileType::Regular),
            ("sub".into(), FileType::Directory),
            ("sub/empty".into(), FileType::Regular),
            ("sub/z".into(), FileType::Regular),
        ];
        assert_eq!(seen, expect);
    }
}

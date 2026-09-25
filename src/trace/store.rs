//! The on-disk cache: one directory per key, holding one file that carries
//! every part of the entry. The file is written under a temporary name and
//! renamed into place, so one rename publishes the whole entry and a reader
//! that opened it holds one generation of it. `crate::cachefs` holds the
//! store-level structure (version, writer lock, sweeps) this shares with the
//! IR cache.

use std::fs;
use std::io;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

use crate::cachefs;

/// The one file of an entry directory.
pub const ENTRY: &str = "entry";
/// Leads the file; the key holds the build id, so an older binary's entries
/// are never read and this only has to catch bytes ietsp did not write.
const VERSION: u8 = 1;
/// The version byte and the number of parts.
const HEADER: usize = 2;
/// One part's place in the file: tag, offset, length.
const SLOT: usize = 17;

/// What an entry holds. Which parts exist depends on the subcommand.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Part {
    /// The observations a hit validates.
    Readset,
    /// The stdout bytes of an `eval`, or the roots of a `build`.
    Result,
    /// Every derivation a `build` evaluation produced.
    Drvs,
    /// Where each source's bytes were.
    Sources,
    /// The lint warnings the run printed; a hit prints them again.
    Warnings,
}

impl Part {
    fn tag(self) -> u8 {
        match self {
            Part::Readset => 0,
            Part::Result => 1,
            Part::Drvs => 2,
            Part::Sources => 3,
            Part::Warnings => 4,
        }
    }
}

/// One generation of one entry: the open file and where its parts are. The
/// descriptor is what makes the snapshot — a publish renames a new file over
/// the name, and this keeps reading the bytes it opened.
pub struct Entry {
    file: fs::File,
    slots: Vec<(u8, u64, u64)>,
}

impl Entry {
    /// `None` for any bytes that are not an entry this binary wrote: the
    /// caller discards it, as it discards a part that does not decode.
    fn parse(file: fs::File) -> Option<Entry> {
        let len = file.metadata().ok()?.len();
        let mut head = [0u8; HEADER];
        file.read_exact_at(&mut head, 0).ok()?;
        if head[0] != VERSION {
            return None;
        }
        let count = usize::from(head[1]);
        let payload = (HEADER + count * SLOT) as u64;
        let mut table = vec![0u8; count * SLOT];
        file.read_exact_at(&mut table, HEADER as u64).ok()?;
        let mut slots = Vec::with_capacity(count);
        for s in table.as_chunks::<SLOT>().0 {
            let at = u64::from_le_bytes(s[1..9].try_into().ok()?);
            let n = u64::from_le_bytes(s[9..17].try_into().ok()?);
            if at < payload || at.checked_add(n)? > len {
                return None;
            }
            slots.push((s[0], at, n));
        }
        Some(Entry { file, slots })
    }

    /// The bytes of one part, read from this generation of the entry.
    pub fn part(&self, part: Part) -> Option<Vec<u8>> {
        let (_, at, n) = *self.slots.iter().find(|(t, ..)| *t == part.tag())?;
        let mut bytes = vec![0u8; usize::try_from(n).ok()?];
        self.file.read_exact_at(&mut bytes, at).ok()?;
        Some(bytes)
    }
}

/// What a key names on disk.
pub enum Opened {
    /// No entry: a cold run, and nothing to report.
    None,
    /// An entry whose bytes are not one: poison and wipe.
    Bad,
    Entry(Entry),
}

pub struct Store {
    root: PathBuf,
}

impl Store {
    /// `--cache-dir DIR`, else `IETSP_CACHE_DIR`, else `$XDG_CACHE_HOME/ietsp`,
    /// else `~/.cache/ietsp`; the entries live in its `trace/` subdirectory,
    /// so the directory the user names is never a removal target. `None` when
    /// no home is known.
    pub fn locate(override_dir: Option<&str>) -> Option<Store> {
        let base = if let Some(d) = override_dir {
            PathBuf::from(d)
        } else if let Some(d) = &crate::session::Session::current().settings().cache_dir {
            d.clone()
        } else {
            let base = match std::env::var_os("XDG_CACHE_HOME") {
                Some(x) if !x.is_empty() => PathBuf::from(x),
                _ => PathBuf::from(std::env::var_os("HOME")?).join(".cache"),
            };
            base.join("ietsp")
        };
        Some(Store {
            root: base.join("trace"),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn entry_dir(&self, key: &str) -> PathBuf {
        self.root.join(key)
    }

    /// Opens the entry once. Every part a run reads comes from the returned
    /// [`Entry`], so a sibling that publishes a new generation meanwhile
    /// cannot be read half.
    pub fn open(&self, key: &str) -> Opened {
        let Ok(f) = fs::File::open(self.entry_dir(key).join(ENTRY)) else {
            return Opened::None;
        };
        match Entry::parse(f) {
            Some(e) => Opened::Entry(e),
            None => Opened::Bad,
        }
    }

    /// Publishes `parts` as the whole entry, in one rename under one hold of
    /// the store lock. `false` when a sibling writer held that lock: the
    /// entry stays the generation it was and the next run misses.
    pub fn publish(&self, key: &str, parts: &[(Part, &[u8])]) -> io::Result<bool> {
        let count = u8::try_from(parts.len()).map_err(|_| io::ErrorKind::InvalidInput)?;
        let mut head = vec![VERSION, count];
        let mut at = (HEADER + parts.len() * SLOT) as u64;
        for (p, bytes) in parts {
            head.push(p.tag());
            head.extend_from_slice(&at.to_le_bytes());
            head.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
            at += bytes.len() as u64;
        }
        let mut chunks: Vec<&[u8]> = Vec::with_capacity(parts.len() + 1);
        chunks.push(&head);
        chunks.extend(parts.iter().map(|(_, b)| *b));
        let dir = self.entry_dir(key);
        fs::create_dir_all(&dir)?;
        let Some(_lock) = cachefs::write_lock(&self.root) else {
            return Ok(false);
        };
        cachefs::write_atomic(&dir, ENTRY, cachefs::tag(), &chunks).map(|()| true)
    }

    /// A hit marks the entry as used; renaming a file into the directory
    /// already does that, so a republished entry needs no touch.
    pub fn touch(&self, key: &str) {
        cachefs::touch(&self.entry_dir(key));
    }

    /// Poison and wipe, never repair: an entry that does not decode goes.
    pub fn discard(&self, key: &str) {
        let _ = fs::remove_dir_all(self.entry_dir(key));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(name: &str) -> Store {
        let root =
            std::env::temp_dir().join(format!("ietsp-trace-store-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        assert!(cachefs::prepare(&root, cachefs::Shape::Dirs, false));
        Store { root }
    }

    const KEY: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    #[test]
    fn one_rename_publishes_every_part() {
        let s = store("parts");
        assert!(matches!(s.open(KEY), Opened::None));
        assert!(
            s.publish(
                KEY,
                &[
                    (Part::Result, b"out".as_slice()),
                    (Part::Readset, b"obs".as_slice()),
                ],
            )
            .expect("publish")
        );
        let Opened::Entry(e) = s.open(KEY) else {
            panic!("the entry is there")
        };
        assert_eq!(e.part(Part::Readset).as_deref(), Some(b"obs".as_slice()));
        assert_eq!(e.part(Part::Result).as_deref(), Some(b"out".as_slice()));
        assert_eq!(e.part(Part::Drvs), None);
        assert_eq!(fs::read_dir(s.entry_dir(KEY)).expect("dir").count(), 1);
        let _ = fs::remove_dir_all(s.root());
    }

    #[test]
    fn a_reader_of_one_generation_never_sees_the_next() {
        let s = store("generation");
        assert!(
            s.publish(
                KEY,
                &[
                    (Part::Readset, b"old obs".as_slice()),
                    (Part::Result, b"old out".as_slice()),
                ],
            )
            .expect("publish")
        );
        let Opened::Entry(e) = s.open(KEY) else {
            panic!("the entry is there")
        };
        assert_eq!(
            e.part(Part::Readset).as_deref(),
            Some(b"old obs".as_slice())
        );
        assert!(
            s.publish(
                KEY,
                &[
                    (Part::Readset, b"new observations".as_slice()),
                    (Part::Result, b"new out".as_slice()),
                    (Part::Warnings, b"w".as_slice()),
                ],
            )
            .expect("publish")
        );
        // the parts this run goes on to read are the ones it validated
        assert_eq!(e.part(Part::Result).as_deref(), Some(b"old out".as_slice()));
        assert_eq!(e.part(Part::Warnings), None);
        let Opened::Entry(next) = s.open(KEY) else {
            panic!("the entry is there")
        };
        assert_eq!(
            next.part(Part::Result).as_deref(),
            Some(b"new out".as_slice())
        );
        let _ = fs::remove_dir_all(s.root());
    }

    #[test]
    fn bytes_that_are_not_an_entry_are_bad_not_a_panic() {
        let s = store("garbage");
        let dir = s.entry_dir(KEY);
        fs::create_dir_all(&dir).expect("dir");
        for bytes in [
            b"".as_slice(),
            b"\x01".as_slice(),
            b"\x09\x01".as_slice(),
            b"\x01\x04".as_slice(),
            &[
                1, 1, 0, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
                255,
            ],
        ] {
            fs::write(dir.join(ENTRY), bytes).expect("entry");
            assert!(matches!(s.open(KEY), Opened::Bad), "{bytes:?}");
        }
        let _ = fs::remove_dir_all(s.root());
    }
}

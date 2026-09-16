//! Binary format of the cache entry files: a tag byte per record, LEB128
//! lengths, raw bytes. One format version byte leads every file.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::PathBuf;

use super::{Digest32, Filtered, Kind, Obs};
use crate::builtins::derivation::DrvInfo;
use crate::store::path::StorePath;
use crate::store::source::SourceOrigin;

const VERSION: u8 = 1;

struct Enc(Vec<u8>);

impl Enc {
    fn new() -> Enc {
        let mut e = Enc(Vec::new());
        e.u8(VERSION);
        e
    }

    fn u8(&mut self, b: u8) {
        self.0.push(b);
    }

    fn u64(&mut self, mut n: u64) {
        loop {
            let b = (n & 0x7f) as u8;
            n >>= 7;
            if n == 0 {
                self.0.push(b);
                return;
            }
            self.0.push(b | 0x80);
        }
    }

    fn i64(&mut self, n: i64) {
        self.u64(n as u64);
    }

    fn bytes(&mut self, b: &[u8]) {
        self.u64(b.len() as u64);
        self.0.extend_from_slice(b);
    }

    fn opt_bytes(&mut self, b: Option<&[u8]>) {
        match b {
            Some(b) => {
                self.u8(1);
                self.bytes(b);
            }
            None => self.u8(0),
        }
    }

    fn digest(&mut self, d: &Digest32) {
        self.0.extend_from_slice(d);
    }

    fn list(&mut self, items: &[Vec<u8>]) {
        self.u64(items.len() as u64);
        for i in items {
            self.bytes(i);
        }
    }
}

struct Dec<'a> {
    b: &'a [u8],
    i: usize,
}

type R<T> = Result<T, ()>;

impl<'a> Dec<'a> {
    fn new(b: &'a [u8]) -> R<Dec<'a>> {
        let mut d = Dec { b, i: 0 };
        if d.u8()? != VERSION {
            return Err(());
        }
        Ok(d)
    }

    fn done(&self) -> bool {
        self.i >= self.b.len()
    }

    fn u8(&mut self) -> R<u8> {
        let b = *self.b.get(self.i).ok_or(())?;
        self.i += 1;
        Ok(b)
    }

    fn u64(&mut self) -> R<u64> {
        let mut n = 0u64;
        let mut shift = 0;
        loop {
            let b = self.u8()?;
            if shift >= 64 {
                return Err(());
            }
            n |= u64::from(b & 0x7f) << shift;
            if b & 0x80 == 0 {
                return Ok(n);
            }
            shift += 7;
        }
    }

    fn i64(&mut self) -> R<i64> {
        Ok(self.u64()? as i64)
    }

    fn bytes(&mut self) -> R<&'a [u8]> {
        let n = self.u64()? as usize;
        let s = self
            .b
            .get(self.i..self.i.checked_add(n).ok_or(())?)
            .ok_or(())?;
        self.i += n;
        Ok(s)
    }

    fn vec(&mut self) -> R<Vec<u8>> {
        Ok(self.bytes()?.to_vec())
    }

    fn opt_vec(&mut self) -> R<Option<Vec<u8>>> {
        Ok(match self.u8()? {
            0 => None,
            1 => Some(self.vec()?),
            _ => return Err(()),
        })
    }

    fn digest(&mut self) -> R<Digest32> {
        let s = self.b.get(self.i..self.i + 32).ok_or(())?;
        self.i += 32;
        let mut d = [0u8; 32];
        d.copy_from_slice(s);
        Ok(d)
    }

    fn list(&mut self) -> R<Vec<Vec<u8>>> {
        let n = self.u64()? as usize;
        let mut v = Vec::with_capacity(n.min(1 << 16));
        for _ in 0..n {
            v.push(self.vec()?);
        }
        Ok(v)
    }
}

// ---- read set --------------------------------------------------------------

fn put_obs(e: &mut Enc, o: &Obs) {
    match o {
        Obs::File {
            path,
            size,
            mtime_ns,
            ino,
            hash,
        } => {
            e.u8(0);
            e.bytes(path);
            e.u64(*size);
            e.i64(*mtime_ns);
            e.u64(*ino);
            e.digest(hash);
        }
        Obs::Dir { path, hash } => {
            e.u8(1);
            e.bytes(path);
            e.digest(hash);
        }
        Obs::Stat { path, follow, kind } => {
            e.u8(2);
            e.bytes(path);
            e.u8(u8::from(*follow));
            e.u8(kind.to_u8());
        }
        Obs::Tree {
            root,
            nar_hash,
            stat_hash,
            filtered,
        } => {
            e.u8(3);
            e.bytes(root);
            e.digest(nar_hash);
            e.digest(stat_hash);
            match filtered {
                None => e.u8(0),
                Some(f) => {
                    e.u8(1);
                    e.digest(&f.seen_hash);
                    e.list(&f.accepted);
                }
            }
        }
        Obs::Canon { path, target } => {
            e.u8(4);
            e.bytes(path);
            e.opt_bytes(target.as_deref());
        }
        Obs::Env { name, value } => {
            e.u8(5);
            e.bytes(name);
            e.opt_bytes(value.as_deref());
        }
    }
}

fn get_obs(d: &mut Dec<'_>) -> R<Obs> {
    Ok(match d.u8()? {
        0 => Obs::File {
            path: d.vec()?,
            size: d.u64()?,
            mtime_ns: d.i64()?,
            ino: d.u64()?,
            hash: d.digest()?,
        },
        1 => Obs::Dir {
            path: d.vec()?,
            hash: d.digest()?,
        },
        2 => Obs::Stat {
            path: d.vec()?,
            follow: d.u8()? != 0,
            kind: Kind::from_u8(d.u8()?).ok_or(())?,
        },
        3 => {
            let root = d.vec()?;
            let nar_hash = d.digest()?;
            let stat_hash = d.digest()?;
            let filtered = match d.u8()? {
                0 => None,
                1 => Some(Filtered {
                    seen_hash: d.digest()?,
                    accepted: d.list()?,
                }),
                _ => return Err(()),
            };
            Obs::Tree {
                root,
                nar_hash,
                stat_hash,
                filtered,
            }
        }
        4 => Obs::Canon {
            path: d.vec()?,
            target: d.opt_vec()?,
        },
        5 => Obs::Env {
            name: d.vec()?,
            value: d.opt_vec()?,
        },
        _ => return Err(()),
    })
}

/// One observation alone, for exact-duplicate detection.
pub fn encode_obs(o: &Obs) -> Vec<u8> {
    let mut e = Enc(Vec::new());
    put_obs(&mut e, o);
    e.0
}

pub fn encode_readset(obs: &[Obs]) -> Vec<u8> {
    let mut e = Enc::new();
    e.u64(obs.len() as u64);
    for o in obs {
        put_obs(&mut e, o);
    }
    e.0
}

pub fn decode_readset(b: &[u8]) -> Option<Vec<Obs>> {
    let mut d = Dec::new(b).ok()?;
    let n = d.u64().ok()? as usize;
    let mut v = Vec::with_capacity(n.min(1 << 20));
    for _ in 0..n {
        v.push(get_obs(&mut d).ok()?);
    }
    d.done().then_some(v)
}

// ---- derivations -----------------------------------------------------------

/// Every drv this evaluation produced (the ones with an ATerm); drvs read from
/// `/nix/store` are re-read by the planner.
pub fn encode_drvs(drvs: &HashMap<StorePath, DrvInfo>) -> Vec<u8> {
    let mut e = Enc::new();
    let mut items: Vec<(&StorePath, &DrvInfo)> =
        drvs.iter().filter(|(_, i)| i.aterm.is_some()).collect();
    items.sort_by(|a, b| a.0.cmp(b.0));
    e.u64(items.len() as u64);
    for (p, info) in items {
        e.bytes(p.base_name().as_bytes());
        let outs: Vec<Vec<u8>> = info.outputs.iter().map(|o| o.as_bytes().to_vec()).collect();
        e.list(&outs);
        let refs: Vec<Vec<u8>> = info
            .references
            .iter()
            .map(|r| r.base_name().as_bytes().to_vec())
            .collect();
        e.list(&refs);
        e.bytes(info.aterm.as_deref().unwrap_or_default());
    }
    e.0
}

fn store_path(b: &[u8]) -> R<StorePath> {
    StorePath::from_base_name(std::str::from_utf8(b).map_err(|_| ())?).map_err(|_| ())
}

pub fn decode_drvs(b: &[u8]) -> Option<HashMap<StorePath, DrvInfo>> {
    let mut d = Dec::new(b).ok()?;
    let n = d.u64().ok()? as usize;
    let mut m = HashMap::with_capacity(n.min(1 << 20));
    for _ in 0..n {
        let p = store_path(d.bytes().ok()?).ok()?;
        let outputs: BTreeSet<String> = d
            .list()
            .ok()?
            .into_iter()
            .map(|o| String::from_utf8(o).ok())
            .collect::<Option<_>>()?;
        let references: BTreeSet<StorePath> = d
            .list()
            .ok()?
            .iter()
            .map(|r| store_path(r).ok())
            .collect::<Option<_>>()?;
        let aterm = d.vec().ok()?;
        m.insert(
            p,
            DrvInfo {
                outputs,
                references,
                aterm: Some(aterm),
            },
        );
    }
    d.done().then_some(m)
}

// ---- sources ---------------------------------------------------------------

pub fn encode_sources(sources: &HashMap<StorePath, SourceOrigin>) -> Vec<u8> {
    let mut e = Enc::new();
    let mut items: Vec<(&StorePath, &SourceOrigin)> = sources.iter().collect();
    items.sort_by(|a, b| a.0.cmp(b.0));
    e.u64(items.len() as u64);
    for (p, o) in items {
        e.bytes(p.base_name().as_bytes());
        match o {
            SourceOrigin::Tree { local, accepted } => {
                e.u8(0);
                e.bytes(local.as_os_str().as_encoded_bytes());
                match accepted {
                    None => e.u8(0),
                    Some(a) => {
                        e.u8(1);
                        let mut v: Vec<Vec<u8>> = a
                            .iter()
                            .map(|p| p.as_os_str().as_encoded_bytes().to_vec())
                            .collect();
                        v.sort();
                        e.list(&v);
                    }
                }
            }
            SourceOrigin::Text {
                contents,
                references,
            } => {
                e.u8(1);
                e.bytes(contents);
                let refs: Vec<Vec<u8>> = references
                    .iter()
                    .map(|r| r.base_name().as_bytes().to_vec())
                    .collect();
                e.list(&refs);
            }
        }
    }
    e.0
}

fn path_buf(b: &[u8]) -> PathBuf {
    PathBuf::from(<std::ffi::OsStr as std::os::unix::ffi::OsStrExt>::from_bytes(b))
}

pub fn decode_sources(b: &[u8]) -> Option<HashMap<StorePath, SourceOrigin>> {
    let mut d = Dec::new(b).ok()?;
    let n = d.u64().ok()? as usize;
    let mut m = HashMap::with_capacity(n.min(1 << 20));
    for _ in 0..n {
        let p = store_path(d.bytes().ok()?).ok()?;
        let o = match d.u8().ok()? {
            0 => {
                let local = path_buf(d.bytes().ok()?);
                let accepted = match d.u8().ok()? {
                    0 => None,
                    1 => Some(
                        d.list()
                            .ok()?
                            .iter()
                            .map(|p| path_buf(p))
                            .collect::<HashSet<PathBuf>>(),
                    ),
                    _ => return None,
                };
                SourceOrigin::Tree { local, accepted }
            }
            1 => {
                let contents = d.vec().ok()?;
                let references: BTreeSet<StorePath> = d
                    .list()
                    .ok()?
                    .iter()
                    .map(|r| store_path(r).ok())
                    .collect::<Option<_>>()?;
                SourceOrigin::Text {
                    contents,
                    references,
                }
            }
            _ => return None,
        };
        m.insert(p, o);
    }
    d.done().then_some(m)
}

// ---- build result ----------------------------------------------------------

/// The roots `ietsp build` resolved: drv path and the output named by context.
pub fn encode_roots(roots: &[(StorePath, Option<String>)]) -> Vec<u8> {
    let mut e = Enc::new();
    e.u64(roots.len() as u64);
    for (p, out) in roots {
        e.bytes(p.base_name().as_bytes());
        e.opt_bytes(out.as_deref().map(str::as_bytes));
    }
    e.0
}

pub fn decode_roots(b: &[u8]) -> Option<Vec<(StorePath, Option<String>)>> {
    let mut d = Dec::new(b).ok()?;
    let n = d.u64().ok()? as usize;
    let mut v = Vec::with_capacity(n.min(1 << 16));
    for _ in 0..n {
        let p = store_path(d.bytes().ok()?).ok()?;
        let out = match d.opt_vec().ok()? {
            Some(o) => Some(String::from_utf8(o).ok()?),
            None => None,
        };
        v.push((p, out));
    }
    d.done().then_some(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readset_roundtrip() {
        let obs = vec![
            Obs::File {
                path: b"/a".to_vec(),
                size: 3,
                mtime_ns: -5,
                ino: 1 << 40,
                hash: [7; 32],
            },
            Obs::Dir {
                path: b"/d".to_vec(),
                hash: [1; 32],
            },
            Obs::Stat {
                path: b"/s".to_vec(),
                follow: true,
                kind: Kind::Symlink,
            },
            Obs::Tree {
                root: b"/t".to_vec(),
                nar_hash: [2; 32],
                stat_hash: [3; 32],
                filtered: Some(Filtered {
                    seen_hash: [4; 32],
                    accepted: vec![b"/t/x".to_vec()],
                }),
            },
            Obs::Canon {
                path: b"/c".to_vec(),
                target: None,
            },
            Obs::Env {
                name: b"HOME".to_vec(),
                value: Some(b"/h".to_vec()),
            },
        ];
        let b = encode_readset(&obs);
        assert_eq!(decode_readset(&b).unwrap(), obs);
        assert!(decode_readset(&b[..b.len() - 1]).is_none());
    }

    #[test]
    fn drvs_sources_roots_roundtrip() {
        let p = StorePath::from_base_name("a3ck4d9zzk4l1r76rz5zv2wgycmpsy1y-x.drv").unwrap();
        let s = StorePath::from_base_name("a3ck4d9zzk4l1r76rz5zv2wgycmpsy1y-src").unwrap();
        let mut drvs = HashMap::new();
        drvs.insert(
            p.clone(),
            DrvInfo {
                outputs: ["out".to_string()].into(),
                references: [s.clone()].into(),
                aterm: Some(b"Derive(...)".to_vec()),
            },
        );
        drvs.insert(
            s.clone(),
            DrvInfo {
                outputs: BTreeSet::new(),
                references: BTreeSet::new(),
                aterm: None,
            },
        );
        let back = decode_drvs(&encode_drvs(&drvs)).unwrap();
        assert_eq!(back.len(), 1);
        assert_eq!(back[&p].aterm.as_deref(), Some(&b"Derive(...)"[..]));
        assert_eq!(back[&p].references, [s.clone()].into());

        let mut sources = HashMap::new();
        sources.insert(
            s.clone(),
            SourceOrigin::Tree {
                local: PathBuf::from("/tmp/x"),
                accepted: Some([PathBuf::from("/tmp/x/a")].into()),
            },
        );
        sources.insert(
            p.clone(),
            SourceOrigin::Text {
                contents: b"hi".to_vec(),
                references: [s.clone()].into(),
            },
        );
        let back = decode_sources(&encode_sources(&sources)).unwrap();
        match &back[&s] {
            SourceOrigin::Tree { local, accepted } => {
                assert_eq!(local, &PathBuf::from("/tmp/x"));
                assert!(
                    accepted
                        .as_ref()
                        .unwrap()
                        .contains(&PathBuf::from("/tmp/x/a"))
                );
            }
            _ => panic!(),
        }
        match &back[&p] {
            SourceOrigin::Text {
                contents,
                references,
            } => {
                assert_eq!(contents, b"hi");
                assert_eq!(references, &[s.clone()].into());
            }
            _ => panic!(),
        }

        let roots = vec![(p.clone(), Some("out".to_string())), (p, None)];
        assert_eq!(decode_roots(&encode_roots(&roots)).unwrap(), roots);
    }
}

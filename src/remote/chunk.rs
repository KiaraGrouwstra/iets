//! Content-addressed chunking of byte streams (NARs) and digest conversions.

use std::io::{self, Write};

use crate::store::hash::{Hash, HashAlgo, hash_bytes};

use super::proto::{Digest, HashAlgo as ProtoAlgo};

/// The largest chunk the protocol carries. Keeps every message small enough for HTTP/2
/// flow control to pipeline several in flight.
pub const CHUNK_MAX: usize = 4 << 20;

pub fn sha256(data: &[u8]) -> Hash {
    hash_bytes(HashAlgo::Sha256, data)
}

pub fn to_digest(h: &Hash) -> Digest {
    Digest {
        algo: ProtoAlgo::Sha256 as i32,
        bytes: h.bytes().to_vec(),
    }
}

pub fn from_digest(d: &Digest) -> Result<Hash, String> {
    if d.algo != ProtoAlgo::Sha256 as i32 {
        return Err(format!("unsupported hash algorithm {}", d.algo));
    }
    Hash::from_bytes(HashAlgo::Sha256, &d.bytes).map_err(|e| e.0)
}

/// A `Write` sink that cuts the stream into `CHUNK_MAX` pieces and hands each one to `emit`.
/// Call `finish` to flush the last partial chunk.
pub struct Chunker<F: FnMut(Vec<u8>) -> io::Result<()>> {
    buf: Vec<u8>,
    emit: F,
}

impl<F: FnMut(Vec<u8>) -> io::Result<()>> Chunker<F> {
    pub fn new(emit: F) -> Self {
        Chunker {
            buf: Vec::with_capacity(CHUNK_MAX),
            emit,
        }
    }

    pub fn finish(mut self) -> io::Result<()> {
        if !self.buf.is_empty() {
            let chunk = std::mem::take(&mut self.buf);
            (self.emit)(chunk)?;
        }
        Ok(())
    }
}

impl<F: FnMut(Vec<u8>) -> io::Result<()>> Write for Chunker<F> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        let room = CHUNK_MAX - self.buf.len();
        let n = room.min(data.len());
        self.buf.extend_from_slice(&data[..n]);
        if self.buf.len() == CHUNK_MAX {
            let chunk = std::mem::replace(&mut self.buf, Vec::with_capacity(CHUNK_MAX));
            (self.emit)(chunk)?;
        }
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Writes to both sinks; used to hash a NAR while chunking it.
pub struct Tee<'a>(pub &'a mut dyn Write, pub &'a mut dyn Write);

impl Write for Tee<'_> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.0.write_all(data)?;
        self.1.write_all(data)?;
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()?;
        self.1.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunker_cuts_at_max() {
        let mut sizes = Vec::new();
        let mut c = Chunker::new(|v| {
            sizes.push(v.len());
            Ok(())
        });
        c.write_all(&vec![1u8; CHUNK_MAX + 10]).unwrap();
        c.write_all(&vec![2u8; CHUNK_MAX - 10]).unwrap();
        c.write_all(&[3u8; 5]).unwrap();
        c.finish().unwrap();
        assert_eq!(sizes, vec![CHUNK_MAX, CHUNK_MAX, 5]);
    }

    #[test]
    fn digest_roundtrip() {
        let h = sha256(b"abc");
        assert_eq!(from_digest(&to_digest(&h)).unwrap(), h);
    }
}

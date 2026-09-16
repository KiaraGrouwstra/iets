//! Framing of the Nix worker protocol (libutil/serialise.hh): every integer is a u64 in
//! little-endian; a string is its length, its bytes, and zero padding to a multiple of 8.

use std::io::{self, Read, Write};

pub fn write_u64(w: &mut dyn Write, n: u64) -> io::Result<()> {
    w.write_all(&n.to_le_bytes())
}

pub fn write_str(w: &mut dyn Write, s: &[u8]) -> io::Result<()> {
    write_u64(w, s.len() as u64)?;
    w.write_all(s)?;
    let rem = s.len() % 8;
    if rem != 0 {
        w.write_all(&[0u8; 8][..8 - rem])?;
    }
    Ok(())
}

/// Mirrors serialise.cc:writeStrings: a count followed by each string.
pub fn write_strs<S: AsRef<[u8]>>(w: &mut dyn Write, items: &[S]) -> io::Result<()> {
    write_u64(w, items.len() as u64)?;
    for s in items {
        write_str(w, s.as_ref())?;
    }
    Ok(())
}

pub fn read_u64(r: &mut dyn Read) -> io::Result<u64> {
    let mut b = [0u8; 8];
    r.read_exact(&mut b)?;
    Ok(u64::from_le_bytes(b))
}

fn bad(why: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("worker protocol: {why}"),
    )
}

/// Mirrors serialise.cc:readString with a size cap.
pub fn read_bytes(r: &mut dyn Read, max: u64) -> io::Result<Vec<u8>> {
    let len = read_u64(r)?;
    if len > max {
        return Err(bad("string too long"));
    }
    let mut buf = vec![0u8; len as usize];
    r.read_exact(&mut buf)?;
    let rem = (len % 8) as usize;
    if rem != 0 {
        let mut pad = [0u8; 8];
        r.read_exact(&mut pad[..8 - rem])?;
        if pad.iter().any(|&b| b != 0) {
            return Err(bad("non-zero padding"));
        }
    }
    Ok(buf)
}

pub const MAX_STRING: u64 = 64 << 20;

pub fn read_str(r: &mut dyn Read) -> io::Result<String> {
    Ok(String::from_utf8_lossy(&read_bytes(r, MAX_STRING)?).into_owned())
}

pub fn read_strs(r: &mut dyn Read) -> io::Result<Vec<String>> {
    let n = read_u64(r)?;
    if n > 1 << 24 {
        return Err(bad("string list too long"));
    }
    (0..n).map(|_| read_str(r)).collect()
}

/// Mirrors serialise.hh:FramedSink: the payload of `AddMultipleToStore` goes out as
/// length-prefixed frames (no padding) closed by a zero-length frame, so the daemon can find
/// the end without parsing the content.
pub struct FramedSink<'a> {
    to: &'a mut dyn Write,
    buf: Vec<u8>,
}

impl<'a> FramedSink<'a> {
    pub const FRAME: usize = 1 << 20;

    pub fn new(to: &'a mut dyn Write) -> Self {
        FramedSink {
            to,
            buf: Vec::with_capacity(Self::FRAME),
        }
    }

    fn send(&mut self) -> io::Result<()> {
        if !self.buf.is_empty() {
            write_u64(self.to, self.buf.len() as u64)?;
            self.to.write_all(&self.buf)?;
            self.buf.clear();
        }
        Ok(())
    }

    /// Sends the last frame and the terminator.
    pub fn finish(mut self) -> io::Result<()> {
        self.send()?;
        write_u64(self.to, 0)?;
        self.to.flush()
    }
}

impl Write for FramedSink<'_> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if self.buf.len() + data.len() > Self::FRAME {
            self.send()?;
        }
        if data.len() >= Self::FRAME {
            write_u64(self.to, data.len() as u64)?;
            self.to.write_all(data)?;
        } else {
            self.buf.extend_from_slice(data);
        }
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.send()?;
        self.to.flush()
    }
}

/// Counts the bytes that pass through, for `--timings`.
pub struct Counting<W: Write> {
    pub inner: W,
    pub bytes: u64,
}

impl<W: Write> Write for Counting<W> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(data)?;
        self.bytes += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strings_are_padded() {
        let mut out = Vec::new();
        write_str(&mut out, b"hello").unwrap();
        assert_eq!(out.len(), 16);
        assert_eq!(&out[..8], &5u64.to_le_bytes());
        assert_eq!(&out[13..], &[0, 0, 0]);
        let mut r = out.as_slice();
        assert_eq!(read_str(&mut r).unwrap(), "hello");
        assert!(r.is_empty());
    }

    #[test]
    fn framed_sink_frames_and_terminates() {
        let mut out = Vec::new();
        {
            let mut s = FramedSink::new(&mut out);
            s.write_all(b"abc").unwrap();
            s.write_all(b"de").unwrap();
            s.finish().unwrap();
        }
        let mut expect = Vec::new();
        write_u64(&mut expect, 5).unwrap();
        expect.extend_from_slice(b"abcde");
        write_u64(&mut expect, 0).unwrap();
        assert_eq!(out, expect);
    }
}

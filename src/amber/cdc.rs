//! Content-defined chunking as core-rs `src/chunkers.rs`: the UltraCDC byte chunker (a port
//! of PlakarKorp/go-cdc-chunkers, ISC) at the amberfs parameters, and the item chunker that
//! cuts index and entry streams between whole items.
//!
//! Copyright (c) 2023 Gilles Chehade <gilles@poolp.org>. Permission to use, copy, modify, and
//! distribute this software for any purpose with or without fee is hereby granted, provided
//! that the above copyright notice and this permission notice appear in all copies. THE
//! SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL WARRANTIES WITH REGARD TO THIS
//! SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS.

use std::io::{self, Read};

/// UltraCDC sizes. amberfs (hairesis decision 0028) chunks every Nix store path at
/// 8 KiB / 32 KiB / 64 KiB; every key depends on these, so they never change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByteOpts {
    pub min_size: usize,
    pub normal_size: usize,
    pub max_size: usize,
}

pub const BYTE_OPTS: ByteOpts = ByteOpts {
    min_size: 8 * 1024,
    normal_size: 32 * 1024,
    max_size: 64 * 1024,
};

/// core-rs `ingest::DEFAULT_ITEM_BITS`: directory leaves and file index nodes average 2^7
/// items.
pub const ITEM_BITS: u32 = 7;

const HAMMING_TO_0XAA: [i32; 256] = {
    let mut t = [0i32; 256];
    let mut b = 0usize;
    while b < 256 {
        t[b] = ((b as u8) ^ 0xAA).count_ones() as i32;
        b += 1;
    }
    t
};

/// Upstream `UltraCDC.Algorithm`: the exclusive cut index in `data`, which holds at most
/// `max_size` bytes.
fn cutpoint(opts: &ByteOpts, data: &[u8]) -> usize {
    const MASK_S: u64 = 0x2F;
    const MASK_L: u64 = 0x2C;
    const LOW_ENTROPY_STRING_THRESHOLD: usize = 64;

    let min_size = opts.min_size;
    let max_size = opts.max_size;
    let mut normal_size = opts.normal_size;
    let mut low_entropy_count = 0usize;
    let mut mask = MASK_S;

    let mut n = data.len();
    if n <= min_size {
        return n;
    } else if n >= max_size {
        n = max_size;
    } else if n <= normal_size {
        normal_size = n;
    }
    if n < min_size + 8 {
        return n;
    }

    let mut out_buf_win: &[u8] = &data[min_size..min_size + 8];
    let mut dist: i64 = out_buf_win
        .iter()
        .map(|&v| i64::from(HAMMING_TO_0XAA[v as usize]))
        .sum();

    for i in (min_size + 8..=n - 8).step_by(8) {
        if i >= normal_size {
            mask = MASK_L;
        }
        let in_buf_win = &data[i..i + 8];
        if in_buf_win == out_buf_win {
            low_entropy_count += 1;
            if low_entropy_count >= LOW_ENTROPY_STRING_THRESHOLD {
                return i + 8;
            }
            continue;
        }
        low_entropy_count = 0;
        for j in 0..8 {
            if (dist as u64) & mask == 0 {
                return i + j;
            }
            let out_byte = data[i + j - 8];
            let in_byte = data[i + j];
            dist += i64::from(HAMMING_TO_0XAA[in_byte as usize])
                - i64::from(HAMMING_TO_0XAA[out_byte as usize]);
        }
        out_buf_win = in_buf_win;
    }
    n
}

/// Runs the byte chunker over `reader` and hands each chunk to `f` in order. An empty input
/// yields no chunks (core-rs `split_bytes`; the caller adds the empty Blob for an empty
/// file).
pub fn split_bytes<R: Read>(
    mut reader: R,
    opts: &ByteOpts,
    mut f: impl FnMut(Vec<u8>) -> io::Result<()>,
) -> io::Result<()> {
    let max_size = opts.max_size;
    let mut buf: Vec<u8> = Vec::with_capacity(max_size);
    let mut eof = false;
    loop {
        if !eof && buf.len() < max_size {
            let want = max_size - buf.len();
            let got = reader.by_ref().take(want as u64).read_to_end(&mut buf)?;
            if got < want {
                eof = true;
            }
        }
        if buf.is_empty() {
            return Ok(());
        }
        let cut = cutpoint(opts, &buf);
        f(buf[..cut].to_vec())?;
        buf.drain(..cut);
    }
}

/// Ends a run of items when the low `bits` bits of BLAKE3(item encoding), read as a
/// little-endian u64 of the first 8 digest bytes, are zero; bounded by `min_run` and
/// `max_run` (core-rs `ItemChunker`).
#[derive(Debug, Clone, Copy)]
pub struct ItemChunker {
    pub min_run: usize,
    pub max_run: usize,
    mask: u64,
}

impl ItemChunker {
    pub fn new(bits: u32) -> ItemChunker {
        let avg = 1usize << bits;
        ItemChunker {
            min_run: (avg / 4).max(2),
            max_run: avg * 4,
            mask: if bits > 0 { (1u64 << bits) - 1 } else { 0 },
        }
    }

    /// `run_len` counts the items in the run including this one.
    pub fn is_boundary(&self, enc: &[u8], run_len: usize) -> bool {
        if run_len >= self.max_run {
            return true;
        }
        if run_len < self.min_run {
            return false;
        }
        let sum = blake3::hash(enc);
        let mut first8 = [0u8; 8];
        first8.copy_from_slice(&sum.as_bytes()[..8]);
        u64::from_le_bytes(first8) & self.mask == 0
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// splitmix64 as VECTORS.md defines it.
    fn splitmix(seed: u64, n: usize) -> Vec<u8> {
        let mut state = seed;
        let mut out = Vec::with_capacity(n + 8);
        while out.len() < n {
            state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = state;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            out.extend_from_slice(&(z ^ (z >> 31)).to_le_bytes());
        }
        out.truncate(n);
        out
    }

    /// A VECTORS.md payload description.
    pub(crate) fn payload(v: &serde_json::Value) -> Vec<u8> {
        match v["kind"].as_str().unwrap() {
            "splitmix" => splitmix(
                v["seed"].as_u64().unwrap(),
                v["len"].as_u64().unwrap() as usize,
            ),
            "const" => vec![v["byte"].as_u64().unwrap() as u8; v["len"].as_u64().unwrap() as usize],
            "concat" => v["parts"]
                .as_array()
                .unwrap()
                .iter()
                .flat_map(payload)
                .collect(),
            k => panic!("unknown payload kind {k}"),
        }
    }

    fn fixture(name: &str) -> serde_json::Value {
        let text = std::fs::read_to_string(format!(
            "{}/tests/fixtures/amber/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap();
        serde_json::from_str(&text).unwrap()
    }

    fn lengths(data: &[u8], opts: &ByteOpts) -> Vec<usize> {
        let mut out = Vec::new();
        split_bytes(data, opts, |c| {
            out.push(c.len());
            Ok(())
        })
        .unwrap();
        out
    }

    /// `ultracdc.json`: zero sizes select the upstream defaults 2048/10240/65536.
    #[test]
    fn golden_ultracdc() {
        let v = fixture("ultracdc.json");
        let mut n = 0;
        for case in v["cases"].as_array().unwrap() {
            let pick = |k: &str, d: usize| match case[k].as_u64().unwrap() as usize {
                0 => d,
                x => x,
            };
            let opts = ByteOpts {
                min_size: pick("min", 2048),
                normal_size: pick("normal", 10240),
                max_size: pick("max", 65536),
            };
            let data = payload(&case["data"]);
            let want: Vec<usize> = case["chunks"]
                .as_array()
                .unwrap()
                .iter()
                .map(|x| x.as_u64().unwrap() as usize)
                .collect();
            assert_eq!(lengths(&data, &opts), want, "{}", case["name"]);
            n += 1;
        }
        assert!(n > 5);
    }

    #[test]
    fn golden_item_chunker() {
        let v = fixture("item_chunker.json");
        for case in v["cases"].as_array().unwrap() {
            let ic = ItemChunker::new(case["bits"].as_u64().unwrap() as u32);
            let mut runs = Vec::new();
            let mut run = 0usize;
            for item in case["items"].as_array().unwrap() {
                run += 1;
                if ic.is_boundary(&payload(item), run) {
                    runs.push(run);
                    run = 0;
                }
            }
            if run > 0 {
                runs.push(run);
            }
            let want: Vec<usize> = case["runs"]
                .as_array()
                .unwrap()
                .iter()
                .map(|x| x.as_u64().unwrap() as usize)
                .collect();
            assert_eq!(runs, want, "bits {}", case["bits"]);
        }
    }

    #[test]
    fn amberfs_sizes_reassemble() {
        let data = splitmix(9, 3 * 1024 * 1024);
        let mut got = Vec::new();
        let mut lens = Vec::new();
        split_bytes(&data[..], &BYTE_OPTS, |c| {
            lens.push(c.len());
            got.extend_from_slice(&c);
            Ok(())
        })
        .unwrap();
        assert_eq!(got, data);
        assert!(lens.len() > 40);
        for &l in &lens[..lens.len() - 1] {
            assert!((BYTE_OPTS.min_size..=BYTE_OPTS.max_size).contains(&l));
        }
    }

    #[test]
    fn item_chunker_bounds() {
        let ic = ItemChunker::new(ITEM_BITS);
        assert_eq!((ic.min_run, ic.max_run), (32, 512));
        assert!(!ic.is_boundary(b"x", 31));
        assert!(ic.is_boundary(b"x", 512));
    }
}

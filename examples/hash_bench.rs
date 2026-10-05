//! Timing harness for the hash code (src/store/hash.rs).
//!
//! `cargo run --release --example hash_bench -- <op> [runs] [arg]`
//!
//! Ops: `algos` (every algorithm over a size ladder), `encode` (the digest
//! encoders and parsers), `path` (what a store path costs around its digest),
//! `file PATH` (one file through the read loop and through a mapping),
//! `mix [bytes]` (one sha256 and one blake3 over the same bytes, one pass
//! against two), `pair` (two independent sha256 streams). Prints one
//! `name bytes runs best_ns mean_ns MB/s` line per measurement, so
//! bench/hash.nu can parse it.

use std::hint::black_box;

use iets::store::hash::{Hash, HashAlgo, Hasher, base64_encode, hash_bytes};
use std::time::Instant;

// b92c0d84 made the whole-string encoders test-only, because the library
// itself only ever appends into a buffer it already has. This harness
// measures them side by side, so it brings its own.
fn base16_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    iets::store::hash::base16_encode_into(&mut s, bytes);
    s
}

fn nix32_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(iets::store::hash::nix32_encoded_len(bytes.len()));
    iets::store::hash::nix32_encode_into(&mut s, bytes);
    s
}

const LADDER: [usize; 9] = [32, 64, 128, 1024, 4096, 1 << 16, 1 << 20, 1 << 24, 1 << 26];

fn report(name: &str, bytes: usize, runs: u32, times: &[f64]) {
    let best = times.iter().copied().fold(f64::INFINITY, f64::min);
    let mean = times.iter().sum::<f64>() / times.len() as f64;
    let mbs = if best > 0.0 {
        bytes as f64 / best / 1e6
    } else {
        0.0
    };
    println!(
        "{name} {bytes} {runs} {:.0} {:.0} {mbs:.0}",
        best * 1e9,
        mean * 1e9
    );
}

/// `iters` passes over `data`, repeated `runs` times; reports per-pass cost.
fn time<T>(name: &str, bytes: usize, runs: u32, iters: u32, mut f: impl FnMut() -> T) {
    for _ in 0..iters.min(16) {
        black_box(f());
    }
    let mut times = Vec::new();
    for _ in 0..runs {
        let t = Instant::now();
        for _ in 0..iters {
            black_box(f());
        }
        times.push(t.elapsed().as_secs_f64() / iters as f64);
    }
    report(name, bytes, runs, &times);
}

fn buf(n: usize) -> Vec<u8> {
    (0..n)
        .map(|i| (i as u8).wrapping_mul(37).wrapping_add(11))
        .collect()
}

/// Enough passes that one measurement is ~50 ms, so the clock is not the noise.
fn iters_for(n: usize) -> u32 {
    (((1 << 26) / n.max(1)) as u32).clamp(4, 200_000)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let op = args.first().map(|s| s.as_str()).unwrap_or("algos");
    let runs: u32 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(5);
    match op {
        "algos" => {
            for n in LADDER {
                let data = buf(n);
                let it = iters_for(n);
                for algo in [
                    HashAlgo::Sha256,
                    HashAlgo::Sha512,
                    HashAlgo::Blake3,
                    HashAlgo::Sha1,
                    HashAlgo::Md5,
                ] {
                    time(algo.name(), n, runs, it, || {
                        hash_bytes(algo, black_box(&data))
                    });
                }
            }
        }
        "encode" => {
            let h = hash_bytes(HashAlgo::Sha256, b"abc");
            let b16 = h.to_base16();
            let n32 = h.to_nix32();
            let b64 = h.to_base64();
            let sri = h.to_sri();
            time("base16_encode", 32, runs, 100_000, || {
                base16_encode(black_box(h.bytes()))
            });
            time("nix32_encode", 32, runs, 100_000, || {
                nix32_encode(black_box(h.bytes()))
            });
            time("base64_encode", 32, runs, 100_000, || {
                base64_encode(black_box(h.bytes()))
            });
            time("base16_parse", 32, runs, 100_000, || {
                Hash::from_base16(HashAlgo::Sha256, black_box(&b16))
            });
            time("nix32_parse", 32, runs, 100_000, || {
                Hash::from_nix32(HashAlgo::Sha256, black_box(&n32))
            });
            time("base64_parse", 32, runs, 100_000, || {
                Hash::from_base64(HashAlgo::Sha256, black_box(&b64))
            });
            time("sri_parse", 32, runs, 100_000, || {
                Hash::parse_sri(black_box(&sri))
            });
        }
        "file" => {
            let path = args.get(2).map(|s| s.as_str()).unwrap_or_else(|| {
                eprintln!("hash_bench: file <runs> <path>");
                std::process::exit(2)
            });
            let len = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0) as usize;
            for algo in [HashAlgo::Sha256, HashAlgo::Blake3] {
                time(&format!("read_{}", algo.name()), len, runs, 3, || {
                    iets::store::nar::flat_hash(std::path::Path::new(path), algo).unwrap()
                });
            }
            let f = std::fs::File::open(path).unwrap();
            let map = unsafe {
                let p = libc::mmap(
                    std::ptr::null_mut(),
                    len,
                    libc::PROT_READ,
                    libc::MAP_PRIVATE | libc::MAP_POPULATE,
                    std::os::fd::AsRawFd::as_raw_fd(&f),
                    0,
                );
                assert_ne!(p, libc::MAP_FAILED, "mmap");
                std::slice::from_raw_parts(p as *const u8, len)
            };
            for algo in [HashAlgo::Sha256, HashAlgo::Blake3] {
                time(&format!("mmap_{}", algo.name()), len, runs, 3, || {
                    hash_bytes(algo, black_box(map))
                });
            }
        }
        "mix" => {
            let n = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(1 << 20);
            let data = buf(n);
            let it = iters_for(n);
            time("mix_serial", n, runs, it, || {
                let x = hash_bytes(HashAlgo::Sha256, black_box(&data));
                let y = hash_bytes(HashAlgo::Blake3, black_box(&data));
                (x, y)
            });
            for g in [4096usize, 1 << 16, 1 << 20] {
                time(&format!("mix_interleaved_{g}"), n, runs, it, || {
                    let mut hs = Hasher::new(HashAlgo::Sha256);
                    let mut hb = Hasher::new(HashAlgo::Blake3);
                    for c in data.chunks(g) {
                        hs.update(black_box(c));
                        hb.update(black_box(c));
                    }
                    (hs.finish(), hb.finish())
                });
            }
            time("sha256_alone", n, runs, it, || {
                hash_bytes(HashAlgo::Sha256, black_box(&data))
            });
            time("blake3_alone", n, runs, it, || {
                hash_bytes(HashAlgo::Blake3, black_box(&data))
            });
        }
        "path" => {
            use std::collections::BTreeSet;
            let h = hash_bytes(HashAlgo::Sha256, b"abc");
            let h20 = iets::store::hash::compress_hash(&h, 20);
            let refs = BTreeSet::new();
            time("base16_32", 32, runs, 200_000, || {
                base16_encode(black_box(h.bytes()))
            });
            time("nix32_20", 20, runs, 200_000, || {
                nix32_encode(black_box(h20.bytes()))
            });
            time("nix32_32", 32, runs, 200_000, || {
                nix32_encode(black_box(h.bytes()))
            });
            time("base64_32", 32, runs, 200_000, || {
                base64_encode(black_box(h.bytes()))
            });
            time("compress_hash", 32, runs, 200_000, || {
                iets::store::hash::compress_hash(black_box(&h), 20)
            });
            time("make_store_path", 0, runs, 100_000, || {
                iets::store::path::make_store_path("source", black_box(&h), "hello-2.12.1").unwrap()
            });
            time("make_text_path", 0, runs, 100_000, || {
                iets::store::path::make_text_path("builder.sh", black_box(&h), &refs).unwrap()
            });
        }
        "pair" => {
            for n in [1024, 1 << 16, 1 << 20, 1 << 24] {
                let a = buf(n);
                let b = buf(n + 1);
                let it = iters_for(n);
                time("pair_serial", 2 * n, runs, it, || {
                    let x = hash_bytes(HashAlgo::Sha256, black_box(&a));
                    let y = hash_bytes(HashAlgo::Sha256, black_box(&b));
                    (x, y)
                });
                time("pair_chunked", 2 * n, runs, it, || {
                    let mut ha = Hasher::new(HashAlgo::Sha256);
                    let mut hb = Hasher::new(HashAlgo::Sha256);
                    for (ca, cb) in a.chunks(4096).zip(b.chunks(4096)) {
                        ha.update(black_box(ca));
                        hb.update(black_box(cb));
                    }
                    (ha.finish(), hb.finish())
                });
                time("pair_sha256_blake3", 2 * n, runs, it, || {
                    let x = hash_bytes(HashAlgo::Sha256, black_box(&a));
                    let y = hash_bytes(HashAlgo::Blake3, black_box(&a));
                    (x, y)
                });
            }
        }
        _ => {
            eprintln!("hash_bench: unknown op '{op}'");
            std::process::exit(2);
        }
    }
}

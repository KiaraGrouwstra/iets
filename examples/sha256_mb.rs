//! Prototype: multi-buffer SHA-256 on the SHA-NI unit.
//!
//! A single SHA-256 stream is latency-bound: 32 `sha256rnds2` per block, each
//! 4 cycles on Zen 4, so ~128 cycles per 64-byte block however fast the unit
//! retires. Independent streams interleaved in one thread hide that latency.
//! This measures the real gain, and checks the digests against src/store/hash.rs.
//!
//! `cargo run --release --example sha256_mb -- [runs] [size]`

use std::hint::black_box;
use std::time::Instant;

use iets::store::hash::{HashAlgo, hash_bytes};

// a vector kernel reads better with indexed loops over its lane and word
// arrays than with iterators, and every block here is a fixed 64 bytes
#[cfg(target_arch = "x86_64")]
#[allow(
    unsafe_op_in_unsafe_fn,
    clippy::needless_range_loop,
    clippy::chunks_exact_to_as_chunks
)]
mod kernel {
    use core::arch::x86_64::*;

    pub const H0: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];

    #[rustfmt::skip]
    pub const K32: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
        0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
        0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
        0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
        0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
        0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
        0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
    ];

    macro_rules! load_state {
        ($s:expr, $abef:ident, $cdgh:ident) => {{
            let p = $s.as_ptr() as *const __m128i;
            let dcba = _mm_loadu_si128(p.add(0));
            let efgh = _mm_shuffle_epi32(_mm_loadu_si128(p.add(1)), 0x1b);
            let cdab = _mm_shuffle_epi32(dcba, 0xb1);
            $abef = _mm_alignr_epi8(cdab, efgh, 8);
            $cdgh = _mm_blend_epi16(efgh, cdab, 0xf0);
        }};
    }

    macro_rules! store_state {
        ($s:expr, $abef:ident, $cdgh:ident) => {{
            let feba = _mm_shuffle_epi32($abef, 0x1b);
            let dchg = _mm_shuffle_epi32($cdgh, 0xb1);
            let dcba = _mm_blend_epi16(feba, dchg, 0xf0);
            let hgef = _mm_alignr_epi8(dchg, feba, 8);
            let p = $s.as_mut_ptr() as *mut __m128i;
            _mm_storeu_si128(p.add(0), dcba);
            _mm_storeu_si128(p.add(1), hgef);
        }};
    }

    macro_rules! kv {
        ($i:expr) => {
            _mm_loadu_si128(K32.as_ptr().add(4 * $i) as *const __m128i)
        };
    }

    macro_rules! rnds {
        ($abef:ident, $cdgh:ident, $rest:expr, $i:expr) => {{
            let t1 = _mm_add_epi32($rest, kv!($i));
            $cdgh = _mm_sha256rnds2_epu32($cdgh, $abef, t1);
            let t2 = _mm_shuffle_epi32(t1, 0x0e);
            $abef = _mm_sha256rnds2_epu32($abef, $cdgh, t2);
        }};
    }

    macro_rules! sched {
        ($w0:expr, $w1:expr, $w2:expr, $w3:expr) => {{
            let t1 = _mm_sha256msg1_epu32($w0, $w1);
            let t2 = _mm_alignr_epi8($w3, $w2, 4);
            _mm_sha256msg2_epu32(_mm_add_epi32(t1, t2), $w3)
        }};
    }

    /// One lane's 64 rounds, written out so the caller can interleave lanes.
    macro_rules! lane_block {
        ($blk:expr, $mask:ident, $abef:ident, $cdgh:ident, $save:block) => {{
            let p = $blk.as_ptr() as *const __m128i;
            let mut w0 = _mm_shuffle_epi8(_mm_loadu_si128(p.add(0)), $mask);
            let mut w1 = _mm_shuffle_epi8(_mm_loadu_si128(p.add(1)), $mask);
            let mut w2 = _mm_shuffle_epi8(_mm_loadu_si128(p.add(2)), $mask);
            let mut w3 = _mm_shuffle_epi8(_mm_loadu_si128(p.add(3)), $mask);
            let mut w4;
            rnds!($abef, $cdgh, w0, 0);
            rnds!($abef, $cdgh, w1, 1);
            rnds!($abef, $cdgh, w2, 2);
            rnds!($abef, $cdgh, w3, 3);
            w4 = sched!(w0, w1, w2, w3);
            rnds!($abef, $cdgh, w4, 4);
            w0 = sched!(w1, w2, w3, w4);
            rnds!($abef, $cdgh, w0, 5);
            w1 = sched!(w2, w3, w4, w0);
            rnds!($abef, $cdgh, w1, 6);
            w2 = sched!(w3, w4, w0, w1);
            rnds!($abef, $cdgh, w2, 7);
            w3 = sched!(w4, w0, w1, w2);
            rnds!($abef, $cdgh, w3, 8);
            w4 = sched!(w0, w1, w2, w3);
            rnds!($abef, $cdgh, w4, 9);
            w0 = sched!(w1, w2, w3, w4);
            rnds!($abef, $cdgh, w0, 10);
            w1 = sched!(w2, w3, w4, w0);
            rnds!($abef, $cdgh, w1, 11);
            w2 = sched!(w3, w4, w0, w1);
            rnds!($abef, $cdgh, w2, 12);
            w3 = sched!(w4, w0, w1, w2);
            rnds!($abef, $cdgh, w3, 13);
            w4 = sched!(w0, w1, w2, w3);
            rnds!($abef, $cdgh, w4, 14);
            w0 = sched!(w1, w2, w3, w4);
            rnds!($abef, $cdgh, w0, 15);
            let _ = (w0, w1, w2, w3, w4);
            $save
        }};
    }

    const fn mask() -> [u8; 16] {
        [3, 2, 1, 0, 7, 6, 5, 4, 11, 10, 9, 8, 15, 14, 13, 12]
    }

    #[target_feature(enable = "sha,sse2,ssse3,sse4.1")]
    pub unsafe fn compress1(state: &mut [u32; 8], blocks: &[u8]) {
        let m = _mm_loadu_si128(mask().as_ptr() as *const __m128i);
        let mut a0;
        let mut c0;
        load_state!(state, a0, c0);
        for blk in blocks.chunks_exact(64) {
            let (sa, sc) = (a0, c0);
            lane_block!(blk, m, a0, c0, {
                a0 = _mm_add_epi32(a0, sa);
                c0 = _mm_add_epi32(c0, sc);
            });
        }
        store_state!(state, a0, c0);
    }

    /// Two independent streams, their rounds interleaved block by block.
    #[target_feature(enable = "sha,sse2,ssse3,sse4.1")]
    pub unsafe fn compress2(s: &mut [[u32; 8]; 2], b0: &[u8], b1: &[u8]) {
        let m = _mm_loadu_si128(mask().as_ptr() as *const __m128i);
        let mut a0;
        let mut c0;
        let mut a1;
        let mut c1;
        load_state!(s[0], a0, c0);
        load_state!(s[1], a1, c1);
        for (x, y) in b0.chunks_exact(64).zip(b1.chunks_exact(64)) {
            let (sa0, sc0, sa1, sc1) = (a0, c0, a1, c1);
            let p0 = x.as_ptr() as *const __m128i;
            let p1 = y.as_ptr() as *const __m128i;
            let mut u0 = _mm_shuffle_epi8(_mm_loadu_si128(p0.add(0)), m);
            let mut v0 = _mm_shuffle_epi8(_mm_loadu_si128(p1.add(0)), m);
            let mut u1 = _mm_shuffle_epi8(_mm_loadu_si128(p0.add(1)), m);
            let mut v1 = _mm_shuffle_epi8(_mm_loadu_si128(p1.add(1)), m);
            let mut u2 = _mm_shuffle_epi8(_mm_loadu_si128(p0.add(2)), m);
            let mut v2 = _mm_shuffle_epi8(_mm_loadu_si128(p1.add(2)), m);
            let mut u3 = _mm_shuffle_epi8(_mm_loadu_si128(p0.add(3)), m);
            let mut v3 = _mm_shuffle_epi8(_mm_loadu_si128(p1.add(3)), m);
            let mut u4;
            let mut v4;
            macro_rules! step {
                ($i:expr, $x:expr, $y:expr) => {{
                    rnds!(a0, c0, $x, $i);
                    rnds!(a1, c1, $y, $i);
                }};
            }
            step!(0, u0, v0);
            step!(1, u1, v1);
            step!(2, u2, v2);
            step!(3, u3, v3);
            u4 = sched!(u0, u1, u2, u3);
            v4 = sched!(v0, v1, v2, v3);
            step!(4, u4, v4);
            u0 = sched!(u1, u2, u3, u4);
            v0 = sched!(v1, v2, v3, v4);
            step!(5, u0, v0);
            u1 = sched!(u2, u3, u4, u0);
            v1 = sched!(v2, v3, v4, v0);
            step!(6, u1, v1);
            u2 = sched!(u3, u4, u0, u1);
            v2 = sched!(v3, v4, v0, v1);
            step!(7, u2, v2);
            u3 = sched!(u4, u0, u1, u2);
            v3 = sched!(v4, v0, v1, v2);
            step!(8, u3, v3);
            u4 = sched!(u0, u1, u2, u3);
            v4 = sched!(v0, v1, v2, v3);
            step!(9, u4, v4);
            u0 = sched!(u1, u2, u3, u4);
            v0 = sched!(v1, v2, v3, v4);
            step!(10, u0, v0);
            u1 = sched!(u2, u3, u4, u0);
            v1 = sched!(v2, v3, v4, v0);
            step!(11, u1, v1);
            u2 = sched!(u3, u4, u0, u1);
            v2 = sched!(v3, v4, v0, v1);
            step!(12, u2, v2);
            u3 = sched!(u4, u0, u1, u2);
            v3 = sched!(v4, v0, v1, v2);
            step!(13, u3, v3);
            u4 = sched!(u0, u1, u2, u3);
            v4 = sched!(v0, v1, v2, v3);
            step!(14, u4, v4);
            u0 = sched!(u1, u2, u3, u4);
            v0 = sched!(v1, v2, v3, v4);
            step!(15, u0, v0);
            a0 = _mm_add_epi32(a0, sa0);
            c0 = _mm_add_epi32(c0, sc0);
            a1 = _mm_add_epi32(a1, sa1);
            c1 = _mm_add_epi32(c1, sc1);
        }
        store_state!(s[0], a0, c0);
        store_state!(s[1], a1, c1);
    }

    /// Two streams, one after the other in the same call: what the out-of-order
    /// window gives for free, without hand interleaving.
    #[target_feature(enable = "sha,sse2,ssse3,sse4.1")]
    pub unsafe fn compress2_seq(s: &mut [[u32; 8]; 2], b0: &[u8], b1: &[u8]) {
        compress1(&mut s[0], b0);
        compress1(&mut s[1], b1);
    }

    /// `N` independent streams, interleaved round group by round group.
    /// The lane arrays stay in registers while `N` is small; past that the
    /// spills are the measurement.
    #[target_feature(enable = "sha,sse2,ssse3,sse4.1")]
    pub unsafe fn compress_n<const N: usize>(s: &mut [[u32; 8]; N], blocks: &[&[u8]]) {
        let m = _mm_loadu_si128(mask().as_ptr() as *const __m128i);
        let mut abef = [_mm_setzero_si128(); N];
        let mut cdgh = [_mm_setzero_si128(); N];
        for l in 0..N {
            let p = s[l].as_ptr() as *const __m128i;
            let dcba = _mm_loadu_si128(p.add(0));
            let efgh = _mm_shuffle_epi32(_mm_loadu_si128(p.add(1)), 0x1b);
            let cdab = _mm_shuffle_epi32(dcba, 0xb1);
            abef[l] = _mm_alignr_epi8(cdab, efgh, 8);
            cdgh[l] = _mm_blend_epi16(efgh, cdab, 0xf0);
        }
        let nb = blocks[0].len() / 64;
        let mut w = [[_mm_setzero_si128(); 5]; N];
        for bi in 0..nb {
            let sa = abef;
            let sc = cdgh;
            for l in 0..N {
                let p = blocks[l].as_ptr().add(bi * 64) as *const __m128i;
                for k in 0..4 {
                    w[l][k] = _mm_shuffle_epi8(_mm_loadu_si128(p.add(k)), m);
                }
            }
            for g in 0..16 {
                if g >= 4 {
                    for l in 0..N {
                        let (i0, i1, i2, i3) = ((g + 1) % 5, (g + 2) % 5, (g + 3) % 5, (g + 4) % 5);
                        let t1 = _mm_sha256msg1_epu32(w[l][i0], w[l][i1]);
                        let t2 = _mm_alignr_epi8(w[l][i3], w[l][i2], 4);
                        w[l][g % 5] = _mm_sha256msg2_epu32(_mm_add_epi32(t1, t2), w[l][i3]);
                    }
                }
                let kvg = _mm_loadu_si128(K32.as_ptr().add(4 * g) as *const __m128i);
                for l in 0..N {
                    let t1 = _mm_add_epi32(w[l][g % 5], kvg);
                    cdgh[l] = _mm_sha256rnds2_epu32(cdgh[l], abef[l], t1);
                    let t2 = _mm_shuffle_epi32(t1, 0x0e);
                    abef[l] = _mm_sha256rnds2_epu32(abef[l], cdgh[l], t2);
                }
            }
            for l in 0..N {
                abef[l] = _mm_add_epi32(abef[l], sa[l]);
                cdgh[l] = _mm_add_epi32(cdgh[l], sc[l]);
            }
        }
        for l in 0..N {
            let feba = _mm_shuffle_epi32(abef[l], 0x1b);
            let dchg = _mm_shuffle_epi32(cdgh[l], 0xb1);
            let dcba = _mm_blend_epi16(feba, dchg, 0xf0);
            let hgef = _mm_alignr_epi8(dchg, feba, 8);
            let p = s[l].as_mut_ptr() as *mut __m128i;
            _mm_storeu_si128(p.add(0), dcba);
            _mm_storeu_si128(p.add(1), hgef);
        }
    }
    /// `len` bytes of message already compressed; pad and finish.
    pub fn tail(len: u64) -> ([u8; 136], usize) {
        let rem = (len % 64) as usize;
        let total = if rem + 9 <= 64 { 64 } else { 128 };
        let pad = total - rem;
        let mut t = [0u8; 136];
        t[0] = 0x80;
        t[pad - 8..pad].copy_from_slice(&(len * 8).to_be_bytes());
        (t, pad)
    }

    pub fn digest(state: &[u32; 8]) -> [u8; 32] {
        let mut out = [0u8; 32];
        for (i, w) in state.iter().enumerate() {
            out[4 * i..4 * i + 4].copy_from_slice(&w.to_be_bytes());
        }
        out
    }
}

#[cfg(target_arch = "x86_64")]
fn main() {
    use kernel::*;
    let args: Vec<String> = std::env::args().skip(1).collect();
    let runs: u32 = args.first().and_then(|s| s.parse().ok()).unwrap_or(5);
    let sizes: Vec<usize> = if args.len() > 1 {
        args[1..].iter().filter_map(|s| s.parse().ok()).collect()
    } else {
        vec![1024, 4096, 1 << 16, 1 << 20]
    };

    let hash1 = |data: &[u8]| -> [u8; 32] {
        let mut st = H0;
        let whole = data.len() / 64 * 64;
        unsafe { compress1(&mut st, &data[..whole]) };
        let (t, n) = tail(data.len() as u64);
        let mut last = Vec::with_capacity(n);
        last.extend_from_slice(&data[whole..]);
        last.extend_from_slice(&t[..n]);
        unsafe { compress1(&mut st, &last) };
        digest(&st)
    };
    for n in [0usize, 1, 55, 56, 64, 119, 120, 1000, 4096] {
        let d: Vec<u8> = (0..n).map(|i| (i as u8).wrapping_mul(31)).collect();
        let want = hash_bytes(HashAlgo::Sha256, &d);
        assert_eq!(hash1(&d), want.bytes(), "size {n}");
    }
    let a: Vec<u8> = (0..4096).map(|i| (i as u8).wrapping_mul(7)).collect();
    let b: Vec<u8> = (0..4096).map(|i| (i as u8).wrapping_mul(11)).collect();
    let mut st = [H0; 2];
    unsafe { compress2(&mut st, &a, &b) };
    let mut one = [H0; 2];
    unsafe {
        compress1(&mut one[0], &a);
        compress1(&mut one[1], &b);
    }
    assert_eq!(digest(&st[0]), digest(&one[0]), "lane 0");
    assert_eq!(digest(&st[1]), digest(&one[1]), "lane 1");
    println!("# correctness ok");

    for n in sizes {
        let whole = n / 64 * 64;
        let a: Vec<u8> = (0..whole).map(|i| (i as u8).wrapping_mul(7)).collect();
        let b: Vec<u8> = (0..whole).map(|i| (i as u8).wrapping_mul(11)).collect();
        let iters: u32 = (((1 << 24) / whole.max(1)) as u32).clamp(8, 100_000);
        let bench = |name: &str, nbytes: usize, mut f: Box<dyn FnMut()>| {
            for _ in 0..4 {
                f();
            }
            let mut best = f64::INFINITY;
            for _ in 0..runs {
                let t = Instant::now();
                for _ in 0..iters {
                    f();
                }
                let s = t.elapsed().as_secs_f64() / iters as f64;
                best = best.min(s);
            }
            println!(
                "{name} {} {runs} {:.0} {:.2}",
                nbytes,
                best * 1e9,
                nbytes as f64 / best / 1e9
            );
        };
        {
            let (a, b) = (a.clone(), b.clone());
            bench(
                "stock_sha2_x2",
                2 * whole,
                Box::new(move || {
                    black_box(hash_bytes(HashAlgo::Sha256, black_box(&a)));
                    black_box(hash_bytes(HashAlgo::Sha256, black_box(&b)));
                }),
            );
        }
        {
            let (a, b) = (a.clone(), b.clone());
            bench(
                "kernel1_x2",
                2 * whole,
                Box::new(move || {
                    let mut s0 = H0;
                    let mut s1 = H0;
                    unsafe {
                        compress1(&mut s0, black_box(&a));
                        compress1(&mut s1, black_box(&b));
                    }
                    black_box((s0, s1));
                }),
            );
        }
        {
            let (a, b) = (a.clone(), b.clone());
            bench(
                "kernel2_seq",
                2 * whole,
                Box::new(move || {
                    let mut s = [H0; 2];
                    unsafe { compress2_seq(&mut s, black_box(&a), black_box(&b)) };
                    black_box(s);
                }),
            );
        }
        {
            let (a, b) = (a.clone(), b.clone());
            bench(
                "kernel2_interleaved",
                2 * whole,
                Box::new(move || {
                    let mut s = [H0; 2];
                    unsafe { compress2(&mut s, black_box(&a), black_box(&b)) };
                    black_box(s);
                }),
            );
        }
        {
            let bufs: Vec<Vec<u8>> = (0..8u8)
                .map(|j| {
                    (0..whole)
                        .map(|i| (i as u8).wrapping_mul(7).wrapping_add(j))
                        .collect()
                })
                .collect();
            macro_rules! lanes {
                ($n:expr) => {{
                    let refs: Vec<&[u8]> = bufs[..$n].iter().map(|v| v.as_slice()).collect();
                    let mut st = [H0; $n];
                    unsafe { compress_n::<$n>(&mut st, &refs) };
                    let mut one = H0;
                    unsafe { compress1(&mut one, &bufs[0]) };
                    assert_eq!(digest(&st[0]), digest(&one), "lanes {}", $n);
                    let r2 = refs.clone();
                    bench(
                        concat!("lanes", stringify!($n)),
                        $n * whole,
                        Box::new(move || {
                            let mut st = [H0; $n];
                            unsafe { compress_n::<$n>(&mut st, black_box(&r2)) };
                            black_box(st);
                        }),
                    );
                }};
            }
            lanes!(1);
            lanes!(2);
            lanes!(3);
            lanes!(4);
            lanes!(6);
            lanes!(8);
        }
    }
}

#[cfg(not(target_arch = "x86_64"))]
fn main() {
    eprintln!("sha256_mb: x86_64 only");
}

//! Prototype: multi-buffer SHA-256 with plain vector arithmetic, no SHA-NI.
//!
//! One message per 32-bit element: 16 lanes in a zmm register, 8 lanes in a
//! ymm register. The rounds are ordinary add/rotate/xor/ternlog on vectors, so
//! the wide vector units do the work instead of the single serial SHA pipe.
//! Compare with `examples/sha256_mb.rs` (SHA-NI kernels) on the same box.
//!
//! `cargo run --release --example sha256_avx512 -- [runs] [size...]`

// a vector kernel reads better with indexed loops over its lane and word
// arrays than with iterators
#![allow(
    unsafe_op_in_unsafe_fn,
    unused_unsafe,
    clippy::needless_range_loop,
    clippy::type_complexity
)]

use std::hint::black_box;
use std::time::Instant;

use iets::store::hash::{HashAlgo, hash_bytes};

#[cfg(target_arch = "x86_64")]
mod kernel {
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

    pub const BSWAP: [u8; 64] = {
        let mut m = [0u8; 64];
        let mut i = 0;
        while i < 64 {
            m[i] = (i as u8 & 0xfc) | (3 - (i as u8 & 3));
            i += 1;
        }
        m
    };

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

    /// One round. `$w` is the (already scheduled) message word vector.
    macro_rules! rnd {
        ($a:ident,$b:ident,$c:ident,$d:ident,$e:ident,$f:ident,$g:ident,$h:ident, $w:ident, $t:expr) => {{
            let t: usize = $t;
            if t >= 16 {
                $w[t % 16] = add(
                    add($w[t % 16], ssig0($w[(t + 1) % 16])),
                    add($w[(t + 9) % 16], ssig1($w[(t + 14) % 16])),
                );
            }
            // h + (w + k) is off the critical path; e feeds only the right side.
            let t1 = add(
                add($h, add($w[t % 16], set1(K32[t]))),
                add(bsig1($e), ch($e, $f, $g)),
            );
            let t2 = add(bsig0($a), maj($a, $b, $c));
            $d = add($d, t1);
            $h = add(t1, t2);
        }};
    }

    macro_rules! r8 {
        ($w:ident, $t:expr, $a:ident,$b:ident,$c:ident,$d:ident,$e:ident,$f:ident,$g:ident,$h:ident) => {{
            rnd!($a, $b, $c, $d, $e, $f, $g, $h, $w, $t);
            rnd!($h, $a, $b, $c, $d, $e, $f, $g, $w, $t + 1);
            rnd!($g, $h, $a, $b, $c, $d, $e, $f, $w, $t + 2);
            rnd!($f, $g, $h, $a, $b, $c, $d, $e, $w, $t + 3);
            rnd!($e, $f, $g, $h, $a, $b, $c, $d, $w, $t + 4);
            rnd!($d, $e, $f, $g, $h, $a, $b, $c, $w, $t + 5);
            rnd!($c, $d, $e, $f, $g, $h, $a, $b, $w, $t + 6);
            rnd!($b, $c, $d, $e, $f, $g, $h, $a, $w, $t + 7);
        }};
    }

    /// Generates `compress` for one vector width. The module must define
    /// `V`, `L`, `add`, `set1`, `bsig0/1`, `ssig0/1`, `ch`, `maj`,
    /// `load_state`, `store_state`, and the ingest function named here.
    macro_rules! impl_compress {
        ($fname:ident, $feat:literal, $ingest:ident) => {
            #[target_feature(enable = $feat)]
            pub unsafe fn $fname(state: &mut [[u32; 8]; L], lanes: &[&[u8]; L]) {
                let mut st = [[0u32; L]; 8];
                for l in 0..L {
                    for i in 0..8 {
                        st[i][l] = state[l][i];
                    }
                }
                let mut a = load_state(&st[0]);
                let mut b = load_state(&st[1]);
                let mut c = load_state(&st[2]);
                let mut d = load_state(&st[3]);
                let mut e = load_state(&st[4]);
                let mut f = load_state(&st[5]);
                let mut g = load_state(&st[6]);
                let mut h = load_state(&st[7]);
                let nb = lanes[0].len() / 64;
                let ctx = $ingest::prepare(lanes);
                let mut w: [V; 16] = [set1(0); 16];
                for bi in 0..nb {
                    let save: [V; 8] = [a, b, c, d, e, f, g, h];
                    $ingest::load(&ctx, lanes, bi * 64, &mut w);
                    r8!(w, 0, a, b, c, d, e, f, g, h);
                    r8!(w, 8, a, b, c, d, e, f, g, h);
                    r8!(w, 16, a, b, c, d, e, f, g, h);
                    r8!(w, 24, a, b, c, d, e, f, g, h);
                    r8!(w, 32, a, b, c, d, e, f, g, h);
                    r8!(w, 40, a, b, c, d, e, f, g, h);
                    r8!(w, 48, a, b, c, d, e, f, g, h);
                    r8!(w, 56, a, b, c, d, e, f, g, h);
                    a = add(a, save[0]);
                    b = add(b, save[1]);
                    c = add(c, save[2]);
                    d = add(d, save[3]);
                    e = add(e, save[4]);
                    f = add(f, save[5]);
                    g = add(g, save[6]);
                    h = add(h, save[7]);
                }
                store_state(a, &mut st[0]);
                store_state(b, &mut st[1]);
                store_state(c, &mut st[2]);
                store_state(d, &mut st[3]);
                store_state(e, &mut st[4]);
                store_state(f, &mut st[5]);
                store_state(g, &mut st[6]);
                store_state(h, &mut st[7]);
                for l in 0..L {
                    for i in 0..8 {
                        state[l][i] = st[i][l];
                    }
                }
            }
        };
    }

    /// Hash `L` equal-length messages with the given compress function:
    /// whole blocks, then the padded tail (one or two blocks per lane).
    macro_rules! impl_hash {
        ($hname:ident, $cname:ident) => {
            pub fn $hname(lanes: &[&[u8]; L]) -> [[u8; 32]; L] {
                let len = lanes[0].len();
                for l in lanes {
                    assert_eq!(l.len(), len);
                }
                let whole = len / 64 * 64;
                let mut st = [H0; L];
                let heads: [&[u8]; L] = std::array::from_fn(|l| &lanes[l][..whole]);
                unsafe { $cname(&mut st, &heads) };
                let (t, n) = tail(len as u64);
                let lasts: Vec<Vec<u8>> = lanes
                    .iter()
                    .map(|l| {
                        let mut v = Vec::with_capacity(128);
                        v.extend_from_slice(&l[whole..]);
                        v.extend_from_slice(&t[..n]);
                        v
                    })
                    .collect();
                let refs: [&[u8]; L] = std::array::from_fn(|l| lasts[l].as_slice());
                unsafe { $cname(&mut st, &refs) };
                std::array::from_fn(|l| digest(&st[l]))
            }
        };
    }

    /// 16 lanes in zmm registers (AVX-512F/BW).
    pub mod z16 {
        use super::*;
        use core::arch::x86_64::*;

        pub type V = __m512i;
        pub const L: usize = 16;

        #[inline(always)]
        pub unsafe fn add(a: V, b: V) -> V {
            _mm512_add_epi32(a, b)
        }
        #[inline(always)]
        pub unsafe fn set1(k: u32) -> V {
            _mm512_set1_epi32(k as i32)
        }
        #[inline(always)]
        unsafe fn xor3(a: V, b: V, c: V) -> V {
            _mm512_ternarylogic_epi32::<0x96>(a, b, c)
        }
        #[inline(always)]
        pub unsafe fn bsig0(x: V) -> V {
            xor3(
                _mm512_ror_epi32::<2>(x),
                _mm512_ror_epi32::<13>(x),
                _mm512_ror_epi32::<22>(x),
            )
        }
        #[inline(always)]
        pub unsafe fn bsig1(x: V) -> V {
            xor3(
                _mm512_ror_epi32::<6>(x),
                _mm512_ror_epi32::<11>(x),
                _mm512_ror_epi32::<25>(x),
            )
        }
        #[inline(always)]
        pub unsafe fn ssig0(x: V) -> V {
            xor3(
                _mm512_ror_epi32::<7>(x),
                _mm512_ror_epi32::<18>(x),
                _mm512_srli_epi32::<3>(x),
            )
        }
        #[inline(always)]
        pub unsafe fn ssig1(x: V) -> V {
            xor3(
                _mm512_ror_epi32::<17>(x),
                _mm512_ror_epi32::<19>(x),
                _mm512_srli_epi32::<10>(x),
            )
        }
        #[inline(always)]
        pub unsafe fn ch(e: V, f: V, g: V) -> V {
            _mm512_ternarylogic_epi32::<0xCA>(e, f, g)
        }
        #[inline(always)]
        pub unsafe fn maj(a: V, b: V, c: V) -> V {
            _mm512_ternarylogic_epi32::<0xE8>(a, b, c)
        }
        #[inline(always)]
        pub unsafe fn load_state(s: &[u32; L]) -> V {
            _mm512_loadu_si512(s.as_ptr() as *const _)
        }
        #[inline(always)]
        pub unsafe fn store_state(v: V, s: &mut [u32; L]) {
            _mm512_storeu_si512(s.as_mut_ptr() as *mut _, v)
        }
        #[inline(always)]
        unsafe fn bswap(x: V) -> V {
            _mm512_shuffle_epi8(x, _mm512_loadu_si512(BSWAP.as_ptr() as *const _))
        }

        /// Ingest A: one `vpgatherdd` per message word. Lane offsets are
        /// relative to lane 0 and must fit in i32.
        pub mod gather {
            use super::*;
            pub struct Ctx {
                base: *const u8,
                idx: V,
            }
            #[inline(always)]
            pub unsafe fn prepare(lanes: &[&[u8]; L]) -> Ctx {
                let base = lanes[0].as_ptr();
                let mut off = [0i32; L];
                for l in 0..L {
                    let d = lanes[l].as_ptr() as isize - base as isize;
                    off[l] = i32::try_from(d).expect("lane farther than 2 GiB from lane 0");
                }
                Ctx {
                    base,
                    idx: _mm512_loadu_si512(off.as_ptr() as *const _),
                }
            }
            #[inline(always)]
            pub unsafe fn load(ctx: &Ctx, _lanes: &[&[u8]; L], off: usize, w: &mut [V; 16]) {
                let p = ctx.base.add(off);
                for j in 0..16 {
                    w[j] = bswap(_mm512_i32gather_epi32::<1>(
                        ctx.idx,
                        p.add(4 * j) as *const i32,
                    ));
                }
            }
        }

        /// Ingest B: one 64-byte load per lane, then a 16x16 dword transpose
        /// (64 shuffles per block).
        pub mod transpose {
            use super::*;
            pub struct Ctx;
            #[inline(always)]
            pub unsafe fn prepare(_lanes: &[&[u8]; L]) -> Ctx {
                Ctx
            }
            #[inline(always)]
            pub unsafe fn load(_ctx: &Ctx, lanes: &[&[u8]; L], off: usize, w: &mut [V; 16]) {
                let mut r: [V; 16] = [set1(0); 16];
                for l in 0..16 {
                    r[l] = bswap(_mm512_loadu_si512(lanes[l].as_ptr().add(off) as *const _));
                }
                let mut t: [V; 16] = [set1(0); 16];
                for i in 0..8 {
                    t[2 * i] = _mm512_unpacklo_epi32(r[2 * i], r[2 * i + 1]);
                    t[2 * i + 1] = _mm512_unpackhi_epi32(r[2 * i], r[2 * i + 1]);
                }
                let mut u: [V; 16] = [set1(0); 16];
                for i in 0..4 {
                    u[4 * i] = _mm512_unpacklo_epi64(t[4 * i], t[4 * i + 2]);
                    u[4 * i + 1] = _mm512_unpackhi_epi64(t[4 * i], t[4 * i + 2]);
                    u[4 * i + 2] = _mm512_unpacklo_epi64(t[4 * i + 1], t[4 * i + 3]);
                    u[4 * i + 3] = _mm512_unpackhi_epi64(t[4 * i + 1], t[4 * i + 3]);
                }
                // u[4g+k] holds word k, k+4, k+8, k+12 (one per 128-bit chunk)
                // of row group g (rows 4g..4g+3).
                let mut v: [V; 16] = [set1(0); 16];
                for g in 0..2 {
                    for k in 0..4 {
                        let x = u[8 * g + k];
                        let y = u[8 * g + 4 + k];
                        v[8 * g + 2 * k] = _mm512_shuffle_i32x4::<0x88>(x, y);
                        v[8 * g + 2 * k + 1] = _mm512_shuffle_i32x4::<0xdd>(x, y);
                    }
                }
                // v[8g+2k]   = word k    (chunks: rows 0-3, 4-7 of group g) | word k+8
                // v[8g+2k+1] = word k+4  | word k+12
                for k in 0..4 {
                    let x = v[2 * k];
                    let y = v[8 + 2 * k];
                    w[k] = _mm512_shuffle_i32x4::<0x88>(x, y);
                    w[k + 8] = _mm512_shuffle_i32x4::<0xdd>(x, y);
                    let x = v[2 * k + 1];
                    let y = v[8 + 2 * k + 1];
                    w[k + 4] = _mm512_shuffle_i32x4::<0x88>(x, y);
                    w[k + 12] = _mm512_shuffle_i32x4::<0xdd>(x, y);
                }
            }
        }

        /// Ingest C: the same 64-shuffle network, each shuffle pinned with
        /// inline asm so LLVM cannot rewrite it into a 2-source-permute net.
        pub mod transpose_asm {
            use super::*;
            use core::arch::asm;
            pub struct Ctx;
            macro_rules! shuf2 {
                ($name:ident, $ins:literal) => {
                    #[inline]
                    #[target_feature(enable = "avx512f,avx512bw")]
                    unsafe fn $name(a: V, b: V) -> V {
                        let r: V;
                        asm!($ins, out(zmm_reg) r, in(zmm_reg) a, in(zmm_reg) b, options(pure, nomem, nostack));
                        r
                    }
                };
            }
            shuf2!(unpacklo32, "vpunpckldq {0}, {1}, {2}");
            shuf2!(unpackhi32, "vpunpckhdq {0}, {1}, {2}");
            shuf2!(unpacklo64, "vpunpcklqdq {0}, {1}, {2}");
            shuf2!(unpackhi64, "vpunpckhqdq {0}, {1}, {2}");
            shuf2!(shuf88, "vshufi64x2 {0}, {1}, {2}, 0x88");
            shuf2!(shufdd, "vshufi64x2 {0}, {1}, {2}, 0xdd");
            shuf2!(pshufb, "vpshufb {0}, {1}, {2}");
            #[inline(always)]
            pub unsafe fn prepare(_lanes: &[&[u8]; L]) -> Ctx {
                Ctx
            }
            #[inline(always)]
            pub unsafe fn load(_ctx: &Ctx, lanes: &[&[u8]; L], off: usize, w: &mut [V; 16]) {
                let mask = _mm512_loadu_si512(BSWAP.as_ptr() as *const _);
                let mut r: [V; 16] = [set1(0); 16];
                for l in 0..16 {
                    r[l] = pshufb(
                        _mm512_loadu_si512(lanes[l].as_ptr().add(off) as *const _),
                        mask,
                    );
                }
                let mut t: [V; 16] = [set1(0); 16];
                for i in 0..8 {
                    t[2 * i] = unpacklo32(r[2 * i], r[2 * i + 1]);
                    t[2 * i + 1] = unpackhi32(r[2 * i], r[2 * i + 1]);
                }
                let mut u: [V; 16] = [set1(0); 16];
                for i in 0..4 {
                    u[4 * i] = unpacklo64(t[4 * i], t[4 * i + 2]);
                    u[4 * i + 1] = unpackhi64(t[4 * i], t[4 * i + 2]);
                    u[4 * i + 2] = unpacklo64(t[4 * i + 1], t[4 * i + 3]);
                    u[4 * i + 3] = unpackhi64(t[4 * i + 1], t[4 * i + 3]);
                }
                let mut v: [V; 16] = [set1(0); 16];
                for g in 0..2 {
                    for k in 0..4 {
                        let x = u[8 * g + k];
                        let y = u[8 * g + 4 + k];
                        v[8 * g + 2 * k] = shuf88(x, y);
                        v[8 * g + 2 * k + 1] = shufdd(x, y);
                    }
                }
                for k in 0..4 {
                    let x = v[2 * k];
                    let y = v[8 + 2 * k];
                    w[k] = shuf88(x, y);
                    w[k + 8] = shufdd(x, y);
                    let x = v[2 * k + 1];
                    let y = v[8 + 2 * k + 1];
                    w[k + 4] = shuf88(x, y);
                    w[k + 12] = shufdd(x, y);
                }
            }
        }

        impl_compress!(compress_gather, "avx512f,avx512bw,avx512vl", gather);
        impl_compress!(compress_transpose, "avx512f,avx512bw,avx512vl", transpose);
        impl_compress!(
            compress_transpose_asm,
            "avx512f,avx512bw,avx512vl",
            transpose_asm
        );
        impl_hash!(hash_gather, compress_gather);
        impl_hash!(hash_transpose, compress_transpose);
        impl_hash!(hash_transpose_asm, compress_transpose_asm);
    }

    /// 8 lanes in ymm registers with AVX-512VL (rotate and ternlog on ymm).
    pub mod y8vl {
        use super::*;
        use core::arch::x86_64::*;

        pub type V = __m256i;
        pub const L: usize = 8;

        #[inline(always)]
        pub unsafe fn add(a: V, b: V) -> V {
            _mm256_add_epi32(a, b)
        }
        #[inline(always)]
        pub unsafe fn set1(k: u32) -> V {
            _mm256_set1_epi32(k as i32)
        }
        #[inline(always)]
        unsafe fn xor3(a: V, b: V, c: V) -> V {
            _mm256_ternarylogic_epi32::<0x96>(a, b, c)
        }
        #[inline(always)]
        pub unsafe fn bsig0(x: V) -> V {
            xor3(
                _mm256_ror_epi32::<2>(x),
                _mm256_ror_epi32::<13>(x),
                _mm256_ror_epi32::<22>(x),
            )
        }
        #[inline(always)]
        pub unsafe fn bsig1(x: V) -> V {
            xor3(
                _mm256_ror_epi32::<6>(x),
                _mm256_ror_epi32::<11>(x),
                _mm256_ror_epi32::<25>(x),
            )
        }
        #[inline(always)]
        pub unsafe fn ssig0(x: V) -> V {
            xor3(
                _mm256_ror_epi32::<7>(x),
                _mm256_ror_epi32::<18>(x),
                _mm256_srli_epi32::<3>(x),
            )
        }
        #[inline(always)]
        pub unsafe fn ssig1(x: V) -> V {
            xor3(
                _mm256_ror_epi32::<17>(x),
                _mm256_ror_epi32::<19>(x),
                _mm256_srli_epi32::<10>(x),
            )
        }
        #[inline(always)]
        pub unsafe fn ch(e: V, f: V, g: V) -> V {
            _mm256_ternarylogic_epi32::<0xCA>(e, f, g)
        }
        #[inline(always)]
        pub unsafe fn maj(a: V, b: V, c: V) -> V {
            _mm256_ternarylogic_epi32::<0xE8>(a, b, c)
        }
        #[inline(always)]
        pub unsafe fn load_state(s: &[u32; L]) -> V {
            _mm256_loadu_si256(s.as_ptr() as *const _)
        }
        #[inline(always)]
        pub unsafe fn store_state(v: V, s: &mut [u32; L]) {
            _mm256_storeu_si256(s.as_mut_ptr() as *mut _, v)
        }

        pub mod transpose {
            use super::*;
            pub struct Ctx;
            #[inline(always)]
            pub unsafe fn prepare(_lanes: &[&[u8]; L]) -> Ctx {
                Ctx
            }
            #[inline(always)]
            pub unsafe fn load(_ctx: &Ctx, lanes: &[&[u8]; L], off: usize, w: &mut [V; 16]) {
                super::super::transpose8x8_ymm(lanes, off, w, |x| unsafe {
                    _mm256_shuffle_epi8(x, _mm256_loadu_si256(BSWAP.as_ptr() as *const _))
                });
            }
        }

        impl_compress!(
            compress_transpose,
            "avx2,avx512f,avx512bw,avx512vl",
            transpose
        );
        impl_hash!(hash_transpose, compress_transpose);
    }

    /// 8 lanes in ymm registers with plain AVX2: no rotate, no ternlog.
    pub mod y8avx2 {
        use super::*;
        use core::arch::x86_64::*;

        pub type V = __m256i;
        pub const L: usize = 8;

        #[inline(always)]
        pub unsafe fn add(a: V, b: V) -> V {
            _mm256_add_epi32(a, b)
        }
        #[inline(always)]
        pub unsafe fn set1(k: u32) -> V {
            _mm256_set1_epi32(k as i32)
        }
        #[inline(always)]
        unsafe fn ror<const N: i32, const M: i32>(x: V) -> V {
            _mm256_or_si256(_mm256_srli_epi32::<N>(x), _mm256_slli_epi32::<M>(x))
        }
        #[inline(always)]
        unsafe fn xor3(a: V, b: V, c: V) -> V {
            _mm256_xor_si256(_mm256_xor_si256(a, b), c)
        }
        #[inline(always)]
        pub unsafe fn bsig0(x: V) -> V {
            xor3(ror::<2, 30>(x), ror::<13, 19>(x), ror::<22, 10>(x))
        }
        #[inline(always)]
        pub unsafe fn bsig1(x: V) -> V {
            xor3(ror::<6, 26>(x), ror::<11, 21>(x), ror::<25, 7>(x))
        }
        #[inline(always)]
        pub unsafe fn ssig0(x: V) -> V {
            xor3(ror::<7, 25>(x), ror::<18, 14>(x), _mm256_srli_epi32::<3>(x))
        }
        #[inline(always)]
        pub unsafe fn ssig1(x: V) -> V {
            xor3(
                ror::<17, 15>(x),
                ror::<19, 13>(x),
                _mm256_srli_epi32::<10>(x),
            )
        }
        #[inline(always)]
        pub unsafe fn ch(e: V, f: V, g: V) -> V {
            _mm256_xor_si256(g, _mm256_and_si256(e, _mm256_xor_si256(f, g)))
        }
        #[inline(always)]
        pub unsafe fn maj(a: V, b: V, c: V) -> V {
            _mm256_or_si256(
                _mm256_and_si256(a, b),
                _mm256_and_si256(c, _mm256_or_si256(a, b)),
            )
        }
        #[inline(always)]
        pub unsafe fn load_state(s: &[u32; L]) -> V {
            _mm256_loadu_si256(s.as_ptr() as *const _)
        }
        #[inline(always)]
        pub unsafe fn store_state(v: V, s: &mut [u32; L]) {
            _mm256_storeu_si256(s.as_mut_ptr() as *mut _, v)
        }

        pub mod transpose {
            use super::*;
            pub struct Ctx;
            #[inline(always)]
            pub unsafe fn prepare(_lanes: &[&[u8]; L]) -> Ctx {
                Ctx
            }
            #[inline(always)]
            pub unsafe fn load(_ctx: &Ctx, lanes: &[&[u8]; L], off: usize, w: &mut [V; 16]) {
                super::super::transpose8x8_ymm(lanes, off, w, |x| unsafe {
                    _mm256_shuffle_epi8(x, _mm256_loadu_si256(BSWAP.as_ptr() as *const _))
                });
            }
        }

        impl_compress!(compress_transpose, "avx2", transpose);
        impl_hash!(hash_transpose, compress_transpose);
    }

    /// Two 8x8 dword transposes per block (words 0-7, then 8-15): 48 shuffles.
    #[inline(always)]
    unsafe fn transpose8x8_ymm(
        lanes: &[&[u8]; 8],
        off: usize,
        w: &mut [core::arch::x86_64::__m256i; 16],
        bswap: impl Fn(core::arch::x86_64::__m256i) -> core::arch::x86_64::__m256i,
    ) {
        use core::arch::x86_64::*;
        for half in 0..2 {
            let mut r = [_mm256_setzero_si256(); 8];
            for l in 0..8 {
                r[l] = bswap(_mm256_loadu_si256(
                    lanes[l].as_ptr().add(off + 32 * half) as *const _
                ));
            }
            let mut t = [_mm256_setzero_si256(); 8];
            for i in 0..4 {
                t[2 * i] = _mm256_unpacklo_epi32(r[2 * i], r[2 * i + 1]);
                t[2 * i + 1] = _mm256_unpackhi_epi32(r[2 * i], r[2 * i + 1]);
            }
            let mut u = [_mm256_setzero_si256(); 8];
            for i in 0..2 {
                u[4 * i] = _mm256_unpacklo_epi64(t[4 * i], t[4 * i + 2]);
                u[4 * i + 1] = _mm256_unpackhi_epi64(t[4 * i], t[4 * i + 2]);
                u[4 * i + 2] = _mm256_unpacklo_epi64(t[4 * i + 1], t[4 * i + 3]);
                u[4 * i + 3] = _mm256_unpackhi_epi64(t[4 * i + 1], t[4 * i + 3]);
            }
            for k in 0..4 {
                w[8 * half + k] = _mm256_permute2x128_si256::<0x20>(u[k], u[4 + k]);
                w[8 * half + k + 4] = _mm256_permute2x128_si256::<0x31>(u[k], u[4 + k]);
            }
        }
    }

    /// Actual core cycles (APERF) via the unprivileged `rdpru` (Zen 2+).
    pub fn has_rdpru() -> bool {
        unsafe { core::arch::x86_64::__cpuid(0x8000_0008).ebx & (1 << 4) != 0 }
    }
    #[inline(always)]
    pub fn aperf() -> u64 {
        let lo: u32;
        let hi: u32;
        unsafe {
            core::arch::asm!("rdpru", in("ecx") 1u32, out("eax") lo, out("edx") hi, options(nomem, nostack));
        }
        ((hi as u64) << 32) | lo as u64
    }

    /// Throughput probes: 12 independent ops per iteration, cycles from APERF.
    macro_rules! tput {
        ($name:ident, $($ins:expr),+ $(,)?) => {
            #[target_feature(enable = "avx512f,avx512bw,avx512vl,avx512vbmi2,sha")]
            pub unsafe fn $name(iters: u64) -> f64 {
                let c0 = aperf();
                core::arch::asm!(
                    "2:", $($ins,)+ "dec {n}", "jnz 2b",
                    n = inout(reg) iters => _,
                    out("zmm0") _, out("zmm1") _, out("zmm2") _, out("zmm3") _,
                    out("zmm4") _, out("zmm5") _, out("zmm6") _, out("zmm7") _,
                    out("zmm8") _, out("zmm9") _, out("zmm10") _, out("zmm11") _,
                    out("zmm12") _, out("zmm13") _, out("zmm14") _, out("zmm15") _,
                    options(nomem, nostack),
                );
                (aperf() - c0) as f64 / iters as f64 / 12.0
            }
        };
    }
    pub mod ports {
        use super::aperf;
        macro_rules! op12 {
            ($name:ident, $op:literal, $r:literal, $tail:literal) => {
                tput!(
                    $name,
                    concat!($op, " ", $r, "0, ", $r, "12", $tail),
                    concat!($op, " ", $r, "1, ", $r, "12", $tail),
                    concat!($op, " ", $r, "2, ", $r, "12", $tail),
                    concat!($op, " ", $r, "3, ", $r, "12", $tail),
                    concat!($op, " ", $r, "4, ", $r, "12", $tail),
                    concat!($op, " ", $r, "5, ", $r, "12", $tail),
                    concat!($op, " ", $r, "6, ", $r, "12", $tail),
                    concat!($op, " ", $r, "7, ", $r, "12", $tail),
                    concat!($op, " ", $r, "8, ", $r, "12", $tail),
                    concat!($op, " ", $r, "9, ", $r, "12", $tail),
                    concat!($op, " ", $r, "10, ", $r, "12", $tail),
                    concat!($op, " ", $r, "11, ", $r, "12", $tail),
                );
            };
        }
        macro_rules! mix12 {
            ($name:ident, $opa:literal, $ra:literal, $taila:literal, $opb:literal, $rb:literal, $tailb:literal) => {
                tput!(
                    $name,
                    concat!($opa, " ", $ra, "0, ", $ra, "12", $taila),
                    concat!($opb, " ", $rb, "1, ", $rb, "12", $tailb),
                    concat!($opa, " ", $ra, "2, ", $ra, "12", $taila),
                    concat!($opb, " ", $rb, "3, ", $rb, "12", $tailb),
                    concat!($opa, " ", $ra, "4, ", $ra, "12", $taila),
                    concat!($opb, " ", $rb, "5, ", $rb, "12", $tailb),
                    concat!($opa, " ", $ra, "6, ", $ra, "12", $taila),
                    concat!($opb, " ", $rb, "7, ", $rb, "12", $tailb),
                    concat!($opa, " ", $ra, "8, ", $ra, "12", $taila),
                    concat!($opb, " ", $rb, "9, ", $rb, "12", $tailb),
                    concat!($opa, " ", $ra, "10, ", $ra, "12", $taila),
                    concat!($opb, " ", $rb, "11, ", $rb, "12", $tailb),
                );
            };
        }
        mix12!(
            z_rol_shufi,
            "vprold",
            "zmm",
            ", 7",
            "vshufi64x2",
            "zmm",
            ", zmm13, 0x88"
        );
        mix12!(
            z_rol_perm2,
            "vprold",
            "zmm",
            ", 7",
            "vpermt2d",
            "zmm",
            ", zmm13"
        );
        mix12!(
            z_rol_pshufb,
            "vprold",
            "zmm",
            ", 7",
            "vpshufb",
            "zmm",
            ", zmm13"
        );
        op12!(z_shld, "vpshldd", "zmm", ", zmm13, 7");
        mix12!(
            z_rol_shld,
            "vprold",
            "zmm",
            ", 7",
            "vpshldd",
            "zmm",
            ", zmm13, 7"
        );
        mix12!(
            y_rol_unpck,
            "vprold",
            "ymm",
            ", 7",
            "vpunpckldq",
            "ymm",
            ", ymm13"
        );
        tput!(
            sha_rnds,
            "sha256rnds2 xmm1, xmm12",
            "sha256rnds2 xmm2, xmm12",
            "sha256rnds2 xmm3, xmm12",
            "sha256rnds2 xmm4, xmm12",
            "sha256rnds2 xmm5, xmm12",
            "sha256rnds2 xmm6, xmm12",
            "sha256rnds2 xmm7, xmm12",
            "sha256rnds2 xmm8, xmm12",
            "sha256rnds2 xmm9, xmm12",
            "sha256rnds2 xmm10, xmm12",
            "sha256rnds2 xmm11, xmm12",
            "sha256rnds2 xmm13, xmm12",
        );
        tput!(
            sha_z_rol,
            "sha256rnds2 xmm1, xmm12",
            "vprold zmm2, zmm14, 7",
            "sha256rnds2 xmm3, xmm12",
            "vprold zmm4, zmm14, 7",
            "sha256rnds2 xmm5, xmm12",
            "vprold zmm6, zmm14, 7",
            "sha256rnds2 xmm7, xmm12",
            "vprold zmm8, zmm14, 7",
            "sha256rnds2 xmm9, xmm12",
            "vprold zmm10, zmm14, 7",
            "sha256rnds2 xmm11, xmm12",
            "vprold zmm13, zmm14, 7",
        );
        tput!(
            sha_z_add,
            "sha256rnds2 xmm1, xmm12",
            "vpaddd zmm2, zmm14, zmm15",
            "sha256rnds2 xmm3, xmm12",
            "vpaddd zmm4, zmm14, zmm15",
            "sha256rnds2 xmm5, xmm12",
            "vpaddd zmm6, zmm14, zmm15",
            "sha256rnds2 xmm7, xmm12",
            "vpaddd zmm8, zmm14, zmm15",
            "sha256rnds2 xmm9, xmm12",
            "vpaddd zmm10, zmm14, zmm15",
            "sha256rnds2 xmm11, xmm12",
            "vpaddd zmm13, zmm14, zmm15",
        );
        tput!(
            sha_z_tern,
            "sha256rnds2 xmm1, xmm12",
            "vpternlogd zmm2, zmm14, zmm15, 0x96",
            "sha256rnds2 xmm3, xmm12",
            "vpternlogd zmm4, zmm14, zmm15, 0x96",
            "sha256rnds2 xmm5, xmm12",
            "vpternlogd zmm6, zmm14, zmm15, 0x96",
            "sha256rnds2 xmm7, xmm12",
            "vpternlogd zmm8, zmm14, zmm15, 0x96",
            "sha256rnds2 xmm9, xmm12",
            "vpternlogd zmm10, zmm14, zmm15, 0x96",
            "sha256rnds2 xmm11, xmm12",
            "vpternlogd zmm13, zmm14, zmm15, 0x96",
        );
        op12!(z_add, "vpaddd", "zmm", ", zmm13");
        op12!(z_xor, "vpxord", "zmm", ", zmm13");
        op12!(z_rol, "vprold", "zmm", ", 7");
        op12!(z_srl, "vpsrld", "zmm", ", 3");
        op12!(z_tern, "vpternlogd", "zmm", ", zmm13, 0x96");
        op12!(z_unpck, "vpunpckldq", "zmm", ", zmm13");
        op12!(z_shufi, "vshufi64x2", "zmm", ", zmm13, 0x88");
        op12!(z_perm2, "vpermt2d", "zmm", ", zmm13");
        op12!(z_pshufb, "vpshufb", "zmm", ", zmm13");
        mix12!(
            z_rol_add, "vprold", "zmm", ", 7", "vpaddd", "zmm", ", zmm13"
        );
        mix12!(
            z_rol_tern,
            "vprold",
            "zmm",
            ", 7",
            "vpternlogd",
            "zmm",
            ", zmm13, 0x96"
        );
        mix12!(
            z_rol_unpck,
            "vprold",
            "zmm",
            ", 7",
            "vpunpckldq",
            "zmm",
            ", zmm13"
        );
        mix12!(
            z_tern_add,
            "vpternlogd",
            "zmm",
            ", zmm13, 0x96",
            "vpaddd",
            "zmm",
            ", zmm13"
        );
        mix12!(
            z_tern_unpck,
            "vpternlogd",
            "zmm",
            ", zmm13, 0x96",
            "vpunpckldq",
            "zmm",
            ", zmm13"
        );
        mix12!(
            z_rol_xor, "vprold", "zmm", ", 7", "vpxord", "zmm", ", zmm13"
        );
        op12!(y_add, "vpaddd", "ymm", ", ymm13");
        op12!(y_rol, "vprold", "ymm", ", 7");
        op12!(y_srl, "vpsrld", "ymm", ", 3");
        op12!(y_tern, "vpternlogd", "ymm", ", ymm13, 0x96");
        op12!(y_unpck, "vpunpckldq", "ymm", ", ymm13");
        op12!(y_por, "vpor", "ymm", ", ymm13");
        mix12!(
            y_rol_add, "vprold", "ymm", ", 7", "vpaddd", "ymm", ", ymm13"
        );
        mix12!(
            y_rol_tern,
            "vprold",
            "ymm",
            ", 7",
            "vpternlogd",
            "ymm",
            ", ymm13, 0x96"
        );
        mix12!(
            y_tern_add,
            "vpternlogd",
            "ymm",
            ", ymm13, 0x96",
            "vpaddd",
            "ymm",
            ", ymm13"
        );

        pub fn run_all() {
            let n = 1u64 << 22;
            let tests: &[(&str, unsafe fn(u64) -> f64)] = &[
                ("z_add", z_add),
                ("z_xor", z_xor),
                ("z_rol", z_rol),
                ("z_srl", z_srl),
                ("z_tern", z_tern),
                ("z_unpck", z_unpck),
                ("z_shufi", z_shufi),
                ("z_perm2", z_perm2),
                ("z_pshufb", z_pshufb),
                ("z_rol+add", z_rol_add),
                ("z_rol+tern", z_rol_tern),
                ("z_rol+unpck", z_rol_unpck),
                ("z_tern+add", z_tern_add),
                ("z_tern+unpck", z_tern_unpck),
                ("z_rol+xor", z_rol_xor),
                ("y_add", y_add),
                ("y_rol", y_rol),
                ("y_srl", y_srl),
                ("y_tern", y_tern),
                ("y_unpck", y_unpck),
                ("y_por", y_por),
                ("y_rol+add", y_rol_add),
                ("y_rol+tern", y_rol_tern),
                ("y_tern+add", y_tern_add),
                ("y_rol+unpck", y_rol_unpck),
                ("z_rol+shufi", z_rol_shufi),
                ("z_rol+perm2", z_rol_perm2),
                ("z_rol+pshufb", z_rol_pshufb),
                ("z_shld", z_shld),
                ("z_rol+shld", z_rol_shld),
                ("sha_rnds2", sha_rnds),
                ("sha+z_rol", sha_z_rol),
                ("sha+z_add", sha_z_add),
                ("sha+z_tern", sha_z_tern),
            ];
            for (name, f) in tests {
                let best = (0..5)
                    .map(|_| unsafe { f(n) })
                    .fold(f64::INFINITY, f64::min);
                println!(
                    "# port {name} cycles_per_op {best:.3} ops_per_cycle {:.2}",
                    1.0 / best
                );
            }
        }
    }
    /// Core clock from a dependent chain of 1-cycle ops: 8 per iteration.
    pub fn clock_ghz_scalar(iters: u64) -> f64 {
        let t = Instant::now();
        unsafe {
            core::arch::asm!(
                "2:",
                "add {x}, 1", "add {x}, 1", "add {x}, 1", "add {x}, 1",
                "add {x}, 1", "add {x}, 1", "add {x}, 1", "add {x}, 1",
                "dec {n}",
                "jnz 2b",
                x = inout(reg) 0u64 => _,
                n = inout(reg) iters => _,
                options(nomem, nostack),
            );
        }
        8.0 * iters as f64 / t.elapsed().as_secs_f64() / 1e9
    }

    /// Same, on a zmm add chain (vpaddd latency 1 on Zen 4).
    #[target_feature(enable = "avx512f")]
    pub unsafe fn clock_ghz_zmm(iters: u64) -> f64 {
        let t = Instant::now();
        core::arch::asm!(
            "vpxord zmm0, zmm0, zmm0",
            "vpternlogd zmm1, zmm1, zmm1, 0xff",
            "2:",
            "vpaddd zmm0, zmm0, zmm1", "vpaddd zmm0, zmm0, zmm1",
            "vpaddd zmm0, zmm0, zmm1", "vpaddd zmm0, zmm0, zmm1",
            "vpaddd zmm0, zmm0, zmm1", "vpaddd zmm0, zmm0, zmm1",
            "vpaddd zmm0, zmm0, zmm1", "vpaddd zmm0, zmm0, zmm1",
            "dec {n}",
            "jnz 2b",
            n = inout(reg) iters => _,
            out("zmm0") _, out("zmm1") _,
            options(nomem, nostack),
        );
        8.0 * iters as f64 / t.elapsed().as_secs_f64() / 1e9
    }

    use std::time::Instant;
}

#[cfg(target_arch = "x86_64")]
fn main() {
    use kernel::*;
    if !is_x86_feature_detected!("avx512f") || !is_x86_feature_detected!("avx512bw") {
        eprintln!("sha256_avx512: this cpu has no AVX-512");
        std::process::exit(1);
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    let runs: u32 = args.first().and_then(|s| s.parse().ok()).unwrap_or(7);
    let sizes: Vec<usize> = if args.len() > 1 {
        args[1..].iter().filter_map(|s| s.parse().ok()).collect()
    } else {
        vec![1024, 1 << 16, 1 << 20]
    };

    let fill = |n: usize, j: usize| -> Vec<u8> {
        (0..n)
            .map(|i| {
                (i as u8)
                    .wrapping_mul(7)
                    .wrapping_add((j as u8).wrapping_mul(13))
            })
            .collect()
    };
    let mut lens = vec![0usize, 1, 55, 56, 64, 119, 120, 1000, 4096];
    lens.extend(sizes.iter().copied());
    for &n in &lens {
        let bufs: Vec<Vec<u8>> = (0..16).map(|j| fill(n, j)).collect();
        let want: Vec<[u8; 32]> = bufs
            .iter()
            .map(|b| {
                let h = hash_bytes(HashAlgo::Sha256, b);
                let mut d = [0u8; 32];
                d.copy_from_slice(h.bytes());
                d
            })
            .collect();
        let l16: [&[u8]; 16] = std::array::from_fn(|l| bufs[l].as_slice());
        let l8: [&[u8]; 8] = std::array::from_fn(|l| bufs[l].as_slice());
        let got = z16::hash_transpose(&l16);
        for l in 0..16 {
            assert_eq!(got[l], want[l], "z16 transpose, size {n}, lane {l}");
        }
        let got = z16::hash_gather(&l16);
        for l in 0..16 {
            assert_eq!(got[l], want[l], "z16 gather, size {n}, lane {l}");
        }
        let got = z16::hash_transpose_asm(&l16);
        for l in 0..16 {
            assert_eq!(got[l], want[l], "z16 transpose_asm, size {n}, lane {l}");
        }
        let got = y8vl::hash_transpose(&l8);
        for l in 0..8 {
            assert_eq!(got[l], want[l], "y8vl transpose, size {n}, lane {l}");
        }
        let got = y8avx2::hash_transpose(&l8);
        for l in 0..8 {
            assert_eq!(got[l], want[l], "y8avx2 transpose, size {n}, lane {l}");
        }
    }
    println!("# correctness ok ({} sizes, all lanes)", lens.len());
    if has_rdpru() {
        ports::run_all();
    }

    let rdpru = has_rdpru();
    let c0 = if rdpru { aperf() } else { 0 };
    let ghz_s = (0..5)
        .map(|_| clock_ghz_scalar(1 << 25))
        .fold(0.0, f64::max);
    let c1 = if rdpru { aperf() } else { 0 };
    let ghz_z = (0..5)
        .map(|_| unsafe { clock_ghz_zmm(1 << 25) })
        .fold(0.0, f64::max);
    println!("# clock_scalar_ghz {ghz_s:.3} clock_zmm_ghz {ghz_z:.3} (dependent-chain estimate)");
    println!(
        "# rdpru {} aperf_cycles_per_add_iter {:.2} (expect 8.0)",
        rdpru,
        (c1 - c0) as f64 / (5.0 * (1u64 << 25) as f64)
    );

    for n in sizes {
        let whole = n / 64 * 64;
        let bufs: Vec<Vec<u8>> = (0..16).map(|j| fill(whole, j)).collect();
        let l16: [&[u8]; 16] = std::array::from_fn(|l| bufs[l].as_slice());
        let l8: [&[u8]; 8] = std::array::from_fn(|l| bufs[l].as_slice());
        let blocks = (whole / 64).max(1) as f64;
        let bench = |name: &str, nbytes: usize, mut f: Box<dyn FnMut() + '_>| {
            let iters: u32 = (((1 << 26) / nbytes.max(1)) as u32).clamp(4, 100_000);
            for _ in 0..4 {
                f();
            }
            let mut best = f64::INFINITY;
            let mut best_cyc = f64::INFINITY;
            for _ in 0..runs {
                let c0 = if rdpru { aperf() } else { 0 };
                let t = Instant::now();
                for _ in 0..iters {
                    f();
                }
                let s = t.elapsed().as_secs_f64() / iters as f64;
                let c1 = if rdpru { aperf() } else { 0 };
                best = best.min(s);
                best_cyc = best_cyc.min((c1 - c0) as f64 / iters as f64);
            }
            let cyc = if rdpru { best_cyc } else { best * ghz_z * 1e9 };
            println!(
                "{name} {nbytes} {runs} {:.0} {:.2} cyc/blockstep={:.1} cyc/lane-block={:.1} ghz={:.2}",
                best * 1e9,
                nbytes as f64 / best / 1e9,
                cyc / blocks,
                cyc / blocks / (nbytes / whole.max(1)) as f64,
                cyc / (best * 1e9),
            );
        };
        bench(
            "z16_transpose",
            16 * whole,
            Box::new(|| {
                let mut st = [H0; 16];
                unsafe { z16::compress_transpose(&mut st, black_box(&l16)) };
                black_box(st);
            }),
        );
        bench(
            "z16_transpose_asm",
            16 * whole,
            Box::new(|| {
                let mut st = [H0; 16];
                unsafe { z16::compress_transpose_asm(&mut st, black_box(&l16)) };
                black_box(st);
            }),
        );
        bench(
            "z16_gather",
            16 * whole,
            Box::new(|| {
                let mut st = [H0; 16];
                unsafe { z16::compress_gather(&mut st, black_box(&l16)) };
                black_box(st);
            }),
        );
        bench(
            "y8vl_transpose",
            8 * whole,
            Box::new(|| {
                let mut st = [H0; 8];
                unsafe { y8vl::compress_transpose(&mut st, black_box(&l8)) };
                black_box(st);
            }),
        );
        bench(
            "y8avx2_transpose",
            8 * whole,
            Box::new(|| {
                let mut st = [H0; 8];
                unsafe { y8avx2::compress_transpose(&mut st, black_box(&l8)) };
                black_box(st);
            }),
        );
    }
}

#[cfg(not(target_arch = "x86_64"))]
fn main() {
    eprintln!("sha256_avx512: x86_64 only");
}

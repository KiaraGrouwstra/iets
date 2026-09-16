//! `Word` is the one-machine-word value representation.
//!
//! Low three bits are the tag:
//!
//! | tag | meaning |
//! |-----|---------|
//! | 000 | pointer to a heap object (see `value.rs`); the word is never 0 |
//! | 001 | 61-bit immediate integer |
//! | 010 | pointer to a thunk object, not yet evaluated |
//! | 011 | immediates: null, true, false, blackhole |
//!
//! `Word(0)` is EMPTY: no value. Only thunk result cells hold it.

use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct Word(pub u64);

pub const TAG_MASK: u64 = 0b111;
pub const TAG_PTR: u64 = 0b000;
pub const TAG_INT: u64 = 0b001;
pub const TAG_THUNK: u64 = 0b010;
pub const TAG_THUNK_APP: u64 = 0b110;
pub const TAG_THUNK_APP2: u64 = 0b100;
pub const TAG_LAMBDA: u64 = 0b101;
pub const TAG_IMM: u64 = 0b011;

const IMM_NULL: u64 = TAG_IMM;
const IMM_TRUE: u64 = TAG_IMM | (1 << 3);
const IMM_FALSE: u64 = TAG_IMM | (2 << 3);
const IMM_BLACKHOLE: u64 = TAG_IMM | (3 << 3);

pub const INT_MIN: i64 = -(1 << 60);
pub const INT_MAX: i64 = (1 << 60) - 1;

impl Word {
    pub const EMPTY: Word = Word(0);
    pub const NULL: Word = Word(IMM_NULL);
    pub const TRUE: Word = Word(IMM_TRUE);
    pub const FALSE: Word = Word(IMM_FALSE);
    pub const BLACKHOLE: Word = Word(IMM_BLACKHOLE);

    #[inline]
    pub fn tag(self) -> u64 {
        self.0 & TAG_MASK
    }

    #[inline]
    pub fn from_bool(b: bool) -> Word {
        if b { Word::TRUE } else { Word::FALSE }
    }

    /// Immediate integer. Caller checks the range with `fits_imm`.
    #[inline]
    pub fn imm_int(v: i64) -> Word {
        debug_assert!(Self::fits_imm(v));
        Word(((v << 3) as u64) | TAG_INT)
    }

    #[inline]
    pub fn fits_imm(v: i64) -> bool {
        (INT_MIN..=INT_MAX).contains(&v)
    }

    #[inline]
    pub fn is_imm_int(self) -> bool {
        self.tag() == TAG_INT
    }

    #[inline]
    pub fn imm_int_value(self) -> i64 {
        (self.0 as i64) >> 3
    }

    #[inline]
    pub fn is_thunk(self) -> bool {
        // tags 010, 100, 110: even, and not the pointer tag 000
        self.0 & 0b001 == 0 && self.0 & 0b110 != 0
    }

    #[inline]
    pub fn is_ptr(self) -> bool {
        self.tag() == TAG_PTR && self.0 != 0
    }

    #[inline]
    pub fn is_bool(self) -> bool {
        self == Word::TRUE || self == Word::FALSE
    }

    #[inline]
    pub fn ptr<T>(self) -> *const T {
        (self.0 & !TAG_MASK) as *const T
    }

    #[inline]
    pub fn from_ptr<T>(p: *const T) -> Word {
        debug_assert!(p as u64 & TAG_MASK == 0);
        Word(p as u64)
    }

    #[inline]
    pub fn thunk_from_ptr<T>(p: *const T) -> Word {
        debug_assert!(p as u64 & TAG_MASK == 0);
        Word(p as u64 | TAG_THUNK)
    }

    #[inline]
    pub fn thunk_app_from_ptr<T>(p: *const T) -> Word {
        debug_assert!(p as u64 & TAG_MASK == 0);
        Word(p as u64 | TAG_THUNK_APP)
    }

    #[inline]
    pub fn thunk_app2_from_ptr<T>(p: *const T) -> Word {
        debug_assert!(p as u64 & TAG_MASK == 0);
        Word(p as u64 | TAG_THUNK_APP2)
    }

    #[inline]
    pub fn lambda_from_ptr<T>(p: *const T) -> Word {
        debug_assert!(p as u64 & TAG_MASK == 0);
        Word(p as u64 | TAG_LAMBDA)
    }
}

/// A mutable slot holding a `Word`. Env slots, list elements, attr values and
/// thunk results are cells.
///
/// Single-threaded today, so every access is `Relaxed`: it compiles to a
/// plain load or store on x86-64 and AArch64. The parallel phase changes
/// `set` on thunk results to a compare-exchange claim protocol and readers to
/// `Acquire`; the layout stays the same, which is the point of using an atomic
/// from day one.
#[repr(transparent)]
pub struct Cell(AtomicU64);

impl Cell {
    #[inline]
    pub const fn new(w: Word) -> Cell {
        Cell(AtomicU64::new(w.0))
    }

    #[inline]
    pub fn get(&self) -> Word {
        Word(self.0.load(Ordering::Relaxed))
    }

    #[inline]
    pub fn set(&self, w: Word) {
        self.0.store(w.0, Ordering::Relaxed)
    }
}

impl std::fmt::Debug for Word {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.tag() {
            TAG_INT => write!(f, "Int({})", self.imm_int_value()),
            TAG_THUNK => write!(f, "Thunk({:#x})", self.0),
            TAG_THUNK_APP => write!(f, "ThunkApp({:#x})", self.0),
            TAG_THUNK_APP2 => write!(f, "ThunkApp2({:#x})", self.0),
            TAG_LAMBDA => write!(f, "Lambda({:#x})", self.0),
            TAG_IMM => match *self {
                Word::NULL => write!(f, "null"),
                Word::TRUE => write!(f, "true"),
                Word::FALSE => write!(f, "false"),
                Word::BLACKHOLE => write!(f, "blackhole"),
                _ => write!(f, "Imm({:#x})", self.0),
            },
            _ => {
                if self.0 == 0 {
                    write!(f, "EMPTY")
                } else {
                    write!(f, "Ptr({:#x})", self.0)
                }
            }
        }
    }
}

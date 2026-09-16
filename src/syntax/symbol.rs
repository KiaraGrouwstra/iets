use std::collections::HashMap;

/// Interned identifier or attribute name. Ids are dense and start at 0.
/// Attrsets sort by id, so id order is arbitrary but stable within a run.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct Sym(pub u32);

#[derive(Default)]
pub struct Interner {
    map: FxHashMap<Box<[u8]>, Sym>,
    names: Vec<Box<[u8]>>,
}

impl Interner {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn intern(&mut self, name: &[u8]) -> Sym {
        if let Some(&s) = self.map.get(name) {
            return s;
        }
        let s = Sym(self.names.len() as u32);
        let boxed: Box<[u8]> = name.into();
        self.names.push(boxed.clone());
        self.map.insert(boxed, s);
        s
    }

    pub fn get(&self, name: &[u8]) -> Option<Sym> {
        self.map.get(name).copied()
    }

    pub fn name(&self, s: Sym) -> &[u8] {
        &self.names[s.0 as usize]
    }

    pub fn name_str(&self, s: Sym) -> &str {
        std::str::from_utf8(self.name(s)).unwrap_or("<non-utf8>")
    }

    pub fn len(&self) -> usize {
        self.names.len()
    }

    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }
}

/// FxHash: one multiply per word. SipHash was 4% of the profile on the
/// interner and the compiler's scope maps, whose keys are short.
#[derive(Default, Clone, Copy)]
pub struct FxHasher(u64);

const FX_SEED: u64 = 0x517c_c1b7_2722_0a95;

impl FxHasher {
    #[inline]
    fn add(&mut self, w: u64) {
        self.0 = (self.0.rotate_left(5) ^ w).wrapping_mul(FX_SEED);
    }
}

impl std::hash::Hasher for FxHasher {
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        let mut b = bytes;
        while b.len() >= 8 {
            self.add(u64::from_le_bytes(b[..8].try_into().unwrap()));
            b = &b[8..];
        }
        if b.len() >= 4 {
            self.add(u32::from_le_bytes(b[..4].try_into().unwrap()) as u64);
            b = &b[4..];
        }
        for &x in b {
            self.add(x as u64);
        }
    }
    #[inline]
    fn write_u32(&mut self, i: u32) {
        self.add(i as u64);
    }
    #[inline]
    fn write_u64(&mut self, i: u64) {
        self.add(i);
    }
    #[inline]
    fn write_usize(&mut self, i: usize) {
        self.add(i as u64);
    }
    #[inline]
    fn finish(&self) -> u64 {
        self.0
    }
}

pub type FxBuild = std::hash::BuildHasherDefault<FxHasher>;
pub type FxHashMap<K, V> = HashMap<K, V, FxBuild>;

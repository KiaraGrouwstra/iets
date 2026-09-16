use std::collections::HashMap;

/// Interned identifier or attribute name. Ids are dense and start at 0.
/// Attrsets sort by id, so id order is arbitrary but stable within a run.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct Sym(pub u32);

#[derive(Default)]
pub struct Interner {
    map: HashMap<Box<[u8]>, Sym>,
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

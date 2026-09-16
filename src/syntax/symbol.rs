/// Interned identifier or attribute name. Ids are dense and start at 0.
/// Attrsets sort by id, so id order is arbitrary but stable within a run.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct Sym(pub u32);

pub use crate::parallel::interner::Interner;

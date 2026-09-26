use crate::backend::{Backend, MemBackend};

pub fn walk(all: &[MemBackend], maybe: Option<&MemBackend>) -> usize {
    let mut n = crate::store::compact();
    for b in all {
        n += b.list().len();
    }
    n += all.iter().map(|b| b.list().len()).sum::<usize>();
    if let Some(m) = maybe {
        n += m.list().len();
    }
    n
}

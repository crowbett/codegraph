pub mod backend;
pub mod repo;

use backend::{Backend, FsBackend, MemBackend};
use repo::Repo;

pub fn short(n: u32) -> String {
    n.to_string()
}

pub fn open_all() -> Result<usize, String> {
    let fs = FsBackend::new("/tmp");
    let repo = Repo::open(Box::new(fs))?;
    let n = repo.count();
    assert_eq!(short(n as u32), "0");
    let names = vec![short(1), short(2)];
    println!("{}", repo.name().len());
    let mem = MemBackend::default();
    mem.list();
    let b: &dyn Backend = &mem;
    b.list();
    Ok(names.len() + repo.backend().list().len())
}

pub fn generic<B: Backend>(b: &B) -> usize {
    b.list().len()
}

pub fn by_impl(b: impl Backend) -> usize {
    b.list().len()
}

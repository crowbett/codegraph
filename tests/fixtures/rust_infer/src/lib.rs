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

pub mod store;
pub mod walk;
pub mod shared;

pub fn parse<T: Default>(_s: &str) -> T {
    T::default()
}

pub fn turbofish() {
    assert_eq!(parse::<u32>("1"), 0);
    let r = Repo::open(Box::new(MemBackend::default())).unwrap();
    assert!(r.name().is_empty() && r.cast::<u8>() == 0);
}
pub mod ext;
pub mod io_util;
pub mod uses;
pub mod value;
pub mod buf;
pub mod sysx;
pub mod walker;
pub mod scan_use;
pub mod overload;
pub mod slices;
pub mod gen;
pub mod conv;
pub mod pat;
pub mod turbo;
pub mod assoc2;
pub mod prim;
pub mod arr;
pub mod tmacro;
pub mod sup;
pub mod selfdef;

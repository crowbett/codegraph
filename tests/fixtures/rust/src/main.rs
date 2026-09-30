mod fmt;
mod store;
mod util;

use store::{Backend, Store};

fn main() {
    let s = Store::new();
    s.get("k");
    util::helper();
    let _ = util::new();
    run(&s);
    let _ = store::Kind::Missing;
    assert!(matches!(store::Kind::Missing, Kind::Missing));
}

use store::Kind;

fn run<B: Backend>(b: &B) -> usize {
    b.load()
}

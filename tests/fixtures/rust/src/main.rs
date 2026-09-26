mod store;
mod util;

use store::{Backend, Store};

fn main() {
    let s = Store::new();
    s.get("k");
    util::helper();
    let _ = util::new();
    run(&s);
}

fn run<B: Backend>(b: &B) -> usize {
    b.load()
}

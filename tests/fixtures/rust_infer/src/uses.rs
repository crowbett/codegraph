use crate::io_util::write;
use std::fmt::Write as _;

pub fn run() -> usize {
    let mut s = String::new();
    write!(s, "x").ok();
    write("p", &s)
}

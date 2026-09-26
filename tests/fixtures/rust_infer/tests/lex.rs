#[path = "../src/walk.rs"]
mod walk;

#[path = "lex/cases.rs"]
mod cases;

#[cfg(not(feature = "std"))]
#[path = "lex/alt.rs"]
mod imp;
#[cfg(feature = "std")]
use std::io as imp;

pub fn run() -> usize {
    imp::stub()
}

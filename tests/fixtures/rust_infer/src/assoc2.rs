use crate::pat::Num;

pub struct Cnt;

pub trait Mk {
    type Num;
}

impl Mk for Cnt {
    type Num = u8;
}

pub fn get(n: &Num) -> f64 {
    n.as_f64()
}

pub trait Float {
    fn exponent_limit() -> i32;
}

impl Float for f64 {
    fn exponent_limit() -> i32 {
        308
    }
}

pub trait Math {
    fn from_u64(x: u64) -> Self;
    fn hi(&self) -> u64 {
        0
    }
}

pub struct Big;

impl Math for Big {
    fn from_u64(_x: u64) -> Self {
        Big
    }
}

pub struct Unit;

impl Unit {
    pub fn go(&self) -> u8 {
        1
    }
}

pub fn run() -> i32 {
    let b = Big::from_u64(1);
    let _ = std::mem::ManuallyDrop::new(Unit).go();
    let _ = Unit.go();
    let _ = Big::hi(&b);
    let p: *mut Unit = std::ptr::null_mut();
    let _ = unsafe { (*p).go() };
    f64::exponent_limit()
}

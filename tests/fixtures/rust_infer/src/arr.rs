pub trait Put {
    fn put1(&mut self);
}

impl Put for &mut [u8] {
    fn put1(&mut self) {}
}

pub struct Wide;

impl Wide {
    pub fn go(&self) -> u8 {
        0
    }
}

pub fn run() -> u8 {
    let mut a = [0u8; 4];
    a.put1();
    let r = &mut Wide;
    r.go()
}

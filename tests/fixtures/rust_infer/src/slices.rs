pub struct Point {
    x: u8,
}

pub trait Show {
    fn show(&self) -> u8;
}

impl<'a> Show for &'a Point {
    fn show(&self) -> u8 {
        self.x
    }
}

pub trait PutBytes {
    fn put_bytes(&mut self, b: u8);
}

impl PutBytes for &mut [u8] {
    fn put_bytes(&mut self, b: u8) {
        self[0] = b;
    }
}

pub fn go(p: &Point, mut buf: &mut [u8]) -> u8 {
    buf.put_bytes(1);
    p.show()
}

pub trait Formatter {
    fn write_null(&mut self) -> u8 {
        0
    }
}

pub struct Ser<W, F> {
    writer: W,
    formatter: F,
}

impl<W, F: Formatter> Ser<W, F> {
    pub fn null(&mut self) -> u8 {
        self.formatter.write_null()
    }
}

pub trait Reader {
    fn discard(&mut self);
}

pub fn skip<R: Reader>(r: &mut R) {
    R::discard(r);
}

pub trait Len {
    fn len(&self) -> usize;
}

impl Len for [u8] {
    fn len(&self) -> usize {
        0
    }
}

pub trait Show {
    fn show(&self);
}

impl Show for [u8] {
    fn show(&self) {
        self.len();
    }
}

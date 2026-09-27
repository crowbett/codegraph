pub struct Chain {
    a: super::Buf,
}

impl Chain {
    pub fn len(&self) -> usize {
        self.a.remaining()
    }
}

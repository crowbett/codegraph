pub trait Hash {
    fn hash(&self);
}

impl Hash for [u8] {
    fn hash(&self) {
        self.len();
    }
}

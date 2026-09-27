pub struct Buf {
    n: usize,
}

impl Buf {
    pub fn remaining(&self) -> usize {
        self.n
    }
}

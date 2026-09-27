pub struct Value {
    n: usize,
}

impl Value {
    pub fn pointer(&self) -> usize {
        self.n
    }
}

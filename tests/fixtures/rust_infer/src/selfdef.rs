#[derive(Default)]
pub struct Acc2 {
    n: u8,
}

impl Acc2 {
    pub fn bump(&mut self) {
        self.n += 1;
    }

    pub fn fresh() -> Self {
        let mut v = Self::default();
        v.bump();
        v
    }
}

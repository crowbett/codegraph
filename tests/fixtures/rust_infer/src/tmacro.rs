#[derive(Default)]
pub struct Kp {
    n: u8,
}

impl Kp {
    pub fn is_empty(&self) -> bool {
        self.n == 0
    }

    pub fn me(&self) -> &Kp {
        self
    }
}

pub fn t() {
    assert!(Kp::default().is_empty());
    let k = Kp::default();
    assert!(k.me().is_empty());
}

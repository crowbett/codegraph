pub struct Num {
    v: u8,
}

impl Num {
    pub fn as_f64(&self) -> f64 {
        0.0
    }
}

pub enum Val {
    Number(Num),
    Map { inner: Num },
}

pub fn pair() -> (Num, bool) {
    (Num { v: 1 }, true)
}

pub fn walk(v: &Val) -> f64 {
    let (f, _ok) = pair();
    let a = f.as_f64();
    match v {
        Val::Number(n) => n.as_f64() + a,
        Val::Map { inner } => inner.as_f64(),
    }
}

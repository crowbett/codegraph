pub trait AsCast {
    fn as_cast(x: u64) -> Self;
}

pub trait Float: AsCast + Copy {
    fn pow10(self) -> Self;
}

pub fn make<F: Float>(m: u64) -> F {
    F::as_cast(m)
}

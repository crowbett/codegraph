pub fn from_ns(n: u64) -> u64 {
    n
}

mod imp {
    pub fn go() -> u64 {
        super::from_ns(1)
    }
}

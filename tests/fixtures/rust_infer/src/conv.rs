pub struct Buf3 {
    n: usize,
}

impl From<&'static [u8]> for Buf3 {
    fn from(s: &'static [u8]) -> Buf3 {
        Buf3 { n: s.len() }
    }
}

impl From<Vec<u8>> for Buf3 {
    fn from(v: Vec<u8>) -> Buf3 {
        Buf3 { n: v.len() }
    }
}

impl From<Buf3> for Vec<u8> {
    fn from(b: Buf3) -> Vec<u8> {
        vec![0; b.n]
    }
}

const LONG: &[u8] = b"long";

pub fn conv() -> Vec<u8> {
    let a = Buf3::from(&b"abc"[..]);
    let b = Buf3::from(vec![1, 2]);
    let c = Buf3::from(LONG);
    let _ = (a, b);
    Vec::from(c)
}

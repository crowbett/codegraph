pub struct Acc {
    n: usize,
}

impl Extend<u8> for Acc {
    fn extend<T: IntoIterator<Item = u8>>(&mut self, iter: T) {
        self.n += iter.into_iter().count();
    }
}

impl Extend<u16> for Acc {
    fn extend<T: IntoIterator<Item = u16>>(&mut self, iter: T) {
        self.n += 2 * iter.into_iter().count();
    }
}

pub fn fill(a: &mut Acc) {
    a.extend(vec![1u8]);
}

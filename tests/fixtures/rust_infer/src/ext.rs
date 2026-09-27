pub trait Put {
    fn put_one(&mut self, b: u8);
    fn twice(&mut self, b: u8) {
        self.put_one(b);
        self.put_one(b);
    }
}

impl Put for Vec<u8> {
    fn put_one(&mut self, b: u8) {
        self.push(b)
    }
}

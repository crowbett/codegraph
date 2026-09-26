pub mod x;
pub mod y;

pub struct Snap;
impl Snap {
    pub fn get(&self) -> u8 { 0 }
    pub fn parent(&self) -> Option<Snap> { None }
    pub fn save(&self) {}
}

pub struct Db;
impl Db {
    pub fn read(&self) -> Snap { Snap }
    pub fn get(&self) -> u8 { 1 }
    pub fn save(&self) {}
}

pub struct Other;
impl Other {
    pub fn save(&self) {}
}

pub trait Put {
    fn put(&self);
}

pub fn h1(db: Db) -> u8 {
    db.read().get()
}

pub fn h2(repo: Db, items: Vec<(Other, u8)>) {
    items.iter().for_each(|(repo, _)| repo.save());
    repo.save();
}

pub fn m1(node: Snap) {
    if let Some(node) = node.parent() {
        node.save();
    }
}

pub fn h3<B: ?Sized + Put>(b: &B) {
    b.put();
}

pub fn h3b<B: Clone + Put>(b: B) {
    b.put();
}

pub fn h4(c: x::Config) {
    c.only_x();
}

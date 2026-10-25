use std::collections::BTreeMap;

#[derive(Default)]
pub struct Store {
    map: BTreeMap<String, String>,
    writes: usize,
}

impl Store {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.map.get(key).map(String::as_str)
    }

    pub fn set(&mut self, key: String, value: String) {
        self.bump();
        self.map.insert(key, value);
    }

    pub fn delete(&mut self, key: &str) -> bool {
        self.bump();
        self.map.remove(key).is_some()
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    fn bump(&mut self) {
        self.writes += 1;
    }
}

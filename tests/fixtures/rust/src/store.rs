//! Storage.

/// A key-value store.
#[derive(Debug, Default)]
pub struct Store {
    items: Vec<String>,
}

/// What went wrong.
pub enum Kind {
    Missing,
}

/// Storage backends.
pub trait Backend {
    /// Load everything.
    fn load(&self) -> usize;
}

impl Store {
    /// Make an empty store.
    pub fn new() -> Self {
        Store { items: Vec::new() }
    }

    pub fn get(&self, k: &str) -> Option<&String> {
        self.find(k)
    }

    fn find(&self, k: &str) -> Option<&String> {
        self.items.iter().find(|x| x.as_str() == k)
    }
}

impl Backend for Store {
    fn load(&self) -> usize {
        self.items.len()
    }
}

/// The crate's own result, as many crates define it.
pub type Result<T> = std::result::Result<T, Kind>;

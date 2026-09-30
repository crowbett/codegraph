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

/// A one-letter type, as serde_json's `enum N`.
enum N {
    Pos(u64),
    Named { v: u64 },
}

fn classify<T: Into<u64>>(t: T) -> N {
    use N::*;
    let n = Pos(t.into());
    match n {
        N::Pos(v) => N::Named { v },
        N::Named { v } => Named { v },
    }
}

/// A unit struct and a constant, used as values.
pub struct NoProgress;
pub const LIMIT: usize = 3;

fn values() -> usize {
    let _ = (&NoProgress, LIMIT);
    let f = values;
    vec![NoProgress, NoProgress].len() + f()
}

#[cfg(test)]
mod tests {
    use super::classify;
    use super::*;

    fn t() {
        classify(1u8);
        self::helper();
    }

    fn helper() {}
}

mod tok {
    pub enum Token {
        Ident,
    }
}

fn glob_variant(t: tok::Token) -> u8 {
    use tok::Token::*;
    let N = 3;
    match t {
        Ident => N,
    }
}

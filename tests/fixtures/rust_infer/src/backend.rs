pub trait Backend {
    fn list(&self) -> Vec<String>;
    fn name(&self) -> String {
        String::from("backend")
    }
}

pub struct FsBackend {
    root: String,
}

impl FsBackend {
    pub fn new(root: &str) -> Self {
        FsBackend { root: root.to_string() }
    }
}

impl Backend for FsBackend {
    fn list(&self) -> Vec<String> {
        vec![self.root.clone()]
    }
}

#[derive(Default)]
pub struct MemBackend {
    items: Vec<String>,
}

impl Backend for MemBackend {
    fn list(&self) -> Vec<String> {
        self.items.clone()
    }
}

use crate::backend::Backend;

pub struct Repo {
    backend: Box<dyn Backend>,
    title: String,
}

impl Repo {
    pub fn open(backend: Box<dyn Backend>) -> Result<Self, String> {
        Ok(Repo { backend, title: String::new() })
    }

    pub fn count(&self) -> usize {
        self.backend.list().len()
    }

    pub fn backend(&self) -> &dyn Backend {
        self.backend.as_ref()
    }

    pub fn name(&self) -> &str {
        &self.title
    }
}

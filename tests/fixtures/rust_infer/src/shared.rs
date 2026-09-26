use std::sync::Arc;

use crate::repo::Repo;

pub fn shared(r: Repo) -> usize {
    let repo = Arc::new(r);
    assert!(repo.backend().list().is_empty());
    repo.count()
}

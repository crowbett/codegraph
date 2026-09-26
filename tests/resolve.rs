//! Resolution beyond names: macro arguments, typed receivers, trait dispatch, implicit
//! interfaces. One fixture per language under tests/fixtures/<lang>_infer.
#![allow(clippy::unwrap_used, missing_docs)]

mod common;

use common::{edge, index_fixture};

fn assert_edge(ix: &common::Indexed, file: &str, line: u32, name: &str, qname: &str, conf: &str) {
    let got = edge(&ix.store, file, line, name);
    assert_eq!(
        got,
        Some((qname.to_string(), conf.to_string())),
        "{file}:{line} {name}"
    );
}

#[test]
fn rust_calls_inside_macro_arguments() {
    let ix = index_fixture("rust_infer");
    // assert_eq!(short(n as u32), "0") / vec![short(1), short(2)]
    assert_edge(&ix, "src/lib.rs", 15, "short", "short", "exact");
    assert_edge(&ix, "src/lib.rs", 16, "short", "short", "exact");
}

#[test]
fn rust_typed_receivers_and_trait_dispatch() {
    let ix = index_fixture("rust_infer");
    let lib = "src/lib.rs";
    // `let repo = Repo::open(..)?` -> Result<Self, _> unwrapped -> Repo.
    assert_edge(&ix, lib, 14, "count", "repo::Repo::count", "exact");
    // Repo::name, not the trait default Backend::name.
    assert_edge(&ix, lib, 17, "name", "repo::Repo::name", "exact");
    // Concrete type from `Type::default()` -> its impl of the trait method.
    assert_edge(&ix, lib, 19, "list", "backend::MemBackend::list", "exact");
    // `&dyn Backend`, a method returning `&dyn Backend`, `B: Backend`, `impl Backend`.
    assert_edge(&ix, lib, 21, "list", "backend::Backend::list", "exact");
    assert_edge(&ix, lib, 22, "list", "backend::Backend::list", "exact");
    assert_edge(&ix, lib, 26, "list", "backend::Backend::list", "exact");
    assert_edge(&ix, lib, 30, "list", "backend::Backend::list", "exact");
    // `self.backend.list()` through a `Box<dyn Backend>` field.
    assert_edge(
        &ix,
        "src/repo.rs",
        14,
        "list",
        "backend::Backend::list",
        "exact",
    );
    // `len()` on a Vec<String> is std: resolves to nothing rather than a guess.
    assert_eq!(edge(&ix.store, lib, 26, "len"), None);
}

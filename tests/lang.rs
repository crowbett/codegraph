//! Per-language fixture snapshots plus explicit resolution assertions.
#![allow(clippy::unwrap_used, missing_docs)]

mod common;

use common::{dump, edge, index_fixture};

fn snap(name: &str) -> common::Indexed {
    let ix = index_fixture(name);
    insta::assert_snapshot!(name, dump(&ix.store));
    ix
}

#[test]
fn go_fixture() {
    snap("go");
}

#[test]
fn rust_fixture() {
    snap("rust");
}

#[test]
fn typescript_fixture() {
    snap("typescript");
}

#[test]
fn javascript_fixture() {
    snap("javascript");
}

#[test]
fn python_fixture() {
    snap("python");
}

#[test]
fn c_fixture() {
    snap("c");
}

#[test]
fn cpp_fixture() {
    snap("cpp");
}

fn assert_edge(ix: &common::Indexed, file: &str, line: u32, name: &str, qname: &str, conf: &str) {
    let got = edge(&ix.store, file, line, name);
    assert_eq!(
        got,
        Some((qname.to_string(), conf.to_string())),
        "{file}:{line} {name}"
    );
}

#[test]
fn go_same_name_in_two_packages_and_receivers() {
    let ix = index_fixture("go");
    // a.Helper vs bb.Helper (aliased import) resolve to their own packages.
    let rows: Vec<(String, String)> = {
        let mut st = ix
            .store
            .conn()
            .prepare(
                "SELECT e.qualifier, s.qname FROM edges e JOIN symbols s ON s.id = e.to_symbol
                 JOIN files f ON f.id = e.file_id
                 WHERE f.path = 'main.go' AND e.name = 'Helper' ORDER BY e.col",
            )
            .unwrap();
        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    assert_eq!(
        rows,
        [
            ("a".to_string(), "a.Helper".to_string()),
            ("bb".to_string(), "b.Helper".to_string())
        ]
    );
    // Receiver method: `s.lookup` inside a method with receiver `s` is exact.
    assert_edge(&ix, "a/a.go", 16, "lookup", "a.Store.lookup", "exact");
    // Parameter typed `*a.Store`: the receiver's type is known, so the call is exact.
    assert_edge(&ix, "b/b.go", 10, "Get", "a.Store.Get", "exact");
    // Function passed as a value (handler registration).
    assert_edge(&ix, "main.go", 14, "handle", "main.handle", "exact");
    // Stdlib calls stay unresolved rather than guessing.
    assert_eq!(edge(&ix.store, "main.go", 13, "Println"), None);
}

#[test]
fn rust_impl_blocks_traits_and_modules() {
    let ix = index_fixture("rust");
    // `Store::new` (type-qualified, type imported by `use`) vs free fn `util::new`.
    assert_edge(&ix, "src/main.rs", 8, "new", "store::Store::new", "exact");
    assert_edge(&ix, "src/main.rs", 11, "new", "util::new", "exact");
    assert_edge(&ix, "src/main.rs", 10, "helper", "util::helper", "exact");
    assert_edge(
        &ix,
        "src/store.rs",
        27,
        "find",
        "store::Store::find",
        "exact",
    );
    // Types in path-qualifier position are references: `Store::new()`, `store::Kind::Missing`,
    // and the same inside a macro's token tree.
    assert_edge(&ix, "src/main.rs", 8, "Store", "store::Store", "exact");
    // `use store::{Backend, Store};` references both items.
    assert_edge(&ix, "src/main.rs", 5, "Store", "store::Store", "exact");
    assert_edge(&ix, "src/main.rs", 5, "Backend", "store::Backend", "exact");
    assert_edge(&ix, "src/main.rs", 13, "Kind", "store::Kind", "exact");
    assert_edge(&ix, "src/main.rs", 14, "Kind", "store::Kind", "exact");
    // `use std::fmt;` in src/fmt.rs: `fmt::Error` is std's, not this crate's `fmt::Error`.
    let fmt_error: Vec<Option<i64>> = {
        let mut st = ix
            .store
            .conn()
            .prepare(
                "SELECT e.to_symbol FROM edges e JOIN files f ON f.id = e.file_id
                 WHERE f.path = 'src/fmt.rs' AND e.name = 'Error' AND e.line = 7",
            )
            .unwrap();
        st.query_map([], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    assert_eq!(fmt_error, [None]);
    // `impl Backend for Store` links the struct to the trait.
    let (from, to): (String, String) = ix
        .store
        .conn()
        .query_row(
            "SELECT fs.qname, ts.qname FROM edges e JOIN symbols fs ON fs.id = e.from_symbol
             JOIN symbols ts ON ts.id = e.to_symbol WHERE e.kind = 'impl'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        (from.as_str(), to.as_str()),
        ("store::Store", "store::Backend")
    );
}

#[test]
fn typescript_default_vs_named_exports() {
    let ix = index_fixture("typescript");
    // Names inside import statements reference what they import (default, named, aliased).
    assert_edge(&ix, "src/index.ts", 1, "add", "math.add", "exact");
    assert_edge(&ix, "src/index.ts", 1, "helper", "math.helper", "exact");
    assert_edge(
        &ix,
        "src/index.ts",
        3,
        "Greeter",
        "strings.Greeter",
        "exact",
    );
    assert_edge(&ix, "src/index.ts", 11, "add", "math.add", "exact");
    assert_edge(&ix, "src/index.ts", 9, "helper", "math.helper", "exact");
    // Namespace import through a tsconfig `paths` alias.
    assert_edge(&ix, "src/index.ts", 10, "helper", "strings.helper", "exact");
    // `import { Greeter as G }` ... `extends G`.
    assert_edge(&ix, "src/index.ts", 5, "G", "strings.Greeter", "exact");
    assert_edge(
        &ix,
        "src/lib/strings.ts",
        12,
        "format",
        "strings.Greeter.format",
        "exact",
    );
}

#[test]
fn javascript_default_import_and_require() {
    let ix = index_fixture("javascript");
    assert_edge(&ix, "src/main.js", 1, "Widget", "widget.Widget", "exact");
    assert_edge(&ix, "src/main.js", 5, "Widget", "widget.Widget", "exact");
    assert_edge(&ix, "src/main.js", 7, "slugify", "util.slugify", "exact");
    assert_edge(
        &ix,
        "src/main.js",
        6,
        "render",
        "widget.Widget.render",
        "exact",
    );
}

#[test]
fn python_relative_imports_and_inheritance() {
    let ix = index_fixture("python");
    // `from ..util import helper` / `from ..models import User as U` reference the items;
    // `from .. import models` binds a module, which is no symbol edge.
    assert_edge(
        &ix,
        "pkg/sub/tasks.py",
        1,
        "helper",
        "pkg.util.helper",
        "exact",
    );
    assert_edge(
        &ix,
        "pkg/sub/tasks.py",
        3,
        "User",
        "pkg.models.User",
        "exact",
    );
    assert_eq!(edge(&ix.store, "pkg/sub/tasks.py", 2, "models"), None);
    // Names used as values and in union annotations; locals are not symbols.
    let tasks = "pkg/sub/tasks.py";
    assert_edge(&ix, tasks, 15, "U", "pkg.models.User", "exact");
    assert_edge(&ix, tasks, 16, "run", "pkg.sub.tasks.run", "exact");
    assert_edge(&ix, tasks, 16, "U", "pkg.models.User", "exact");
    assert_edge(&ix, tasks, 17, "helper", "pkg.util.helper", "exact");
    assert_eq!(edge(&ix.store, tasks, 19, "local"), None);
    assert_edge(
        &ix,
        "pkg/sub/tasks.py",
        9,
        "helper",
        "pkg.util.helper",
        "exact",
    );
    assert_edge(&ix, "pkg/sub/tasks.py", 10, "U", "pkg.models.User", "exact");
    assert_edge(
        &ix,
        "pkg/sub/tasks.py",
        11,
        "User",
        "pkg.models.User",
        "exact",
    );
    // `self.save()` in User resolves to the inherited Base.save.
    assert_edge(
        &ix,
        "pkg/models.py",
        18,
        "save",
        "pkg.models.Base.save",
        "likely",
    );
    assert_edge(&ix, "main.py", 6, "helper", "pkg.util.helper", "exact");
}

#[test]
fn c_include_graph() {
    let ix = index_fixture("c");
    assert_edge(&ix, "src/main.c", 6, "util_add", "util_add", "likely");
    assert_edge(&ix, "src/main.c", 5, "point", "point", "likely");
    assert_edge(&ix, "src/util.c", 6, "util_add", "util_add", "exact");
}

#[test]
fn cpp_namespaces_and_classes() {
    let ix = index_fixture("cpp");
    assert_edge(&ix, "src/main.cpp", 6, "total", "geo::total", "exact");
    assert_edge(&ix, "src/main.cpp", 4, "Circle", "geo::Circle", "exact");
    assert_edge(
        &ix,
        "src/shapes.cpp",
        9,
        "area",
        "geo::Shape::area",
        "likely",
    );
    assert_edge(
        &ix,
        "include/shapes.hpp",
        11,
        "Shape",
        "geo::Shape",
        "exact",
    );
}

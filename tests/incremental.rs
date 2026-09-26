//! Incremental re-indexing: only changed files are re-parsed; deletions drop symbols;
//! cross-file edges are re-resolved.
#![allow(clippy::unwrap_used, missing_docs)]

mod common;

use codegraph::index::{self, IndexOptions};
use codegraph::store::Store;
use common::{copy_fixture, edge, test_config};

fn reindex(store: &mut Store, root: &std::path::Path, full: bool) -> index::IndexStats {
    index::index_roots(
        store,
        &test_config(),
        &[root.to_path_buf()],
        &IndexOptions { full, budget: None },
    )
    .unwrap()
}

fn count(store: &Store, sql: &str) -> i64 {
    store.conn().query_row(sql, [], |r| r.get(0)).unwrap()
}

#[test]
fn edit_touch_delete_and_full() {
    let src = copy_fixture("go");
    let db = tempfile::tempdir().unwrap();
    let mut store = Store::open(&db.path().join("g.db")).unwrap();
    let root = src.path();

    let first = reindex(&mut store, root, false);
    assert_eq!((first.files, first.parsed), (3, 3));

    let again = reindex(&mut store, root, false);
    assert_eq!((again.parsed, again.unchanged, again.removed), (0, 3, 0));

    // Edit one file: only it is re-parsed, and callers elsewhere still resolve.
    let b = root.join("b/b.go");
    let mut text = std::fs::read_to_string(&b).unwrap();
    text.push_str("\n// Extra is new.\nfunc Extra() { Helper() }\n");
    std::fs::write(&b, &text).unwrap();
    let edited = reindex(&mut store, root, false);
    assert_eq!((edited.parsed, edited.unchanged), (1, 2));
    assert_eq!(
        count(
            &store,
            "SELECT count(*) FROM symbols WHERE qname = 'b.Extra'"
        ),
        1
    );
    assert_eq!(
        edge(&store, "main.go", 13, "Use"),
        Some(("b.Use".to_string(), "exact".to_string()))
    );

    // Rewrite identical content with a new mtime: hash matches, nothing re-parsed.
    std::thread::sleep(std::time::Duration::from_millis(20));
    std::fs::write(&b, &text).unwrap();
    let touched = reindex(&mut store, root, false);
    assert_eq!((touched.parsed, touched.unchanged), (0, 3));

    // Delete a file: its symbols disappear and edges into it become unresolved.
    std::fs::remove_file(root.join("a/a.go")).unwrap();
    let deleted = reindex(&mut store, root, false);
    assert_eq!((deleted.removed, deleted.parsed), (1, 0));
    assert_eq!(
        count(
            &store,
            "SELECT count(*) FROM symbols WHERE qname LIKE 'a.%'"
        ),
        0
    );
    assert_eq!(edge(&store, "main.go", 12, "NewStore"), None);
    assert_eq!(
        count(
            &store,
            "SELECT count(*) FROM edges WHERE to_symbol IS NOT NULL
             AND to_symbol NOT IN (SELECT id FROM symbols)"
        ),
        0,
        "no dangling edges"
    );

    // --full re-parses everything that is left.
    let full = reindex(&mut store, root, true);
    assert_eq!(full.parsed, 2);
}

#[test]
fn secret_files_are_never_indexed() {
    let src = copy_fixture("python");
    std::fs::write(src.path().join("secrets.py"), "API_KEY = 'x'\n").unwrap();
    std::fs::write(src.path().join(".env.py"), "TOKEN = 'y'\n").unwrap();
    let db = tempfile::tempdir().unwrap();
    let mut store = Store::open(&db.path().join("g.db")).unwrap();
    reindex(&mut store, src.path(), false);
    assert_eq!(
        count(
            &store,
            "SELECT count(*) FROM files WHERE path IN ('secrets.py', '.env.py')"
        ),
        0
    );
    assert_eq!(
        count(
            &store,
            "SELECT count(*) FROM symbols WHERE name = 'API_KEY'"
        ),
        0
    );
}

#[test]
fn budget_leaves_work_pending() {
    let src = copy_fixture("python");
    let db = tempfile::tempdir().unwrap();
    let mut store = Store::open(&db.path().join("g.db")).unwrap();
    let st = index::index_roots(
        &mut store,
        &test_config(),
        &[src.path().to_path_buf()],
        &IndexOptions {
            full: false,
            budget: Some(std::time::Duration::ZERO),
        },
    )
    .unwrap();
    assert_eq!(st.parsed, 0);
    assert!(st.pending > 0);
    let st = reindex(&mut store, src.path(), false);
    assert_eq!(st.parsed, st.files);
}

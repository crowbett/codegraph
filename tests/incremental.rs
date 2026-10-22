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

/// The per-repo counts stored by each resolve (used by the index summary and `status`
/// instead of scanning edges) match a real scan.
#[track_caller]
fn assert_counts(store: &Store, st: &index::IndexStats) {
    let scan = |sql| count(store, sql);
    assert_eq!(st.symbols, scan("SELECT count(*) FROM symbols"));
    assert_eq!(
        st.edges,
        scan("SELECT count(*) FROM edges WHERE kind != 'import'")
    );
    assert_eq!(
        st.resolved_edges,
        scan("SELECT count(*) FROM edges WHERE kind != 'import' AND to_symbol IS NOT NULL")
    );
    let status = codegraph::query::status(store, &codegraph::query::Filter::default()).unwrap();
    for c in ["exact", "likely", "name_only"] {
        let real = store
            .conn()
            .query_row(
                "SELECT count(*) FROM edges WHERE confidence = ?1",
                [c],
                |r| r.get::<_, i64>(0),
            )
            .unwrap();
        assert_eq!(
            status.by_confidence.get(c).copied().unwrap_or(0),
            real,
            "{c}"
        );
    }
}

#[test]
fn edit_touch_delete_and_full() {
    let src = copy_fixture("go");
    let db = tempfile::tempdir().unwrap();
    let mut store = Store::open(&db.path().join("g.db")).unwrap();
    let root = src.path();

    let first = reindex(&mut store, root, false);
    assert_eq!((first.files, first.parsed), (3, 3));
    assert_counts(&store, &first);
    // A cold index drops every secondary index during the load; all are back after it.
    assert_eq!(
        count(
            &store,
            "SELECT count(*) FROM sqlite_master WHERE type = 'index' AND name IN (
                'symbols_file', 'edges_file', 'symbols_name',
                'symbols_name_nocase', 'symbols_qkey', 'edges_to', 'edges_from', 'edges_to_file')"
        ),
        8
    );

    let again = reindex(&mut store, root, false);
    assert_eq!((again.parsed, again.unchanged, again.removed), (0, 3, 0));

    // Edit one file: only it is re-parsed, and callers elsewhere still resolve.
    let b = root.join("b/b.go");
    let mut text = std::fs::read_to_string(&b).unwrap();
    text.push_str("\n// Extra is new.\nfunc Extra() { Helper() }\n");
    std::fs::write(&b, &text).unwrap();
    let edited = reindex(&mut store, root, false);
    assert_eq!((edited.parsed, edited.unchanged), (1, 2));
    assert_counts(&store, &edited);
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
    assert_counts(&store, &deleted);
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
    assert_counts(&store, &full);
}

/// A run that dies between its write commit and its resolve (Ctrl-C, a killed MCP
/// server) leaves `indexed_at = 0`; the next incremental run must repair the repo on
/// its own instead of waiting for `--full`.
#[test]
fn interrupted_runs_are_repaired_by_the_next_run() {
    let src = copy_fixture("go");
    let db = tempfile::tempdir().unwrap();
    let mut store = Store::open(&db.path().join("g.db")).unwrap();
    let root = src.path();
    let first = reindex(&mut store, root, false);
    assert_counts(&store, &first);
    let edges = count(&store, "SELECT count(*) FROM edges");
    assert!(edges > 0);

    // A cold load that died before its edge insert: files and symbols, no edges.
    store
        .conn()
        .execute_batch("DELETE FROM edges; UPDATE repos SET indexed_at = 0")
        .unwrap();
    let repaired = reindex(&mut store, root, false);
    assert_eq!((repaired.parsed, repaired.unchanged), (3, 0));
    assert_eq!(count(&store, "SELECT count(*) FROM edges"), edges);
    assert_counts(&store, &repaired);
    assert!(store.repos().unwrap()[0].indexed_at > 0);

    // An incremental run that died before its resolve: edges stored but unresolved.
    store
        .conn()
        .execute_batch(
            "UPDATE edges SET to_symbol = NULL, confidence = NULL WHERE kind != 'import';
             UPDATE repos SET indexed_at = 0",
        )
        .unwrap();
    let repaired = reindex(&mut store, root, false);
    assert_eq!((repaired.parsed, repaired.unchanged), (0, 3));
    assert_eq!(repaired.resolved_edges, first.resolved_edges);
    assert_counts(&store, &repaired);
    assert_eq!(
        edge(&store, "main.go", 13, "Use"),
        Some(("b.Use".to_string(), "exact".to_string()))
    );

    // A finished repo is left alone.
    let again = reindex(&mut store, root, false);
    assert!(again.changed_repos.is_empty(), "{again:?}");
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

/// Every edge as (file, line, col, name, kind, target qname, target file, confidence,
/// enclosing symbol), independent of row ids.
fn edge_dump(store: &Store) -> Vec<String> {
    let mut st = store
        .conn()
        .prepare(
            "SELECT f.path, e.line, e.col, e.name, e.kind, coalesce(t.qname, ''),
                coalesce(tf.path, ''), coalesce(e.confidence, ''), coalesce(fs.qname, '')
             FROM edges e JOIN files f ON f.id = e.file_id
             LEFT JOIN symbols t ON t.id = e.to_symbol
             LEFT JOIN files tf ON tf.id = e.to_file
             LEFT JOIN symbols fs ON fs.id = e.from_symbol",
        )
        .unwrap();
    let mut rows: Vec<String> = st
        .query_map([], |r| {
            Ok(format!(
                "{}:{}:{} {} {} -> {} {} {} from {}",
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, String>(7)?,
                r.get::<_, String>(8)?,
            ))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect();
    rows.sort();
    rows
}

/// A cold index resolves references in memory and inserts each edge once; a re-index of
/// a stored repo updates stored edges. Both must produce the same graph.
#[test]
fn cold_and_stored_paths_agree() {
    for fixture in [
        "go",
        "go_infer",
        "rust",
        "rust_infer",
        "ts_infer",
        "python",
        "c",
    ] {
        let src = copy_fixture(fixture);
        let db = tempfile::tempdir().unwrap();
        let mut store = Store::open(&db.path().join("g.db")).unwrap();
        reindex(&mut store, src.path(), false);
        let cold = edge_dump(&store);
        assert!(!cold.is_empty(), "{fixture}");
        let full = reindex(&mut store, src.path(), true);
        assert_counts(&store, &full);
        assert_eq!(cold, edge_dump(&store), "{fixture}");
    }
}

/// Refresh through a resolve cache that keeps every repo (whatever its size).
fn refresh_cached(
    store: &mut Store,
    root: &std::path::Path,
    cache: &mut codegraph::resolve::ResolveCache,
) -> index::IndexStats {
    let name = store.repos().unwrap()[0].name.clone();
    index::refresh_repo_cached(
        store,
        &test_config(),
        root,
        &name,
        &IndexOptions {
            full: false,
            budget: None,
        },
        cache,
    )
    .unwrap()
}

/// (fixture, file, edits applied in turn: each replaces `from` with `to`; an empty
/// `from` appends).
type Case = (
    &'static str,
    &'static str,
    &'static [(&'static str, &'static str)],
);

/// `watch` and the MCP server keep a repo's resolution state between refreshes and only
/// splice in the edited files. After every edit the graph must equal a whole-repo
/// resolve of the same store.
#[test]
fn cached_resolve_matches_a_full_resolve() {
    let cases: &[Case] = &[
        (
            "go",
            "b/b.go",
            &[
                ("// Helper shares", "// Helper (edited) shares"),
                ("func Helper() string", "func Extra() string { return Helper() }\n\nfunc Helper() string"),
                ("func Helper() string { return \"b\" }", "func Helper2() string { return \"b\" }"),
                ("func Helper2() string", "func Helper() string"),
            ],
        ),
        (
            "rust_infer",
            "src/repo.rs",
            &[
                ("pub fn count(&self)", "pub fn counted(&self)"),
                ("pub fn counted(&self)", "pub fn count(&self)"),
                ("pub fn name(&self) -> &str {", "pub fn extra(&self) -> usize {\n        self.count()\n    }\n\n    pub fn name(&self) -> &str {"),
            ],
        ),
        (
            "ts_infer",
            "packages/app/src/ann.ts",
            &[
                ("export function annFn() {}", "export function annFn2() {}"),
                ("export function annFn2() {}", "export function annFn() {}\nexport function more() { annFn() }"),
            ],
        ),
        (
            "python",
            "pkg/models.py",
            &[
                ("", "\n\ndef extra_fn():\n    return None\n"),
                ("", "\n\ndef more_fn():\n    return extra_fn()\n"),
            ],
        ),
    ];
    for (fixture, file, edits) in cases {
        let src = copy_fixture(fixture);
        let root = src.path();
        let db = tempfile::tempdir().unwrap();
        let mut store = Store::open(&db.path().join("g.db")).unwrap();
        reindex(&mut store, root, false);
        let mut cache = codegraph::resolve::ResolveCache::with_min_edges(0);
        let path = root.join(file);
        assert!(path.is_file(), "{fixture}: {file}");
        for (i, (from, to)) in edits.iter().enumerate() {
            let text = std::fs::read_to_string(&path).unwrap();
            let edited = if from.is_empty() {
                format!("{text}{to}")
            } else {
                assert!(
                    text.contains(from),
                    "{fixture} edit {i}: `{from}` not found"
                );
                text.replacen(from, to, 1)
            };
            std::thread::sleep(std::time::Duration::from_millis(20));
            std::fs::write(&path, edited).unwrap();
            let st = refresh_cached(&mut store, root, &mut cache);
            assert_eq!(st.parsed, 1, "{fixture} edit {i}");
            let cached = edge_dump(&store);
            let repo_id = store.repos().unwrap()[0].id;
            codegraph::resolve::resolve_repo(&mut store, repo_id, root).unwrap();
            assert_eq!(cached, edge_dump(&store), "{fixture} edit {i}");
        }
        // The first edit loads the repo; the rest are served from memory.
        assert_eq!(cache.hits(), edits.len() as u64 - 1, "{fixture}");
    }
}

/// Another connection writing the graph (a CLI `index` next to a running `watch`) makes
/// the kept state stale: the next refresh must not use it.
#[test]
fn cached_resolve_ignores_state_another_writer_made_stale() {
    let src = copy_fixture("go");
    let root = src.path();
    let db = tempfile::tempdir().unwrap();
    let db_path = db.path().join("g.db");
    let mut store = Store::open(&db_path).unwrap();
    reindex(&mut store, root, false);
    let mut cache = codegraph::resolve::ResolveCache::with_min_edges(0);
    let b = root.join("b/b.go");
    let edit = |from: &str, to: &str| {
        let text = std::fs::read_to_string(&b).unwrap();
        assert!(text.contains(from));
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&b, text.replacen(from, to, 1)).unwrap();
    };
    edit("// Helper shares", "// Helper (1) shares");
    refresh_cached(&mut store, root, &mut cache); // loads and keeps the repo

    // Someone else re-indexes an edit through their own connection.
    edit("func Helper() string", "func Helper9() string");
    let mut other = Store::open(&db_path).unwrap();
    reindex(&mut other, root, false);
    drop(other);

    edit("// Helper (1) shares", "// Helper (2) shares");
    refresh_cached(&mut store, root, &mut cache);
    assert_eq!(cache.hits(), 0, "stale state must not be used");
    let cached = edge_dump(&store);
    let repo_id = store.repos().unwrap()[0].id;
    codegraph::resolve::resolve_repo(&mut store, repo_id, root).unwrap();
    assert_eq!(cached, edge_dump(&store));
}

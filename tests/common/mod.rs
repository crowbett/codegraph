//! Shared helpers for integration tests: index a fixture into a temp database
//! and dump the graph as stable text for snapshots.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used, missing_docs)]

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use codegraph::config::Config;
use codegraph::index::{self, IndexOptions, IndexStats};
use codegraph::store::Store;

pub fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

pub struct Indexed {
    pub dir: tempfile::TempDir,
    pub store: Store,
    pub root: PathBuf,
    pub stats: IndexStats,
}

pub fn test_config() -> Config {
    Config {
        roots: Vec::new(),
        ..Config::default()
    }
}

/// Index `root` (a directory) into a fresh temp database.
pub fn index_dir(root: &Path) -> Indexed {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(&dir.path().join("graph.db")).unwrap();
    let stats = index::index_roots(
        &mut store,
        &test_config(),
        &[root.to_path_buf()],
        &IndexOptions::default(),
    )
    .unwrap();
    Indexed {
        dir,
        store,
        root: root.to_path_buf(),
        stats,
    }
}

pub fn index_fixture(name: &str) -> Indexed {
    index_dir(&fixture(name))
}

/// Copy a fixture into a temp dir (for tests that edit files).
pub fn copy_fixture(name: &str) -> tempfile::TempDir {
    let dst = tempfile::tempdir().unwrap();
    copy_dir(&fixture(name), dst.path());
    dst
}

fn copy_dir(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for e in std::fs::read_dir(src).unwrap() {
        let e = e.unwrap();
        let to = dst.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &to);
        } else {
            std::fs::copy(e.path(), &to).unwrap();
        }
    }
}

/// Stable textual dump: symbols, then resolved and unresolved edges.
pub fn dump(store: &Store) -> String {
    let conn = store.conn();
    let mut out = String::from("symbols:\n");
    let mut st = conn
        .prepare(
            "SELECT f.path, s.start_line, s.end_line, s.kind, s.qname, s.exported, s.is_default,
                    coalesce(p.qname, ''), coalesce(s.doc, '')
             FROM symbols s JOIN files f ON f.id = s.file_id LEFT JOIN symbols p ON p.id = s.parent_id
             ORDER BY f.path, s.start_line, s.start_col, s.qname",
        )
        .unwrap();
    let rows = st
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, u32>(1)?,
                r.get::<_, u32>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, bool>(5)?,
                r.get::<_, bool>(6)?,
                r.get::<_, String>(7)?,
                r.get::<_, String>(8)?,
            ))
        })
        .unwrap();
    for r in rows {
        let (path, l, e, kind, qname, exported, default, parent, doc) = r.unwrap();
        let mut flags = Vec::new();
        if exported {
            flags.push("pub".to_string());
        }
        if default {
            flags.push("default".to_string());
        }
        if !parent.is_empty() {
            flags.push(format!("in {parent}"));
        }
        if !doc.is_empty() {
            flags.push(format!("doc={doc:?}"));
        }
        let _ = writeln!(
            out,
            "  {path}:{l}-{e} {kind} {qname}{}",
            if flags.is_empty() {
                String::new()
            } else {
                format!("  [{}]", flags.join(", "))
            }
        );
    }
    out.push_str("edges:\n");
    let mut st = conn
        .prepare(
            "SELECT f.path, e.line, e.kind, coalesce(fs.qname, '<file>'), coalesce(e.qualifier, ''),
                    e.name, ts.qname, e.confidence, tf.path
             FROM edges e JOIN files f ON f.id = e.file_id
             LEFT JOIN symbols fs ON fs.id = e.from_symbol
             LEFT JOIN symbols ts ON ts.id = e.to_symbol
             LEFT JOIN files tf ON tf.id = e.to_file
             ORDER BY f.path, e.line, e.col, e.kind, e.name",
        )
        .unwrap();
    let rows = st
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, u32>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, Option<String>>(6)?,
                r.get::<_, Option<String>>(7)?,
                r.get::<_, Option<String>>(8)?,
            ))
        })
        .unwrap();
    for r in rows {
        let (path, line, kind, from, qual, name, to, conf, to_file) = r.unwrap();
        let target = if kind == "import" {
            match to_file {
                Some(f) => format!("{name} => {f}"),
                None => format!("{name} => (external)"),
            }
        } else {
            let written = if qual.is_empty() {
                name
            } else {
                format!("{qual}.{name}")
            };
            match (to, conf) {
                (Some(t), Some(c)) => format!("{written} => {t} ({c})"),
                _ => format!("{written} => ?"),
            }
        };
        let _ = writeln!(out, "  {path}:{line} {kind} {from}: {target}");
    }
    out
}

/// The resolved target qname and confidence of the first edge matching (file suffix, line, name).
pub fn edge(store: &Store, file: &str, line: u32, name: &str) -> Option<(String, String)> {
    store
        .conn()
        .query_row(
            "SELECT ts.qname, e.confidence FROM edges e JOIN files f ON f.id = e.file_id
             JOIN symbols ts ON ts.id = e.to_symbol
             WHERE f.path = ?1 AND e.line = ?2 AND e.name = ?3 LIMIT 1",
            rusqlite::params![file, line, name],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok()
}

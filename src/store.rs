//! SQLite storage for the graph: repos, files, symbols, edges (+ FTS5 over symbols).
//!
//! Edges are written unresolved by the indexer (name + qualifier) and filled in
//! (`to_symbol`, `confidence`) by [`crate::resolve`], which re-runs per repo whenever
//! any of its files changed.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OptionalExtension};

use crate::error::{Error, Result};
use crate::extract::FileFacts;
use crate::lang::Lang;
use crate::layout::FileUnit;

/// Bump when the schema or extraction output changes incompatibly; a mismatch
/// wipes the graph so the next `index` rebuilds it.
pub const SCHEMA_VERSION: i64 = 19;

const SCHEMA: &str = r"
CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS repos(
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    root TEXT NOT NULL UNIQUE,
    indexed_at INTEGER NOT NULL DEFAULT 0,
    -- Written by each resolve, so status and index summaries never scan edges.
    symbols INTEGER NOT NULL DEFAULT 0,
    edges INTEGER NOT NULL DEFAULT 0,
    resolved INTEGER NOT NULL DEFAULT 0,
    exact INTEGER NOT NULL DEFAULT 0,
    likely INTEGER NOT NULL DEFAULT 0,
    name_only INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS files(
    id INTEGER PRIMARY KEY,
    repo_id INTEGER NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
    path TEXT NOT NULL,
    lang TEXT NOT NULL,
    hash TEXT NOT NULL,
    mtime INTEGER NOT NULL,
    size INTEGER NOT NULL,
    lines INTEGER NOT NULL,
    module TEXT NOT NULL,
    package TEXT NOT NULL,
    pkgname TEXT,
    parse_errors INTEGER NOT NULL DEFAULT 0,
    UNIQUE(repo_id, path)
);
CREATE TABLE IF NOT EXISTS symbols(
    id INTEGER PRIMARY KEY,
    file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    repo_id INTEGER NOT NULL,
    parent_id INTEGER,
    name TEXT NOT NULL,
    kind TEXT NOT NULL,
    qname TEXT NOT NULL,
    qkey TEXT NOT NULL,
    container TEXT,
    start_line INTEGER NOT NULL,
    start_col INTEGER NOT NULL,
    end_line INTEGER NOT NULL,
    end_col INTEGER NOT NULL,
    signature TEXT NOT NULL,
    doc TEXT,
    exported INTEGER NOT NULL,
    is_default INTEGER NOT NULL,
    recv TEXT,
    ret TEXT,
    fields TEXT
);
CREATE TABLE IF NOT EXISTS edges(
    id INTEGER PRIMARY KEY,
    repo_id INTEGER NOT NULL,
    file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    from_symbol INTEGER,
    from_name TEXT,
    name TEXT NOT NULL,
    qualifier TEXT,
    kind TEXT NOT NULL,
    line INTEGER NOT NULL,
    col INTEGER NOT NULL,
    to_symbol INTEGER,
    to_file INTEGER,
    confidence TEXT,
    extra TEXT
);
-- Substring search covers every symbol except C/C++ object-like `#define` constants: the
-- Linux kernel has 2.0 M of them (2/3 of its symbols). They stay symbols (def, refs, exact
-- and prefix find); only substring/fuzzy-doc search skips them.
CREATE VIEW IF NOT EXISTS symbols_fts_src AS
    SELECT id, name, qname, doc FROM symbols
    WHERE NOT (kind = 'const' AND signature LIKE '#define%');
CREATE VIRTUAL TABLE IF NOT EXISTS symbols_fts USING fts5(
    name, qname, doc, content='symbols_fts_src', content_rowid='id', tokenize='trigram'
);
";

/// Secondary indexes and the triggers keeping `symbols_fts` in sync row by row; dropped
/// during bulk loads (the per-file/per-repo ones only when the graph was empty, since a
/// `--full` run still deletes old files by id) and rebuilt once at the end.
const FTS_TRIGGERS: &str = r"
CREATE INDEX IF NOT EXISTS symbols_file ON symbols(file_id);
CREATE INDEX IF NOT EXISTS edges_file ON edges(file_id);
CREATE INDEX IF NOT EXISTS symbols_name ON symbols(name);
CREATE INDEX IF NOT EXISTS symbols_name_nocase ON symbols(name COLLATE NOCASE);
CREATE INDEX IF NOT EXISTS symbols_qkey ON symbols(qkey);
CREATE INDEX IF NOT EXISTS edges_to ON edges(to_symbol);
CREATE INDEX IF NOT EXISTS edges_from ON edges(from_symbol);
-- Only import edges are looked up by target file (who imports this file).
CREATE INDEX IF NOT EXISTS edges_to_file ON edges(to_file) WHERE kind = 'import';
CREATE TRIGGER IF NOT EXISTS symbols_ai AFTER INSERT ON symbols
WHEN NOT (new.kind = 'const' AND new.signature LIKE '#define%') BEGIN
    INSERT INTO symbols_fts(rowid, name, qname, doc) VALUES (new.id, new.name, new.qname, new.doc);
END;
CREATE TRIGGER IF NOT EXISTS symbols_ad AFTER DELETE ON symbols
WHEN NOT (old.kind = 'const' AND old.signature LIKE '#define%') BEGIN
    INSERT INTO symbols_fts(symbols_fts, rowid, name, qname, doc)
    VALUES ('delete', old.id, old.name, old.qname, old.doc);
END;
";

/// Normalized key for qualified-name matching: `::` -> `.`.
pub fn qkey(qname: &str) -> String {
    qname.replace("::", ".")
}

/// A file row as seen by the indexer.
#[derive(Debug, Clone)]
pub struct FileRow {
    /// Row id.
    pub id: i64,
    /// Content hash.
    pub hash: String,
    /// mtime (ns).
    pub mtime: i64,
    /// Size in bytes.
    pub size: i64,
}

/// A repo row.
#[derive(Debug, Clone)]
pub struct RepoRow {
    /// Row id.
    pub id: i64,
    /// Display name.
    pub name: String,
    /// Absolute root.
    pub root: PathBuf,
    /// Last index time (unix seconds).
    pub indexed_at: i64,
    /// Counts from the last resolve: symbols, non-import edges, resolved, exact, likely,
    /// name-only.
    pub counts: [i64; 6],
}

/// A file to (re)insert.
pub struct NewFile<'a> {
    /// Repo-relative path, forward slashes.
    pub path: &'a str,
    /// Language.
    pub lang: Lang,
    /// Content hash.
    pub hash: &'a str,
    /// mtime (ns).
    pub mtime: i64,
    /// Size.
    pub size: i64,
    /// Module-system unit.
    pub unit: &'a FileUnit,
    /// Extraction output.
    pub facts: &'a FileFacts,
}

/// The graph database.
pub struct Store {
    conn: Connection,
    path: PathBuf,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl Store {
    /// Open (creating if needed) the database at `path`.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| Error::io(format!("create {}", dir.display()), e))?;
        }
        let conn = Connection::open(path)?;
        conn.busy_timeout(std::time::Duration::from_secs(10))?;
        // The graph is a derived, rebuildable cache: skip fsyncs (4x faster bulk
        // indexing). An OS crash mid-index can at worst require `index --full`.
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=OFF; PRAGMA foreign_keys=ON;
             PRAGMA temp_store=MEMORY; PRAGMA cache_size=-65536;",
        )?;
        let mut store = Self {
            conn,
            path: path.to_path_buf(),
        };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&mut self) -> Result<()> {
        let has_meta: bool = self.conn.query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='meta'",
            [],
            |r| r.get::<_, i64>(0).map(|n| n > 0),
        )?;
        if has_meta {
            let v: Option<String> = self
                .conn
                .query_row("SELECT value FROM meta WHERE key='schema'", [], |r| {
                    r.get(0)
                })
                .optional()?;
            if v.as_deref() != Some(&SCHEMA_VERSION.to_string()) {
                self.conn.execute_batch(
                    "DROP TABLE IF EXISTS symbols_fts; DROP VIEW IF EXISTS symbols_fts_src;
                     DROP TABLE IF EXISTS edges;
                     DROP TABLE IF EXISTS symbols; DROP TABLE IF EXISTS files;
                     DROP TABLE IF EXISTS repos; DROP TABLE IF EXISTS meta;",
                )?;
            }
        }
        self.conn.execute_batch(SCHEMA)?;
        self.conn.execute(
            "INSERT OR REPLACE INTO meta(key, value) VALUES ('schema', ?1)",
            [SCHEMA_VERSION.to_string()],
        )?;
        // While a bulk load runs (or after one died) the search index is marked dirty and
        // its triggers/indexes are absent on purpose: readers tolerate that, and only
        // writers repair it (`recover_fts`), so opening the graph never blocks on a rebuild.
        if !self.fts_dirty()? {
            self.conn.execute_batch(FTS_TRIGGERS)?;
        }
        Ok(())
    }

    fn fts_dirty(&self) -> Result<bool> {
        Ok(self.fts_dirty_age()?.is_some())
    }

    /// Seconds since a bulk load marked the search index dirty.
    fn fts_dirty_age(&self) -> Result<Option<i64>> {
        Ok(self
            .conn
            .query_row(
                "SELECT unixepoch() - CAST(value AS INTEGER) FROM meta WHERE key = 'fts_dirty'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .optional()?)
    }

    /// Writers call this first: finish the search index a killed bulk load left behind.
    /// `force` (an explicit `index`) repairs at once; background refreshes only when the
    /// mark is older than 15 minutes, so they don't rebuild under a running bulk load.
    pub fn recover_fts(&self, force: bool) -> Result<()> {
        match self.fts_dirty_age()? {
            Some(age) if force || age > 900 => self.resume_fts(),
            _ => Ok(()),
        }
    }

    /// Stop maintaining the symbol search index row by row (bulk loads); marks it dirty
    /// until [`Store::resume_fts`] rebuilds it in one pass (about 5x cheaper).
    /// `cold` (the graph is empty): also drop the per-file/per-repo indexes, so the load
    /// is append-only.
    pub fn suspend_fts(&self, cold: bool) -> Result<()> {
        // One transaction with the dirty mark, so a crash never leaves indexes missing
        // without the mark that makes the next writer rebuild them.
        let base = if cold {
            "DROP INDEX IF EXISTS symbols_file; DROP INDEX IF EXISTS edges_file;"
        } else {
            ""
        };
        self.conn.execute_batch(&format!(
            "BEGIN;
             {base}
             DROP TRIGGER IF EXISTS symbols_ai; DROP TRIGGER IF EXISTS symbols_ad;
             DROP INDEX IF EXISTS edges_to; DROP INDEX IF EXISTS edges_from;
             DROP INDEX IF EXISTS edges_to_file; DROP INDEX IF EXISTS symbols_name;
             DROP INDEX IF EXISTS symbols_name_nocase; DROP INDEX IF EXISTS symbols_qkey;
             INSERT OR REPLACE INTO meta(key, value) VALUES ('fts_dirty', CAST(unixepoch() AS TEXT));
             COMMIT;"
        ))?;
        Ok(())
    }

    /// Rebuild the symbol search index from `symbols` and restore the row triggers.
    pub fn resume_fts(&self) -> Result<()> {
        let t0 = std::time::Instant::now();
        let tx = self.conn.unchecked_transaction()?;
        tx.execute_batch("INSERT INTO symbols_fts(symbols_fts) VALUES ('rebuild');")?;
        crate::util::trace("index.fts_rebuild", t0);
        tx.execute_batch(FTS_TRIGGERS)?;
        tx.execute("DELETE FROM meta WHERE key = 'fts_dirty'", [])?;
        tx.commit()?;
        Ok(())
    }

    /// `PRAGMA data_version`: changes when another connection commits to the graph.
    pub fn data_version(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("PRAGMA data_version", [], |r| r.get(0))?)
    }

    /// Database file path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Raw connection (queries live in [`crate::query`]).
    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    /// Mutable connection (transactions).
    pub fn conn_mut(&mut self) -> &mut Connection {
        &mut self.conn
    }

    /// Insert or rename a repo; returns its id.
    pub fn upsert_repo(&self, name: &str, root: &Path) -> Result<i64> {
        let root_s = root.to_string_lossy();
        self.conn.execute(
            "INSERT INTO repos(name, root) VALUES (?1, ?2)
             ON CONFLICT(root) DO UPDATE SET name = excluded.name",
            params![name, root_s],
        )?;
        Ok(self
            .conn
            .query_row("SELECT id FROM repos WHERE root = ?1", [root_s], |r| {
                r.get(0)
            })?)
    }

    /// All repos.
    pub fn repos(&self) -> Result<Vec<RepoRow>> {
        let mut st = self.conn.prepare_cached(
            "SELECT id, name, root, indexed_at, symbols, edges, resolved, exact, likely,
                    name_only FROM repos ORDER BY name",
        )?;
        let rows = st.query_map([], |r| {
            Ok(RepoRow {
                id: r.get(0)?,
                name: r.get(1)?,
                root: PathBuf::from(r.get::<_, String>(2)?),
                indexed_at: r.get(3)?,
                counts: [
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                    r.get(8)?,
                    r.get(9)?,
                ],
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Delete a repo and everything under it.
    pub fn delete_repo(&self, id: i64) -> Result<()> {
        self.conn.execute("DELETE FROM repos WHERE id = ?1", [id])?;
        Ok(())
    }

    /// Mark a repo as indexed now (its write and its resolve both finished).
    pub fn set_indexed(&self, id: i64, ts: i64) -> Result<()> {
        self.conn
            .execute("UPDATE repos SET indexed_at = ?2 WHERE id = ?1", [id, ts])?;
        Ok(())
    }

    /// When a repo was last indexed: 0 when it never was, or when its last run died
    /// between the write transaction and the resolve (see [`begin_repo_write`]).
    pub fn repo_indexed_at(&self, id: i64) -> Result<i64> {
        Ok(self
            .conn
            .query_row("SELECT indexed_at FROM repos WHERE id = ?1", [id], |r| {
                r.get(0)
            })?)
    }

    /// Whether any edge of the repo is stored.
    pub fn repo_has_edges(&self, id: i64) -> Result<bool> {
        Ok(self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM edges WHERE repo_id = ?1)",
            [id],
            |r| r.get(0),
        )?)
    }

    /// Files of a repo keyed by relative path.
    pub fn repo_files(&self, repo_id: i64) -> Result<HashMap<String, FileRow>> {
        let mut st = self
            .conn
            .prepare_cached("SELECT id, path, hash, mtime, size FROM files WHERE repo_id = ?1")?;
        let rows = st.query_map([repo_id], |r| {
            Ok((
                r.get::<_, String>(1)?,
                FileRow {
                    id: r.get(0)?,
                    hash: r.get(2)?,
                    mtime: r.get(3)?,
                    size: r.get(4)?,
                },
            ))
        })?;
        Ok(rows.collect::<rusqlite::Result<HashMap<_, _>>>()?)
    }
}

/// Start a repo's write transaction: reset `indexed_at` so that, if the process dies
/// before the resolve that follows the commit, the next run sees an unfinished repo
/// ([`Store::repo_indexed_at`] is 0) and repairs it. `wipe_files` drops every stored file
/// (a cold load whose edges never made it): the repo is then loaded cold again.
pub fn begin_repo_write(tx: &Connection, repo_id: i64, wipe_files: bool) -> Result<()> {
    tx.execute("UPDATE repos SET indexed_at = 0 WHERE id = ?1", [repo_id])?;
    if wipe_files {
        tx.execute("DELETE FROM files WHERE repo_id = ?1", [repo_id])?;
    }
    Ok(())
}

/// Update mtime/size of an unchanged-content file.
pub fn touch_file(tx: &Connection, id: i64, mtime: i64, size: i64) -> Result<()> {
    tx.prepare_cached("UPDATE files SET mtime = ?2, size = ?3 WHERE id = ?1")?
        .execute(params![id, mtime, size])?;
    Ok(())
}

/// Delete a file (cascades to its symbols and edges).
pub fn delete_file(tx: &Connection, id: i64) -> Result<()> {
    tx.prepare_cached("DELETE FROM files WHERE id = ?1")?
        .execute([id])?;
    Ok(())
}

/// `edges.extra` of a reference: its receiver type hint and call syntax.
/// `edges.extra` of a reference: its receiver type hint and method-call syntax.
pub(crate) fn ref_extra(hint: Option<&str>, dot: bool, arg: Option<&str>) -> Option<String> {
    if hint.is_none() && !dot && arg.is_none() {
        return None;
    }
    let mut m = serde_json::Map::new();
    if let Some(h) = hint {
        m.insert("rt".into(), h.into());
    }
    if dot {
        m.insert("dot".into(), true.into());
    }
    if let Some(a) = arg {
        m.insert("at".into(), a.into());
    }
    Some(serde_json::Value::Object(m).to_string())
}

/// `edges.extra` of an import: its bindings and flags.
pub(crate) fn import_extra(imp: &crate::extract::RawImport) -> String {
    let mut extra = serde_json::json!({"bindings": imp.bindings, "system": imp.system});
    if imp.mod_path {
        extra["mod_path"] = true.into();
    }
    extra.to_string()
}

/// Replace a file's row, symbols and (unresolved) edges. Returns the file id and its
/// symbols' ids in extraction order. `with_edges: false` leaves the edges to the caller
/// (bulk loads resolve them first, see `resolve::resolve_and_insert`).
pub fn replace_file(
    tx: &Connection,
    repo_id: i64,
    f: &NewFile<'_>,
    with_edges: bool,
) -> Result<(i64, Vec<i64>)> {
    tx.prepare_cached("DELETE FROM files WHERE repo_id = ?1 AND path = ?2")?
        .execute(params![repo_id, f.path])?;
    tx.prepare_cached(
        "INSERT INTO files(repo_id, path, lang, hash, mtime, size, lines, module, package, pkgname, parse_errors)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
    )?
    .execute(params![
        repo_id,
        f.path,
        f.lang.as_str(),
        f.hash,
        f.mtime,
        f.size,
        f.facts.lines,
        f.unit.module,
        f.unit.package,
        f.facts.package,
        f.facts.has_errors,
    ])?;
    let file_id = tx.last_insert_rowid();
    let sep = f.lang.sep();

    let mut ids: Vec<i64> = Vec::with_capacity(f.facts.symbols.len());
    {
        let mut st = tx.prepare_cached(
            "INSERT INTO symbols(file_id, repo_id, parent_id, name, kind, qname, qkey, container,
                start_line, start_col, end_line, end_col, signature, doc, exported, is_default, recv,
                ret, fields)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17,
                ?18, ?19)",
        )?;
        for s in &f.facts.symbols {
            let parent = s.parent.and_then(|p| ids.get(p).copied());
            let qname = if f.unit.prefix.is_empty() {
                s.local_qname.clone()
            } else {
                format!("{}{sep}{}", f.unit.prefix.replace('/', sep), s.local_qname)
            };
            st.execute(params![
                file_id,
                repo_id,
                parent,
                s.name,
                s.kind.as_str(),
                qname,
                qkey(&qname),
                s.container,
                s.start_line,
                s.start_col,
                s.end_line,
                s.end_col,
                s.signature,
                s.doc,
                s.exported,
                s.default_export,
                s.recv,
                s.ret,
                (!s.fields.is_empty())
                    .then(|| serde_json::to_string(&s.fields).unwrap_or_default()),
            ])?;
            ids.push(tx.last_insert_rowid());
        }
    }
    if !with_edges {
        return Ok((file_id, ids));
    }
    let mut st = tx.prepare_cached(
        "INSERT INTO edges(repo_id, file_id, from_symbol, from_name, name, qualifier, kind, line, col, extra)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
    )?;
    for r in &f.facts.refs {
        let from = r.from.and_then(|i| ids.get(i).copied());
        st.execute(params![
            repo_id,
            file_id,
            from,
            r.from_name,
            r.name,
            r.qualifier,
            r.kind.as_str(),
            r.line,
            r.col,
            ref_extra(r.hint.as_deref(), r.dot, r.arg.as_deref()),
        ])?;
    }
    for imp in &f.facts.imports {
        let extra = import_extra(imp);
        st.execute(params![
            repo_id,
            file_id,
            Option::<i64>::None,
            Option::<String>::None,
            imp.path,
            Option::<String>::None,
            "import",
            imp.line,
            1,
            extra,
        ])?;
    }
    Ok((file_id, ids))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_twice_and_version_reset() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("g.db");
        {
            let s = Store::open(&db).unwrap();
            s.upsert_repo("r", Path::new("C:/r")).unwrap();
            s.conn()
                .execute("UPDATE meta SET value='0' WHERE key='schema'", [])
                .unwrap();
        }
        let s = Store::open(&db).unwrap();
        assert!(s.repos().unwrap().is_empty(), "old schema must be wiped");
        let id = s.upsert_repo("r", Path::new("C:/r")).unwrap();
        assert_eq!(s.upsert_repo("r2", Path::new("C:/r")).unwrap(), id);
        assert_eq!(s.repos().unwrap()[0].name, "r2");
    }

    fn fts_hits(s: &Store, q: &str) -> i64 {
        s.conn()
            .query_row(
                "SELECT count(*) FROM symbols_fts WHERE symbols_fts MATCH ?1",
                [q],
                |r| r.get(0),
            )
            .unwrap()
    }

    fn add_symbol(s: &Store, name: &str) {
        s.conn()
            .execute(
                "INSERT INTO symbols(file_id, repo_id, name, kind, qname, qkey, start_line,
                    start_col, end_line, end_col, signature, exported, is_default)
                 VALUES (1, 1, ?1, 'function', ?1, ?1, 1, 1, 1, 1, '', 1, 0)",
                [name],
            )
            .unwrap();
    }

    #[test]
    fn define_constants_skip_substring_search() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("g.db")).unwrap();
        s.conn()
            .execute_batch(
                "INSERT INTO repos(id, name, root) VALUES (1, 'r', 'C:/r');
                 INSERT INTO files(id, repo_id, path, lang, hash, mtime, size, lines,
                    module, package) VALUES (1, 1, 'a.h', 'c', 'h', 0, 0, 1, '', '');
                 INSERT INTO symbols(file_id, repo_id, name, kind, qname, qkey, start_line,
                    start_col, end_line, end_col, signature, exported, is_default)
                 VALUES (1, 1, 'MAX_WIDGETS', 'const', 'MAX_WIDGETS', 'MAX_WIDGETS', 1, 1, 1, 1,
                    '#define MAX_WIDGETS 8', 1, 0),
                        (1, 1, 'widget_init', 'function', 'widget_init', 'widget_init', 2, 1, 2,
                    1, 'int widget_init(void)', 1, 0);",
            )
            .unwrap();
        assert_eq!(
            fts_hits(&s, "AX_WID"),
            0,
            "no #define constants in substring search"
        );
        assert_eq!(fts_hits(&s, "dget_in"), 1);
        // Deleting both keeps the index consistent (the trigger skips the constant too).
        s.conn().execute("DELETE FROM symbols", []).unwrap();
        s.conn()
            .execute(
                "INSERT INTO symbols_fts(symbols_fts) VALUES ('integrity-check')",
                [],
            )
            .unwrap();
        assert_eq!(fts_hits(&s, "dget_in"), 0);
    }

    #[test]
    fn fts_bulk_mode_recovers_after_a_crash() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("g.db");
        {
            let s = Store::open(&db).unwrap();
            s.conn()
                .execute_batch(
                    "INSERT INTO repos(id, name, root) VALUES (1, 'r', 'C:/r');
                     INSERT INTO files(id, repo_id, path, lang, hash, mtime, size, lines,
                        module, package) VALUES (1, 1, 'a.go', 'go', 'h', 0, 0, 1, '', '');",
                )
                .unwrap();
            s.suspend_fts(false).unwrap();
            add_symbol(&s, "NeedleOne");
            assert_eq!(fts_hits(&s, "edleOn"), 0, "suspended: not indexed yet");
            // No resume_fts: simulated crash.
        }
        let s = Store::open(&db).unwrap();
        assert_eq!(fts_hits(&s, "edleOn"), 0, "readers never rebuild");
        s.recover_fts(false).unwrap();
        assert_eq!(
            fts_hits(&s, "edleOn"),
            0,
            "a fresh mark is left to its bulk load"
        );
        s.recover_fts(true).unwrap();
        assert_eq!(fts_hits(&s, "edleOn"), 1, "the next writer rebuilds");
        add_symbol(&s, "HaystackTwo");
        assert_eq!(fts_hits(&s, "ystackTw"), 1, "row triggers restored");
        s.suspend_fts(false).unwrap();
        add_symbol(&s, "ThirdThing");
        s.resume_fts().unwrap();
        assert_eq!(fts_hits(&s, "irdThin"), 1);
    }
}

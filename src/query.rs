//! Read side: find / def / refs / callers / callees / outline / path / impact /
//! hotspots / dot / status. Shared by the CLI and the MCP server.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};

use rusqlite::{params_from_iter, Connection, OptionalExtension, Row};
use serde::Serialize;

use crate::error::{Error, Result};
use crate::extract;
use crate::lang::Lang;
use crate::model::SymKind;
use crate::store::{qkey, Store};
use crate::util;

/// Hard cap on nodes in any tree / graph answer.
pub const MAX_NODES: usize = 400;
/// Cap on children listed under one tree node.
pub const MAX_CHILDREN: usize = 40;

/// A symbol as returned to users.
#[derive(Debug, Clone, Serialize)]
pub struct Sym {
    /// Database id (stable until the file is re-indexed).
    pub id: i64,
    /// Simple name.
    pub name: String,
    /// Kind.
    pub kind: String,
    /// Qualified name.
    pub qname: String,
    /// Repo name.
    pub repo: String,
    /// Absolute file path (forward slashes).
    pub file: String,
    /// 1-based start line.
    pub line: u32,
    /// 1-based end line.
    pub end_line: u32,
    /// First source line.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub signature: String,
    /// First doc line.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
    /// Repo-relative path (text rendering).
    #[serde(skip)]
    pub rel: String,
    /// Repo root (text rendering).
    #[serde(skip)]
    pub root: String,
    #[serde(skip)]
    exported: bool,
}

impl Sym {
    /// `file:line`.
    pub fn loc(&self) -> String {
        format!("{}:{}", self.file, self.line)
    }
    /// `rel:line`.
    pub fn rel_loc(&self) -> String {
        format!("{}:{}", self.rel, self.line)
    }
    fn brief(mut self) -> Self {
        self.signature.clear();
        self.doc = None;
        self
    }
}

/// A throwaway symbol for rendering tests.
#[cfg(test)]
pub(crate) fn test_sym() -> Sym {
    Sym {
        id: 1,
        name: "f".into(),
        kind: "function".into(),
        qname: "m.f".into(),
        repo: "r".into(),
        file: "C:/r/m.go".into(),
        line: 1,
        end_line: 1,
        signature: String::new(),
        doc: None,
        rel: "m.go".into(),
        root: "C:/r".into(),
        exported: false,
    }
}

const SYM_SELECT: &str =
    "SELECT s.id, s.name, s.kind, s.qname, r.name, r.root, f.path, s.start_line,
    s.end_line, s.signature, s.doc, s.file_id, s.parent_id, s.exported
    FROM symbols s JOIN files f ON f.id = s.file_id JOIN repos r ON r.id = s.repo_id";

fn sym_from_row(r: &Row<'_>) -> rusqlite::Result<Sym> {
    let root: String = r.get(5)?;
    let rel: String = r.get(6)?;
    let root_slash = root.replace('\\', "/");
    Ok(Sym {
        id: r.get(0)?,
        name: r.get(1)?,
        kind: r.get(2)?,
        qname: r.get(3)?,
        repo: r.get(4)?,
        file: format!("{}/{}", root_slash.trim_end_matches('/'), rel),
        line: r.get(7)?,
        end_line: r.get(8)?,
        signature: r.get(9)?,
        doc: r.get(10)?,
        rel,
        root: root_slash,
        exported: r.get(13)?,
    })
}

/// Load symbols by id (order preserved, missing ids skipped).
pub fn syms_by_ids(conn: &Connection, ids: &[i64]) -> Result<Vec<Sym>> {
    let mut out = Vec::with_capacity(ids.len());
    let mut st = conn.prepare_cached(&format!("{SYM_SELECT} WHERE s.id = ?1"))?;
    for id in ids {
        if let Some(s) = st.query_row([id], sym_from_row).optional()? {
            out.push(s);
        }
    }
    Ok(out)
}

fn sym_by_id(conn: &Connection, id: i64) -> Result<Option<Sym>> {
    Ok(syms_by_ids(conn, &[id])?.into_iter().next())
}

/// Common filters.
#[derive(Debug, Clone, Default)]
pub struct Filter {
    /// Restrict to these repo ids.
    pub repos: Option<Vec<i64>>,
    /// Restrict to these kinds.
    pub kinds: Option<Vec<SymKind>>,
}

impl Filter {
    fn sql(&self) -> String {
        let mut s = String::new();
        if let Some(r) = &self.repos {
            let ids: Vec<String> = r.iter().map(i64::to_string).collect();
            s.push_str(&format!(" AND s.repo_id IN ({})", ids.join(",")));
        }
        if let Some(k) = &self.kinds {
            let ks: Vec<String> = k.iter().map(|k| format!("'{}'", k.as_str())).collect();
            s.push_str(&format!(" AND s.kind IN ({})", ks.join(",")));
        }
        s
    }
}

/// Resolve `--repo` into repo ids (exact name, trailing path segment, or root suffix).
pub fn repo_filter(store: &Store, spec: Option<&str>) -> Result<Option<Vec<i64>>> {
    let Some(spec) = spec.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    let want = spec.replace('\\', "/").to_lowercase();
    let repos = store.repos()?;
    let exact: Vec<i64> = repos
        .iter()
        .filter(|r| r.name.to_lowercase() == want)
        .map(|r| r.id)
        .collect();
    if !exact.is_empty() {
        return Ok(Some(exact));
    }
    let loose: Vec<i64> = repos
        .iter()
        .filter(|r| {
            let n = r.name.to_lowercase();
            let root = util::slash(&r.root).to_lowercase();
            n.ends_with(&format!("/{want}")) || root.ends_with(&format!("/{want}")) || root == want
        })
        .map(|r| r.id)
        .collect();
    if loose.is_empty() {
        return Err(Error::NotFound(format!(
            "no indexed repo matches `{spec}` (see `codegraph status`)"
        )));
    }
    Ok(Some(loose))
}

// ---------------------------------------------------------------------------
// Target resolution
// ---------------------------------------------------------------------------

/// Split `path:line` if the spec looks like one.
fn split_file_line(spec: &str) -> Option<(&str, u32)> {
    let (p, l) = spec.rsplit_once(':')?;
    let line: u32 = l.parse().ok()?;
    let looks_path = p.contains(['/', '\\']) || p.contains('.');
    (looks_path && !p.is_empty()).then_some((p, line))
}

/// Indexed files matching a user path (absolute, cwd-relative, or repo-relative suffix).
pub fn find_files(
    store: &Store,
    path: &str,
    filter: &Filter,
) -> Result<Vec<(i64, String, String)>> {
    let conn = store.conn();
    let mut out = Vec::new();
    reject_remote(path)?;
    let p = Path::new(path);
    let abs = util::absolute(p);
    if abs.exists() {
        for r in store.repos()? {
            if let Some(rel) = util::rel_under(&r.root, &abs) {
                if filter
                    .repos
                    .as_ref()
                    .is_some_and(|ids| !ids.contains(&r.id))
                {
                    continue;
                }
                // Case-insensitive path match on Windows.
                let row: Option<(i64, String)> = conn
                    .query_row(
                        "SELECT id, path FROM files WHERE repo_id = ?1 AND path = ?2 COLLATE NOCASE",
                        rusqlite::params![r.id, rel],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .optional()?;
                if let Some((id, path)) = row {
                    out.push((id, util::slash(&r.root), path));
                }
            }
        }
        if !out.is_empty() {
            return Ok(out);
        }
    }
    let norm = path.replace('\\', "/");
    let norm = norm.trim_start_matches("./");
    let mut sql = String::from(
        "SELECT f.id, r.root, f.path FROM files f JOIN repos r ON r.id = f.repo_id
         WHERE (f.path = ?1 COLLATE NOCASE OR (length(f.path) > length(?1)
           AND lower(substr(f.path, -length(?1) - 1)) = lower('/' || ?1)))",
    );
    if let Some(r) = &filter.repos {
        let ids: Vec<String> = r.iter().map(i64::to_string).collect();
        sql.push_str(&format!(" AND f.repo_id IN ({})", ids.join(",")));
    }
    sql.push_str(" ORDER BY length(f.path) LIMIT 20");
    let mut st = conn.prepare(&sql)?;
    let rows = st.query_map([norm], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?.replace('\\', "/"),
            r.get::<_, String>(2)?,
        ))
    })?;
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Resolve a symbol spec: `file:line`, qualified name (`pkg.Type.Method`,
/// `mod::Type::method`), or simple name. Returns the best tier of matches.
pub fn resolve_target(store: &Store, spec: &str, filter: &Filter) -> Result<Vec<Sym>> {
    let conn = store.conn();
    let spec = spec.trim();
    if spec.is_empty() {
        return Err(Error::Usage("empty symbol".into()));
    }
    if let Some((path, line)) = split_file_line(spec) {
        let files = find_files(store, path, filter)?;
        let mut out = Vec::new();
        let mut st = conn.prepare_cached(&format!(
            "{SYM_SELECT} WHERE s.file_id = ?1 AND s.start_line <= ?2 AND s.end_line >= ?2
             ORDER BY (s.end_line - s.start_line) ASC LIMIT 1"
        ))?;
        for (fid, ..) in files {
            if let Some(s) = st
                .query_row(rusqlite::params![fid, line], sym_from_row)
                .optional()?
            {
                out.push(s);
            }
        }
        if out.is_empty() {
            return Err(Error::NotFound(format!("no symbol encloses {spec}")));
        }
        return Ok(out);
    }
    let key = qkey(spec);
    let f = filter.sql();
    let tiers = [
        format!("{SYM_SELECT} WHERE s.qkey = ?1 {f}"),
        format!(
            "{SYM_SELECT} WHERE length(s.qkey) > length(?1) AND substr(s.qkey, -length(?1) - 1) = '.' || ?1 {f}"
        ),
        format!("{SYM_SELECT} WHERE s.name = ?1 {f}"),
        format!("{SYM_SELECT} WHERE s.name = ?1 COLLATE NOCASE {f}"),
    ];
    for (i, sql) in tiers.iter().enumerate() {
        let arg = if i >= 2 {
            spec.to_string()
        } else {
            key.clone()
        };
        let mut st =
            conn.prepare_cached(&format!("{sql} ORDER BY s.kind, r.name, f.path LIMIT 50"))?;
        let rows = st.query_map([arg], sym_from_row)?;
        let v = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        if !v.is_empty() {
            return Ok(rank_targets(v));
        }
    }
    Err(Error::NotFound(format!(
        "no symbol named `{spec}` (try `codegraph find {spec}`)"
    )))
}

/// Prefer definitions with bodies over declarations and callables/types over the rest.
fn rank_targets(mut v: Vec<Sym>) -> Vec<Sym> {
    v.sort_by_key(|s| {
        let k = SymKind::parse(&s.kind).map_or(9, |k| match k {
            SymKind::Function | SymKind::Method => 0,
            SymKind::Class | SymKind::Struct | SymKind::Trait | SymKind::Interface => 1,
            SymKind::Enum | SymKind::Type => 2,
            _ => 3,
        });
        (
            k,
            u32::MAX - (s.end_line - s.line),
            s.repo.clone(),
            s.rel.clone(),
        )
    });
    v
}

// ---------------------------------------------------------------------------
// find
// ---------------------------------------------------------------------------

/// A search hit.
#[derive(Debug, Clone, Serialize)]
pub struct Hit {
    /// How it matched: exact, qualified, prefix, substring, doc, fuzzy.
    #[serde(rename = "match")]
    pub how: &'static str,
    /// The symbol.
    #[serde(flatten)]
    pub sym: Sym,
}

fn fts_quote(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

fn is_subsequence(needle: &str, hay: &str) -> bool {
    let mut it = hay.chars();
    needle
        .chars()
        .all(|c| it.by_ref().any(|h| h.eq_ignore_ascii_case(&c)))
}

fn add(tier: &mut HashMap<i64, u8>, ids: Vec<i64>, t: u8) {
    for id in ids {
        tier.entry(id).and_modify(|x| *x = (*x).min(t)).or_insert(t);
    }
}

/// Fuzzy + exact symbol search.
pub fn find(store: &Store, query: &str, filter: &Filter, limit: usize) -> Result<Vec<Hit>> {
    let conn = store.conn();
    let q = query.trim();
    if q.is_empty() {
        return Err(Error::Usage("empty query".into()));
    }
    let limit = limit.clamp(1, 500);
    let f = filter.sql();
    let mut tier: HashMap<i64, u8> = HashMap::new();
    let ids = |sql: &str, arg: &str| -> Result<Vec<i64>> {
        let mut st = conn.prepare_cached(sql)?;
        let rows = st.query_map([arg], |r| r.get::<_, i64>(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    };
    let base = "SELECT s.id FROM symbols s WHERE";
    add(
        &mut tier,
        ids(&format!("{base} s.name = ?1 {f} LIMIT 200"), q)?,
        0,
    );
    add(
        &mut tier,
        ids(
            &format!("{base} s.name = ?1 COLLATE NOCASE {f} LIMIT 200"),
            q,
        )?,
        1,
    );
    let key = qkey(q);
    if key.contains('.') {
        add(
&mut tier,
            ids(
                &format!(
                    "{base} (s.qkey = ?1 OR (length(s.qkey) > length(?1) AND substr(s.qkey, -length(?1) - 1) = '.' || ?1)) {f} LIMIT 200"
                ),
                &key,
            )?,
            1,
        );
    }
    add(
        &mut tier,
        ids(
            &format!("{base} s.name LIKE ?1 ESCAPE '\\' {f} LIMIT 200"),
            &format!(
                "{}%",
                q.replace('\\', "\\\\")
                    .replace('%', "\\%")
                    .replace('_', "\\_")
            ),
        )?,
        2,
    );
    if q.chars().count() >= 3 {
        let fts = format!("{{name qname}} : {}", fts_quote(q));
        add(
&mut tier,
            ids(
                &format!(
                    "{base} s.id IN (SELECT rowid FROM symbols_fts WHERE symbols_fts MATCH ?1 LIMIT 400) {f}"
                ),
                &fts,
            )?,
            3,
        );
        if tier.len() < limit {
            let fts = format!("doc : {}", fts_quote(q));
            add(
&mut tier,
                ids(
                    &format!(
                        "{base} s.id IN (SELECT rowid FROM symbols_fts WHERE symbols_fts MATCH ?1 LIMIT 200) {f}"
                    ),
                    &fts,
                )?,
                4,
            );
        }
    }
    if tier.len() < limit && q.len() >= 2 && !q.contains(['.', ':']) {
        // Subsequence match over distinct names (e.g. `hndlreq` -> handleRequest).
        let mut st = conn.prepare_cached(&format!(
            "SELECT DISTINCT s.name FROM symbols s WHERE 1=1 {f}"
        ))?;
        let names: Vec<String> = st
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut fuzzy: Vec<&String> = names.iter().filter(|n| is_subsequence(q, n)).collect();
        fuzzy.sort_by_key(|n| n.len());
        fuzzy.truncate(limit * 2);
        for n in fuzzy {
            add(
                &mut tier,
                ids(&format!("{base} s.name = ?1 {f} LIMIT 20"), n)?,
                5,
            );
        }
    }
    let mut ranked: Vec<(i64, u8)> = tier.into_iter().collect();
    ranked.sort_by_key(|&(id, t)| (t, id));
    let all_ids: Vec<i64> = ranked.iter().map(|(id, _)| *id).collect();
    let syms = syms_by_ids(conn, &all_ids)?;
    let tiers: HashMap<i64, u8> = ranked.into_iter().collect();
    let mut hits: Vec<Hit> = syms
        .into_iter()
        .map(|s| {
            let t = tiers.get(&s.id).copied().unwrap_or(9);
            let how = match t {
                0 => "exact",
                1 => "qualified",
                2 => "prefix",
                3 => "substring",
                4 => "doc",
                _ => "fuzzy",
            };
            (t, s, how)
        })
        .map(|(t, s, how)| (t, Hit { how, sym: s }))
        .collect::<Vec<_>>()
        .into_iter()
        .map(|(_, h)| h)
        .collect();
    hits.sort_by_key(|h| {
        let t = tiers.get(&h.sym.id).copied().unwrap_or(9);
        let kind_rank = SymKind::parse(&h.sym.kind).map_or(9, |k| match k {
            SymKind::Function | SymKind::Method | SymKind::Class | SymKind::Struct => 0,
            SymKind::Interface | SymKind::Trait | SymKind::Enum | SymKind::Type => 1,
            SymKind::Module | SymKind::Macro => 2,
            SymKind::Const | SymKind::Var => 3,
        });
        (
            t,
            kind_rank,
            !h.sym.exported,
            h.sym.name.len(),
            h.sym.repo.clone(),
            h.sym.rel.clone(),
            h.sym.line,
        )
    });
    hits.truncate(limit);
    Ok(hits)
}

// ---------------------------------------------------------------------------
// def
// ---------------------------------------------------------------------------

/// A definition with its source.
#[derive(Debug, Clone, Serialize)]
pub struct Def {
    /// The symbol.
    #[serde(flatten)]
    pub sym: Sym,
    /// First line of `source` (may precede `line` to include doc comments).
    pub source_line: u32,
    /// Source text.
    pub source: String,
    /// Lines omitted at the end (0 = complete).
    #[serde(skip_serializing_if = "is_zero")]
    pub truncated_lines: u32,
    /// Why `source` is empty: the file on disk is no longer the one that was indexed
    /// ([`STALE_NOTE`]). Absent when the source was read normally.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_zero(n: &u32) -> bool {
    *n == 0
}

/// Note attached to a `def`/`refs` answer whose file no longer matches the index.
pub const STALE_NOTE: &str = "stale: file changed since indexing, re-index";

/// Hard cap on a source file re-read for `def`/`refs` snippets.
const MAX_SOURCE_BYTES: u64 = 64 * 1024 * 1024;

/// What the index recorded about a file: repo root (as stored), repo-relative path,
/// content hash and size.
struct Indexed {
    root: String,
    rel: String,
    hash: String,
    size: i64,
}

/// Re-read an indexed file for snippets, but only if it is still the file that was
/// indexed: a regular file (not a symlink) under the repo root, no larger than the
/// indexed size, with the indexed content hash. Anything else (a file swapped for a
/// symlink to a secret, a FIFO, an edit since indexing) yields `None`.
fn read_lines(f: &Indexed) -> Option<Vec<String>> {
    let root = Path::new(&f.root);
    let max = u64::try_from(f.size).ok()?.min(MAX_SOURCE_BYTES);
    let bytes = util::read_repo_bytes(root, &root.join(&f.rel), max)?;
    if crate::index::content_hash(&bytes) != f.hash {
        return None;
    }
    let text = String::from_utf8_lossy(&bytes);
    Some(text.lines().map(str::to_string).collect())
}

fn indexed_file_of(conn: &Connection, sym_id: i64) -> Result<Option<Indexed>> {
    Ok(conn
        .prepare_cached(
            "SELECT r.root, f.path, f.hash, f.size FROM symbols s
             JOIN files f ON f.id = s.file_id JOIN repos r ON r.id = s.repo_id
             WHERE s.id = ?1",
        )?
        .query_row([sym_id], |r| {
            Ok(Indexed {
                root: r.get(0)?,
                rel: r.get(1)?,
                hash: r.get(2)?,
                size: r.get(3)?,
            })
        })
        .optional()?)
}

fn is_doc_line(l: &str) -> bool {
    let t = l.trim_start();
    t.starts_with("//")
        || t.starts_with("/*")
        || t.starts_with('*')
        || t.starts_with("#[")
        || t.starts_with('@')
        || (t.starts_with('#') && !t.starts_with("#include") && !t.starts_with("#define"))
}

/// Definitions (with source) for a spec; at most `max_defs`.
pub fn def(
    store: &Store,
    spec: &str,
    filter: &Filter,
    max_lines: u32,
    max_defs: usize,
) -> Result<Vec<Def>> {
    let targets = resolve_target(store, spec, filter)?;
    let mut out = Vec::new();
    for sym in targets.into_iter().take(max_defs.max(1)) {
        let read = indexed_file_of(store.conn(), sym.id)?
            .as_ref()
            .and_then(read_lines);
        let note = read.is_none().then(|| STALE_NOTE.to_string());
        let lines = read.unwrap_or_default();
        let start0 = (sym.line as usize).saturating_sub(1);
        let mut first = start0;
        while first > 0
            && start0 - first < 12
            && lines.get(first - 1).is_some_and(|l| is_doc_line(l))
        {
            first -= 1;
        }
        let end0 = (sym.end_line as usize).min(lines.len());
        let cap = first + max_lines.max(1) as usize;
        let shown_end = end0.min(cap);
        let source = lines
            .get(first..shown_end)
            .map(|v| {
                v.iter()
                    .map(|l| clip_line(l))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        out.push(Def {
            source_line: first as u32 + 1,
            truncated_lines: end0.saturating_sub(shown_end) as u32,
            source,
            sym,
            note,
        });
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// refs
// ---------------------------------------------------------------------------

/// One reference site.
#[derive(Debug, Clone, Serialize)]
pub struct RefSite {
    /// Absolute file.
    pub file: String,
    /// 1-based line.
    pub line: u32,
    /// call / ref / impl / inherit.
    pub kind: String,
    /// exact / likely / name_only.
    pub confidence: String,
    /// Enclosing symbol's qualified name (absent at file level).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// The source line, trimmed.
    pub text: String,
    /// Why `text` is empty: the file no longer matches the index ([`STALE_NOTE`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Repo-relative path (text rendering).
    #[serde(skip)]
    pub rel: String,
    /// Repo root (text rendering).
    #[serde(skip)]
    pub root: String,
}

/// References to a symbol.
#[derive(Debug, Clone, Serialize)]
pub struct References {
    /// The symbol(s) the spec resolved to.
    pub targets: Vec<Sym>,
    /// Sites, grouped by file then line.
    pub refs: Vec<RefSite>,
    /// Total before `limit`.
    pub total: usize,
}

/// Line cache for snippets.
#[derive(Default)]
struct Lines(HashMap<String, Option<Vec<String>>>);

impl Lines {
    /// The trimmed line, or `None` when the file is stale (see [`read_lines`]).
    fn get(&mut self, f: &Indexed, line: u32) -> Option<String> {
        let key = format!("{}\0{}", f.root, f.rel);
        let v = self.0.entry(key).or_insert_with(|| read_lines(f));
        v.as_ref().map(|l| {
            l.get((line as usize).saturating_sub(1))
                .map(|s| extract::clip(s.trim(), 140))
                .unwrap_or_default()
        })
    }
}

/// All incoming references (call/ref/impl/inherit) to the symbol(s) named by `spec`.
pub fn refs(
    store: &Store,
    spec: &str,
    filter: &Filter,
    kind: Option<&str>,
    limit: usize,
) -> Result<References> {
    let conn = store.conn();
    let targets = resolve_target(store, spec, filter)?;
    let targets: Vec<Sym> = targets.into_iter().take(10).collect();
    let ids: Vec<String> = targets.iter().map(|t| t.id.to_string()).collect();
    let mut sql = format!(
        "SELECT r.root, f.path, e.line, e.kind, e.confidence, e.from_symbol, f.hash, f.size
         FROM edges e JOIN files f ON f.id = e.file_id JOIN repos r ON r.id = e.repo_id
         WHERE e.to_symbol IN ({}) AND e.kind != 'import'",
        ids.join(",")
    );
    if let Some(k) = kind {
        if crate::model::EdgeKind::parse(k).is_none() {
            return Err(Error::Usage(format!(
                "unknown edge kind `{k}` (call, ref, impl, inherit)"
            )));
        }
        sql.push_str(&format!(" AND e.kind = '{k}'"));
    }
    sql.push_str(" ORDER BY r.name, f.path, e.line");
    let mut st = conn.prepare(&sql)?;
    let rows = st.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, u32>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, Option<String>>(4)?,
            r.get::<_, Option<i64>>(5)?,
            r.get::<_, String>(6)?,
            r.get::<_, i64>(7)?,
        ))
    })?;
    let all: Vec<_> = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    let total = all.len();
    let mut lines = Lines::default();
    let mut qn = conn.prepare_cached("SELECT qname FROM symbols WHERE id = ?1")?;
    let mut out = Vec::new();
    for (raw_root, path, line, kind, conf, from, hash, size) in all.into_iter().take(limit.max(1)) {
        let text = lines.get(
            &Indexed {
                root: raw_root.clone(),
                rel: path.clone(),
                hash,
                size,
            },
            line,
        );
        let root = raw_root.replace('\\', "/");
        let file = format!("{}/{}", root.trim_end_matches('/'), path);
        let from = match from {
            Some(id) => qn.query_row([id], |r| r.get::<_, String>(0)).optional()?,
            None => None,
        };
        out.push(RefSite {
            note: text.is_none().then(|| STALE_NOTE.to_string()),
            text: text.unwrap_or_default(),
            file,
            line,
            kind,
            confidence: conf.unwrap_or_default(),
            from,
            rel: path,
            root,
        });
    }
    Ok(References {
        targets,
        refs: out,
        total,
    })
}

// ---------------------------------------------------------------------------
// callers / callees
// ---------------------------------------------------------------------------

/// A node in a call tree.
#[derive(Debug, Clone, Serialize)]
pub struct CallNode {
    /// The symbol (absent for external / file-level nodes).
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    pub sym: Option<Sym>,
    /// External callee text (`fmt.Println`) or `<file level>`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub external: Option<String>,
    /// Where the call happens (`file:line`, repo-relative in text output).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub site: Option<String>,
    /// Number of call sites collapsed into this node.
    #[serde(skip_serializing_if = "is_one")]
    pub sites: u32,
    /// Edge confidence.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<String>,
    /// Referenced as a value (callback / handler registration), not called.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub as_value: bool,
    /// Reaches the symbol by dynamic dispatch: the call site resolves to this
    /// interface/trait/base method, which the symbol implements.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
    /// Already on the path from the root (recursion).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub cycle: bool,
    /// Expanded elsewhere in this tree.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub seen: bool,
    /// Children omitted because of caps.
    #[serde(skip_serializing_if = "is_zero")]
    pub more: u32,
    /// Children.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<CallNode>,
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_one(n: &u32) -> bool {
    *n <= 1
}

/// Direction of a call-tree walk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    /// Who calls this.
    Callers,
    /// What this calls.
    Callees,
}

/// A call tree rooted at every symbol matching the spec.
#[derive(Debug, Clone, Serialize)]
pub struct CallTree {
    /// callers / callees.
    pub direction: &'static str,
    /// Depth walked.
    pub depth: u32,
    /// One root per matching symbol.
    pub roots: Vec<CallNode>,
    /// Nodes in the answer.
    pub nodes: usize,
    /// Node cap hit.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
}

struct Step {
    other: Option<i64>,
    external: Option<String>,
    root: String,
    path: String,
    line: u32,
    confidence: Option<String>,
    value: bool,
    via: Option<String>,
}

/// Interface / trait / base-class methods that method `id` implements or overrides:
/// same name, declared in a type its container implements or extends (`impl` /
/// `inherit` edges, including derived Go interface satisfaction). Returns (id, qname).
pub fn dispatch_parents(conn: &Connection, id: i64) -> Result<Vec<(i64, String)>> {
    let mut st = conn.prepare_cached(
        "SELECT DISTINCT am.id, am.qname FROM symbols m
         JOIN files mf ON mf.id = m.file_id
         JOIN symbols t ON t.repo_id = m.repo_id AND t.name = m.container
              AND t.kind IN ('struct', 'class', 'type', 'enum')
              AND (m.parent_id IS NULL OR m.parent_id = t.id OR mf.lang = 'rust')
         JOIN files tf ON tf.id = t.file_id AND tf.package = mf.package
         JOIN edges ie ON ie.from_symbol = t.id AND ie.kind IN ('impl', 'inherit')
         JOIN symbols i ON i.id = ie.to_symbol
         JOIN symbols am ON am.parent_id = i.id AND am.name = m.name AND am.kind = 'method'
         WHERE m.id = ?1 AND m.kind = 'method' AND am.id != m.id",
    )?;
    let rows = st.query_map([id], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn steps(conn: &Connection, id: i64, dir: Dir, externals: bool) -> Result<Vec<Step>> {
    let mut out = steps_of(conn, id, dir, externals, None)?;
    if dir == Dir::Callers {
        for (p, qname) in dispatch_parents(conn, id)? {
            out.extend(steps_of(conn, p, dir, externals, Some(&qname))?);
        }
    }
    Ok(out)
}

fn steps_of(
    conn: &Connection,
    id: i64,
    dir: Dir,
    externals: bool,
    via: Option<&str>,
) -> Result<Vec<Step>> {
    let sql = match dir {
        Dir::Callers => {
            "SELECT e.from_symbol, NULL, NULL, r.root, f.path, e.line, e.confidence, e.kind
             FROM edges e JOIN files f ON f.id = e.file_id JOIN repos r ON r.id = e.repo_id
             WHERE e.to_symbol = ?1 AND e.kind IN ('call', 'value') ORDER BY f.path, e.line"
        }
        Dir::Callees => {
            "SELECT e.to_symbol, e.name, e.qualifier, r.root, f.path, e.line, e.confidence, e.kind
             FROM edges e JOIN files f ON f.id = e.file_id JOIN repos r ON r.id = e.repo_id
             WHERE e.from_symbol = ?1 AND e.kind IN ('call', 'value') ORDER BY e.line, e.col"
        }
    };
    let mut st = conn.prepare_cached(sql)?;
    let rows = st.query_map([id], |r| {
        let other: Option<i64> = r.get(0)?;
        let name: Option<String> = r.get(1)?;
        let qual: Option<String> = r.get(2)?;
        let external = match (other, name) {
            (None, Some(n)) => Some(match qual {
                Some(q) if !q.is_empty() => format!("{q}.{n}"),
                _ => n,
            }),
            (None, None) => Some("<file level>".to_string()),
            _ => None,
        };
        Ok(Step {
            other,
            external,
            root: r.get::<_, String>(3)?.replace('\\', "/"),
            path: r.get(4)?,
            line: r.get(5)?,
            confidence: r.get(6)?,
            value: r.get::<_, String>(7)? == "value",
            via: via.map(str::to_string),
        })
    })?;
    // Unresolved value references (`f(x.Field)`) are just data flow, not calls.
    let v: Vec<Step> = rows
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .filter(|s| !(s.value && s.other.is_none()))
        .collect();
    Ok(if externals {
        v
    } else {
        v.into_iter()
            .filter(|s| s.other.is_some() || dir == Dir::Callers)
            .collect()
    })
}

struct Walker<'a> {
    conn: &'a Connection,
    dir: Dir,
    externals: bool,
    nodes: usize,
    truncated: bool,
    expanded: HashSet<i64>,
    cache: HashMap<i64, Option<Sym>>,
}

impl Walker<'_> {
    fn sym(&mut self, id: i64) -> Result<Option<Sym>> {
        if let Some(s) = self.cache.get(&id) {
            return Ok(s.clone());
        }
        let s = sym_by_id(self.conn, id)?.map(Sym::brief);
        self.cache.insert(id, s.clone());
        Ok(s)
    }

    fn expand(&mut self, id: i64, depth: u32, path: &mut Vec<i64>) -> Result<(Vec<CallNode>, u32)> {
        if depth == 0 {
            return Ok((Vec::new(), 0));
        }
        self.expanded.insert(id);
        let steps = steps(self.conn, id, self.dir, self.externals)?;
        // Collapse multiple sites per (other symbol | external name).
        let mut order: Vec<String> = Vec::new();
        let mut groups: HashMap<String, (Step, u32)> = HashMap::new();
        for s in steps {
            let key = match (s.other, &s.external) {
                (Some(o), _) if s.via.is_some() => format!("#{o}~"),
                (Some(o), _) => format!("#{o}"),
                (None, Some(x)) if x == "<file level>" => format!("file:{}", s.path),
                (None, Some(x)) => format!("x:{x}"),
                (None, None) => continue,
            };
            if let Some(g) = groups.get_mut(&key) {
                g.1 += 1;
            } else {
                order.push(key.clone());
                groups.insert(key, (s, 1));
            }
        }
        let mut out = Vec::new();
        let mut more = 0u32;
        for key in order {
            let Some((s, count)) = groups.remove(&key) else {
                continue;
            };
            if out.len() >= MAX_CHILDREN || self.nodes >= MAX_NODES {
                more += 1;
                self.truncated = true;
                continue;
            }
            self.nodes += 1;
            let site = format!("{}/{}:{}", s.root.trim_end_matches('/'), s.path, s.line);
            let mut node = CallNode {
                sym: None,
                external: s.external.clone(),
                site: Some(site),
                sites: count,
                confidence: s.confidence.clone(),
                as_value: s.value,
                via: s.via.clone(),
                cycle: false,
                seen: false,
                more: 0,
                children: Vec::new(),
            };
            if let Some(o) = s.other {
                node.sym = self.sym(o)?;
                if node.external.as_deref() == Some("<file level>") && node.sym.is_some() {
                    node.external = None;
                }
                if path.contains(&o) {
                    node.cycle = true;
                } else if self.expanded.contains(&o) && depth > 1 {
                    node.seen = true;
                } else {
                    path.push(o);
                    let (children, m) = self.expand(o, depth - 1, path)?;
                    path.pop();
                    node.children = children;
                    node.more = m;
                }
            }
            if s.other.is_none() && self.dir == Dir::Callers {
                node.external = Some(format!("<file level: {}>", s.path));
            }
            out.push(node);
        }
        Ok((out, more))
    }
}

/// Callers or callees of the symbol(s) named by `spec`, `depth` levels deep.
pub fn call_tree(
    store: &Store,
    spec: &str,
    filter: &Filter,
    dir: Dir,
    depth: u32,
    externals: bool,
) -> Result<CallTree> {
    let depth = depth.clamp(1, 8);
    let mut targets = resolve_target(store, spec, filter)?;
    let callables: Vec<Sym> = targets
        .iter()
        .filter(|s| SymKind::parse(&s.kind).is_some_and(|k| k.is_callable() || k.is_type_like()))
        .cloned()
        .collect();
    if !callables.is_empty() {
        targets = callables;
    }
    let mut w = Walker {
        conn: store.conn(),
        dir,
        externals,
        nodes: 0,
        truncated: false,
        expanded: HashSet::new(),
        cache: HashMap::new(),
    };
    let mut roots = Vec::new();
    for t in targets.into_iter().take(8) {
        let mut path = vec![t.id];
        let (children, more) = w.expand(t.id, depth, &mut path)?;
        roots.push(CallNode {
            sym: Some(t),
            external: None,
            site: None,
            sites: 1,
            confidence: None,
            as_value: false,
            via: None,
            cycle: false,
            seen: false,
            more,
            children,
        });
    }
    Ok(CallTree {
        direction: match dir {
            Dir::Callers => "callers",
            Dir::Callees => "callees",
        },
        depth,
        nodes: w.nodes,
        truncated: w.truncated,
        roots,
    })
}

// ---------------------------------------------------------------------------
// outline
// ---------------------------------------------------------------------------

/// An outline entry.
#[derive(Debug, Clone, Serialize)]
pub struct OutlineNode {
    /// Name.
    pub name: String,
    /// Kind.
    pub kind: String,
    /// Start line.
    pub line: u32,
    /// End line.
    pub end_line: u32,
    /// First source line.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub signature: String,
    /// Nested symbols.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<OutlineNode>,
}

/// Outline of a file.
#[derive(Debug, Clone, Serialize)]
pub struct Outline {
    /// Absolute file path.
    pub file: String,
    /// Language.
    pub lang: String,
    /// Line count.
    pub lines: u32,
    /// Whether the file came from the index (false = parsed on the fly).
    pub indexed: bool,
    /// Top-level symbols.
    pub symbols: Vec<OutlineNode>,
}

fn nest(items: Vec<(i64, Option<i64>, OutlineNode)>) -> Vec<OutlineNode> {
    fn build(
        i: usize,
        slots: &mut Vec<Option<(i64, OutlineNode)>>,
        children: &HashMap<i64, Vec<usize>>,
        depth: usize,
    ) -> Option<OutlineNode> {
        let (id, mut node) = slots.get_mut(i)?.take()?;
        if depth < 32 {
            if let Some(ch) = children.get(&id) {
                for &c in ch {
                    if let Some(n) = build(c, slots, children, depth + 1) {
                        node.children.push(n);
                    }
                }
            }
        }
        Some(node)
    }
    let mut children: HashMap<i64, Vec<usize>> = HashMap::new();
    let mut roots = Vec::new();
    let ids: HashSet<i64> = items.iter().map(|(id, ..)| *id).collect();
    for (i, (_, parent, _)) in items.iter().enumerate() {
        match parent {
            Some(p) if ids.contains(p) => children.entry(*p).or_default().push(i),
            _ => roots.push(i),
        }
    }
    let mut slots: Vec<Option<(i64, OutlineNode)>> =
        items.into_iter().map(|(id, _, n)| Some((id, n))).collect();
    roots
        .into_iter()
        .filter_map(|i| build(i, &mut slots, &children, 0))
        .collect()
}

/// Nested outline of a file (indexed, or parsed on the fly when not indexed).
/// Longest source line returned (minified bundles, generated tables).
pub const MAX_LINE_CHARS: usize = 400;

/// Truncate a line to [`MAX_LINE_CHARS`] with a `…[+N chars]` marker.
pub fn clip_line(l: &str) -> String {
    let n = l.chars().count();
    if n <= MAX_LINE_CHARS {
        return l.to_string();
    }
    let head: String = l.chars().take(MAX_LINE_CHARS).collect();
    format!("{head}…[+{} chars]", n - MAX_LINE_CHARS)
}

/// Refuse UNC / device paths before any filesystem call sees them.
fn reject_remote(path: &str) -> Result<()> {
    if util::is_remote_path(path) {
        return Err(Error::Usage(format!(
            "`{path}`: UNC and device paths are not allowed"
        )));
    }
    Ok(())
}

/// Nested outline of a file. Indexed files come from the graph; other files are
/// parsed on the fly only when `unindexed_max_bytes` is set (and within that size).
pub fn outline(
    store: &Store,
    path: &str,
    filter: &Filter,
    unindexed_max_bytes: Option<u64>,
) -> Result<Outline> {
    let conn = store.conn();
    let files = find_files(store, path, filter)?;
    if files.len() > 1 {
        let list: Vec<String> = files
            .iter()
            .map(|(_, root, p)| format!("{root}/{p}"))
            .collect();
        return Err(Error::Usage(format!(
            "`{path}` is ambiguous: {}",
            list.join(", ")
        )));
    }
    if let Some((fid, root, rel)) = files.into_iter().next() {
        let (lang, lines): (String, u32) =
            conn.query_row("SELECT lang, lines FROM files WHERE id = ?1", [fid], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?;
        let mut st = conn.prepare_cached(
            "SELECT id, parent_id,
                 CASE WHEN parent_id IS NULL AND container IS NOT NULL AND kind = 'method'
                      THEN container || (CASE WHEN ?2 IN ('rust', 'cpp', 'c') THEN '::' ELSE '.' END) || name
                      ELSE name END,
                 kind, start_line, end_line, signature FROM symbols
             WHERE file_id = ?1 ORDER BY start_line, start_col",
        )?;
        let rows = st.query_map(rusqlite::params![fid, lang], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, Option<i64>>(1)?,
                OutlineNode {
                    name: r.get(2)?,
                    kind: r.get(3)?,
                    line: r.get(4)?,
                    end_line: r.get(5)?,
                    signature: r.get(6)?,
                    children: Vec::new(),
                },
            ))
        })?;
        let items = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        return Ok(Outline {
            file: format!("{}/{rel}", root.trim_end_matches('/')),
            lang,
            lines,
            indexed: true,
            symbols: nest(items),
        });
    }
    // Not indexed: parse directly, if the caller allows it.
    let Some(max_bytes) = unindexed_max_bytes else {
        return Err(Error::NotFound(format!("{path}: not an indexed file")));
    };
    let abs = util::absolute(Path::new(path));
    if abs
        .file_name()
        .is_some_and(|n| util::is_secret_file(&n.to_string_lossy()))
    {
        return Err(Error::Usage(format!(
            "{path}: refusing to read a secret file"
        )));
    }
    let size = std::fs::metadata(&abs)
        .map_err(|e| Error::io(format!("stat {}", abs.display()), e))?
        .len();
    if size > max_bytes {
        return Err(Error::Usage(format!(
            "{path}: {size} bytes exceeds max_file_kb ({max_bytes} bytes)"
        )));
    }
    let lang = Lang::from_path(&abs).ok_or_else(|| {
        Error::NotFound(format!(
            "{path}: not an indexed file or a supported language"
        ))
    })?;
    let bytes = std::fs::read(&abs).map_err(|e| Error::io(format!("read {}", abs.display()), e))?;
    let facts = extract::extract(lang, &bytes)?;
    let items: Vec<(i64, Option<i64>, OutlineNode)> = facts
        .symbols
        .iter()
        .enumerate()
        .map(|(i, s)| {
            (
                i as i64,
                s.parent.map(|p| p as i64),
                OutlineNode {
                    name: match (&s.container, s.parent) {
                        (Some(c), None) if s.kind == SymKind::Method => {
                            format!("{c}{}{}", lang.sep(), s.name)
                        }
                        _ => s.name.clone(),
                    },
                    kind: s.kind.as_str().to_string(),
                    line: s.start_line,
                    end_line: s.end_line,
                    signature: s.signature.clone(),
                    children: Vec::new(),
                },
            )
        })
        .collect();
    Ok(Outline {
        file: util::slash(&abs),
        lang: lang.as_str().to_string(),
        lines: facts.lines,
        indexed: false,
        symbols: nest(items),
    })
}

// ---------------------------------------------------------------------------
// path
// ---------------------------------------------------------------------------

/// One hop on a call path.
#[derive(Debug, Clone, Serialize)]
pub struct Hop {
    /// The symbol.
    #[serde(flatten)]
    pub sym: Sym,
    /// Where it calls the next hop (absent on the last hop).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub calls_at: Option<String>,
}

/// Shortest call path between two symbols.
#[derive(Debug, Clone, Serialize)]
pub struct CallPath {
    /// Hops from source to target.
    pub hops: Vec<Hop>,
    /// Edges on the path.
    pub length: usize,
}

/// Shortest call path (BFS over resolved call edges) from any `from` match to any `to` match.
pub fn path(
    store: &Store,
    from: &str,
    to: &str,
    filter: &Filter,
    max_depth: u32,
) -> Result<CallPath> {
    let conn = store.conn();
    let src = resolve_target(store, from, filter)?;
    let dst: HashSet<i64> = resolve_target(store, to, filter)?
        .iter()
        .map(|s| s.id)
        .collect();
    let mut prev: HashMap<i64, (i64, String)> = HashMap::new();
    let mut depth: HashMap<i64, u32> = HashMap::new();
    let mut q: VecDeque<i64> = VecDeque::new();
    for s in &src {
        depth.insert(s.id, 0);
        q.push_back(s.id);
    }
    let mut st = conn.prepare_cached(
        "SELECT e.to_symbol, r.root, f.path, e.line FROM edges e
         JOIN files f ON f.id = e.file_id JOIN repos r ON r.id = e.repo_id
         WHERE e.from_symbol = ?1 AND e.kind IN ('call', 'value') AND e.to_symbol IS NOT NULL
         ORDER BY e.line",
    )?;
    let mut found = src.iter().find(|s| dst.contains(&s.id)).map(|s| s.id);
    while found.is_none() {
        let Some(cur) = q.pop_front() else { break };
        let d = depth.get(&cur).copied().unwrap_or(0);
        if d >= max_depth || depth.len() > 200_000 {
            continue;
        }
        let rows = st.query_map([cur], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, u32>(3)?,
            ))
        })?;
        for row in rows {
            let (next, root, p, line) = row?;
            if depth.contains_key(&next) {
                continue;
            }
            depth.insert(next, d + 1);
            let site = format!(
                "{}/{p}:{line}",
                root.replace('\\', "/").trim_end_matches('/')
            );
            prev.insert(next, (cur, site));
            if dst.contains(&next) {
                found = Some(next);
                break;
            }
            q.push_back(next);
        }
    }
    let Some(end) = found else {
        return Err(Error::NotFound(format!(
            "no call path from `{from}` to `{to}` within {max_depth} hops"
        )));
    };
    let mut chain: Vec<(i64, Option<String>)> = vec![(end, None)];
    let mut cur = end;
    while let Some((p, site)) = prev.get(&cur) {
        chain.push((*p, Some(site.clone())));
        cur = *p;
    }
    chain.reverse();
    let mut hops = Vec::new();
    for (id, site) in chain {
        if let Some(s) = sym_by_id(conn, id)? {
            hops.push(Hop {
                sym: s.brief(),
                calls_at: site,
            });
        }
    }
    Ok(CallPath {
        length: hops.len().saturating_sub(1),
        hops,
    })
}

// ---------------------------------------------------------------------------
// impact
// ---------------------------------------------------------------------------

/// A dependent found by impact analysis.
#[derive(Debug, Clone, Serialize)]
pub struct Dependent {
    /// Distance from the target (1 = direct).
    pub depth: u32,
    /// The dependent symbol.
    #[serde(flatten)]
    pub sym: Sym,
    /// Edge kind that links it to the previous level.
    pub via: String,
    /// Where (`file:line`).
    pub site: String,
}

/// File-level summary of impact.
#[derive(Debug, Clone, Serialize)]
pub struct ImpactFile {
    /// Absolute file.
    pub file: String,
    /// Dependents in the file.
    pub count: usize,
    /// Smallest depth seen in the file.
    pub min_depth: u32,
}

/// "What breaks if I change X".
#[derive(Debug, Clone, Serialize)]
pub struct Impact {
    /// What was analysed (file path or symbol spec).
    pub target: String,
    /// Symbols the target resolved to.
    pub seeds: usize,
    /// Depth walked.
    pub depth: u32,
    /// Dependents by depth.
    pub dependents: Vec<Dependent>,
    /// Files importing the target file (file targets only).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub importers: Vec<String>,
    /// Affected files.
    pub files: Vec<ImpactFile>,
    /// Total dependents found (before the cap).
    pub total: usize,
    /// Cap hit.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
}

/// Transitive dependents of a file or symbol (reverse call/ref/impl/inherit edges).
pub fn impact(
    store: &Store,
    target: &str,
    filter: &Filter,
    depth: u32,
    limit: usize,
) -> Result<Impact> {
    let conn = store.conn();
    let depth = depth.clamp(1, 10);
    let mut seeds: Vec<i64> = Vec::new();
    let mut importers = Vec::new();
    reject_remote(target)?;
    let is_path = target.contains(['/', '\\']) || Path::new(target).exists();
    let files = if is_path && split_file_line(target).is_none() {
        find_files(store, target, filter)?
    } else {
        Vec::new()
    };
    if files.is_empty() {
        seeds.extend(resolve_target(store, target, filter)?.iter().map(|s| s.id));
    } else {
        let mut st = conn.prepare_cached("SELECT id FROM symbols WHERE file_id = ?1")?;
        let mut imp = conn.prepare_cached(
            "SELECT DISTINCT r.root, f.path FROM edges e JOIN files f ON f.id = e.file_id
             JOIN repos r ON r.id = e.repo_id
             WHERE e.kind = 'import' AND e.file_id != ?1 AND e.to_file IN (
                 SELECT o.id FROM files o JOIN files t ON t.id = ?1
                 WHERE o.id = t.id OR (t.lang = 'go' AND o.repo_id = t.repo_id
                       AND o.lang = 'go' AND o.package = t.package))
             ORDER BY f.path",
        )?;
        for (fid, ..) in &files {
            let ids = st
                .query_map([fid], |r| r.get::<_, i64>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            seeds.extend(ids);
            let rows = imp.query_map([fid], |r| {
                Ok(format!(
                    "{}/{}",
                    r.get::<_, String>(0)?
                        .replace('\\', "/")
                        .trim_end_matches('/'),
                    r.get::<_, String>(1)?
                ))
            })?;
            for r in rows {
                importers.push(r?);
            }
        }
    }
    let seed_set: HashSet<i64> = seeds.iter().copied().collect();
    let mut seen: HashSet<i64> = seed_set.clone();
    let mut frontier: Vec<i64> = seeds.clone();
    let mut found: Vec<(u32, i64, String, String)> = Vec::new();
    let mut st = conn.prepare_cached(
        "SELECT e.from_symbol, e.kind, r.root, f.path, e.line FROM edges e
         JOIN files f ON f.id = e.file_id JOIN repos r ON r.id = e.repo_id
         WHERE e.to_symbol = ?1 AND e.kind != 'import' AND e.from_symbol IS NOT NULL",
    )?;
    let mut total = 0usize;
    for d in 1..=depth {
        let mut next = Vec::new();
        // A method is also reached through the interface / trait methods it implements.
        let mut expanded: Vec<i64> = Vec::new();
        for id in &frontier {
            expanded.push(*id);
            for (p, _) in dispatch_parents(conn, *id)? {
                if seen.insert(p) {
                    expanded.push(p);
                }
            }
        }
        for id in &expanded {
            let rows = st.query_map([id], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, u32>(4)?,
                ))
            })?;
            for row in rows {
                let (from, kind, root, p, line) = row?;
                if !seen.insert(from) {
                    continue;
                }
                total += 1;
                let site = format!(
                    "{}/{p}:{line}",
                    root.replace('\\', "/").trim_end_matches('/')
                );
                found.push((d, from, kind, site));
                next.push(from);
            }
            if seen.len() > 50_000 {
                break;
            }
        }
        if next.is_empty() {
            break;
        }
        frontier = next;
    }
    let truncated = found.len() > limit;
    found.truncate(limit.max(1));
    let mut dependents = Vec::new();
    let mut per_file: HashMap<String, (usize, u32)> = HashMap::new();
    for (d, id, via, site) in found {
        if let Some(s) = sym_by_id(conn, id)? {
            let e = per_file.entry(s.file.clone()).or_insert((0, d));
            e.0 += 1;
            e.1 = e.1.min(d);
            dependents.push(Dependent {
                depth: d,
                sym: s.brief(),
                via,
                site,
            });
        }
    }
    let mut files: Vec<ImpactFile> = per_file
        .into_iter()
        .map(|(file, (count, min_depth))| ImpactFile {
            file,
            count,
            min_depth,
        })
        .collect();
    files.sort_by(|a, b| {
        a.min_depth
            .cmp(&b.min_depth)
            .then(b.count.cmp(&a.count))
            .then(a.file.cmp(&b.file))
    });
    importers.sort();
    importers.dedup();
    Ok(Impact {
        target: target.to_string(),
        seeds: seed_set.len(),
        depth,
        dependents,
        importers,
        files,
        total,
        truncated,
    })
}

// ---------------------------------------------------------------------------
// hotspots
// ---------------------------------------------------------------------------

/// A ranked symbol.
#[derive(Debug, Clone, Serialize)]
pub struct Ranked {
    /// The symbol.
    #[serde(flatten)]
    pub sym: Sym,
    /// The metric (incoming refs, distinct callees, ...).
    pub count: i64,
    /// Secondary metric (distinct referencing files).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub files: Option<i64>,
}

/// Structural hotspots.
#[derive(Debug, Clone, Serialize)]
pub struct Hotspots {
    /// Most-referenced symbols (incoming resolved edges).
    pub most_referenced: Vec<Ranked>,
    /// Largest fan-out (distinct callees).
    pub fan_out: Vec<Ranked>,
    /// Unreferenced non-exported functions/methods (dead-code candidates).
    pub orphans: Vec<Ranked>,
    /// Total orphans before `limit`.
    pub orphan_total: i64,
}

fn repo_clause(filter: &Filter, col: &str) -> String {
    match &filter.repos {
        Some(r) => {
            let ids: Vec<String> = r.iter().map(i64::to_string).collect();
            format!(" AND {col} IN ({})", ids.join(","))
        }
        None => String::new(),
    }
}

/// Most-referenced, largest fan-out, and orphan symbols.
pub fn hotspots(store: &Store, filter: &Filter, limit: usize) -> Result<Hotspots> {
    let conn = store.conn();
    let limit = limit.clamp(1, 200) as i64;
    let rc = repo_clause(filter, "e.repo_id");
    let ranked = |sql: &str| -> Result<Vec<Ranked>> {
        let mut st = conn.prepare(sql)?;
        let rows = st.query_map([limit], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, Option<i64>>(2)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, count, files) = row?;
            if let Some(s) = sym_by_id(conn, id)? {
                out.push(Ranked {
                    sym: s.brief(),
                    count,
                    files,
                });
            }
        }
        Ok(out)
    };
    let most_referenced = ranked(&format!(
        "SELECT e.to_symbol, count(*) c, count(DISTINCT e.file_id) FROM edges e
         WHERE e.to_symbol IS NOT NULL AND e.kind != 'import' {rc}
         GROUP BY e.to_symbol ORDER BY c DESC, e.to_symbol LIMIT ?1"
    ))?;
    let fan_out = ranked(&format!(
        "SELECT e.from_symbol, count(DISTINCT coalesce(e.to_symbol, e.name)) c, NULL FROM edges e
         WHERE e.kind = 'call' AND e.from_symbol IS NOT NULL {rc}
         GROUP BY e.from_symbol ORDER BY c DESC, e.from_symbol LIMIT ?1"
    ))?;
    let sc = repo_clause(filter, "s.repo_id");
    let orphan_where = format!(
        "s.kind IN ('function','method') AND s.exported = 0 {sc}
         AND NOT EXISTS (SELECT 1 FROM edges e WHERE e.to_symbol = s.id)
         AND s.name NOT IN ('main','init','new','default','drop','fmt','from','constructor',
             'setUp','tearDown','render','run','handle','__init__','__main__','ServeHTTP','String','Error')
         AND s.name NOT LIKE 'Test%' AND s.name NOT LIKE 'test%' AND s.name NOT LIKE 'Benchmark%'
         AND s.name NOT LIKE 'Fuzz%' AND s.name NOT LIKE 'Example%' AND s.name NOT LIKE '\\_\\_%' ESCAPE '\\'
         AND s.qkey NOT LIKE '%.tests.%' AND s.qkey NOT LIKE 'tests.%'
         AND s.file_id NOT IN (SELECT id FROM files WHERE path LIKE 'tests/%'
             OR path LIKE '%/tests/%' OR path LIKE 'test/%' OR path LIKE '%/test/%'
             OR path LIKE '%\\_test.go' ESCAPE '\\' OR path LIKE '%.test.%'
             OR path LIKE '%.spec.%' OR path LIKE '%/test\\_%.py' ESCAPE '\\'
             OR path LIKE 'test\\_%.py' ESCAPE '\\' OR path LIKE '%\\_test.py' ESCAPE '\\')"
    );
    let orphan_total: i64 = conn.query_row(
        &format!("SELECT count(*) FROM symbols s WHERE {orphan_where}"),
        [],
        |r| r.get(0),
    )?;
    let orphans = ranked(&format!(
        "SELECT s.id, s.end_line - s.start_line + 1, NULL FROM symbols s WHERE {orphan_where}
         ORDER BY s.end_line - s.start_line DESC, s.id LIMIT ?1"
    ))?;
    Ok(Hotspots {
        most_referenced,
        fan_out,
        orphans,
        orphan_total,
    })
}

// ---------------------------------------------------------------------------
// dot / mermaid
// ---------------------------------------------------------------------------

/// Graph output format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphFormat {
    /// Graphviz DOT.
    Dot,
    /// Mermaid flowchart.
    Mermaid,
}

/// Subgraph around a symbol (callers + callees to `depth`) rendered as DOT or Mermaid.
pub fn graph(
    store: &Store,
    spec: &str,
    filter: &Filter,
    depth: u32,
    format: GraphFormat,
) -> Result<String> {
    let mut nodes: HashMap<i64, Sym> = HashMap::new();
    let mut edges: HashSet<(i64, i64)> = HashSet::new();
    let roots: HashSet<i64>;
    {
        let mut add_tree = |t: &CallTree, dir: Dir| {
            fn walk(
                n: &CallNode,
                parent: Option<i64>,
                dir: Dir,
                nodes: &mut HashMap<i64, Sym>,
                edges: &mut HashSet<(i64, i64)>,
            ) {
                let Some(s) = &n.sym else { return };
                nodes.entry(s.id).or_insert_with(|| s.clone());
                if let Some(p) = parent {
                    match dir {
                        Dir::Callers => edges.insert((s.id, p)),
                        Dir::Callees => edges.insert((p, s.id)),
                    };
                }
                for c in &n.children {
                    walk(c, Some(s.id), dir, nodes, edges);
                }
            }
            for r in &t.roots {
                walk(r, None, dir, &mut nodes, &mut edges);
            }
        };
        let callers = call_tree(store, spec, filter, Dir::Callers, depth, false)?;
        let callees = call_tree(store, spec, filter, Dir::Callees, depth, false)?;
        roots = callers
            .roots
            .iter()
            .filter_map(|r| r.sym.as_ref().map(|s| s.id))
            .collect();
        add_tree(&callers, Dir::Callers);
        add_tree(&callees, Dir::Callees);
    }
    let mut ids: Vec<i64> = nodes.keys().copied().collect();
    ids.sort_unstable();
    let mut edge_list: Vec<(i64, i64)> = edges.into_iter().collect();
    edge_list.sort_unstable();
    let label = |s: &Sym| s.qname.replace('"', "'");
    let mut out = String::new();
    match format {
        GraphFormat::Dot => {
            out.push_str("digraph codegraph {\n  rankdir=LR;\n  node [shape=box, fontname=\"monospace\", fontsize=10];\n");
            for id in &ids {
                let s = &nodes[id];
                let style = if roots.contains(id) {
                    ", style=filled, fillcolor=\"#ffe08a\""
                } else {
                    ""
                };
                out.push_str(&format!(
                    "  n{id} [label=\"{}\\n{}:{}\"{style}];\n",
                    label(s),
                    s.rel,
                    s.line
                ));
            }
            for (a, b) in &edge_list {
                out.push_str(&format!("  n{a} -> n{b};\n"));
            }
            out.push_str("}\n");
        }
        GraphFormat::Mermaid => {
            out.push_str("flowchart LR\n");
            for id in &ids {
                let s = &nodes[id];
                out.push_str(&format!("  n{id}[\"{}\"]\n", label(s)));
            }
            for (a, b) in &edge_list {
                out.push_str(&format!("  n{a} --> n{b}\n"));
            }
            for id in ids.iter().filter(|i| roots.contains(i)) {
                out.push_str(&format!("  style n{id} fill:#ffe08a\n"));
            }
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// status
// ---------------------------------------------------------------------------

/// Per-repo status.
#[derive(Debug, Clone, Serialize)]
pub struct RepoStatus {
    /// Name.
    pub name: String,
    /// Root.
    pub root: String,
    /// Files.
    pub files: i64,
    /// Symbols.
    pub symbols: i64,
    /// Non-import edges.
    pub edges: i64,
    /// Resolved edges.
    pub resolved: i64,
    /// Last indexed (UTC).
    pub indexed_at: String,
}

/// Graph status.
#[derive(Debug, Clone, Serialize)]
pub struct Status {
    /// Database path.
    pub db: String,
    /// Database size in bytes.
    pub db_bytes: u64,
    /// Totals.
    pub repos: usize,
    /// Files.
    pub files: i64,
    /// Symbols.
    pub symbols: i64,
    /// Non-import edges.
    pub edges: i64,
    /// Resolved edges.
    pub resolved: i64,
    /// Resolved by confidence.
    pub by_confidence: HashMap<String, i64>,
    /// Files per language.
    pub languages: Vec<(String, i64)>,
    /// Per-repo details.
    pub repo_list: Vec<RepoStatus>,
}

/// Status of the graph.
pub fn status(store: &Store, filter: &Filter) -> Result<Status> {
    let conn = store.conn();
    let mut repo_list = Vec::new();
    let mut per: HashMap<i64, (i64, i64, i64, i64)> = HashMap::new();
    {
        let mut st = conn.prepare("SELECT repo_id, count(*) FROM files GROUP BY repo_id")?;
        for r in st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))? {
            let (id, n) = r?;
            per.entry(id).or_default().0 = n;
        }
        let mut st = conn.prepare("SELECT repo_id, count(*) FROM symbols GROUP BY repo_id")?;
        for r in st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))? {
            let (id, n) = r?;
            per.entry(id).or_default().1 = n;
        }
        let mut st = conn.prepare(
            "SELECT repo_id, count(*), count(to_symbol) FROM edges WHERE kind != 'import' GROUP BY repo_id",
        )?;
        for r in st.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })? {
            let (id, n, res) = r?;
            let e = per.entry(id).or_default();
            e.2 = n;
            e.3 = res;
        }
    }
    let (mut files, mut symbols, mut edges, mut resolved) = (0, 0, 0, 0);
    for r in store.repos()? {
        if filter
            .repos
            .as_ref()
            .is_some_and(|ids| !ids.contains(&r.id))
        {
            continue;
        }
        let (f, s, e, res) = per.get(&r.id).copied().unwrap_or_default();
        files += f;
        symbols += s;
        edges += e;
        resolved += res;
        repo_list.push(RepoStatus {
            name: r.name,
            root: util::slash(&r.root),
            files: f,
            symbols: s,
            edges: e,
            resolved: res,
            indexed_at: util::format_unix(r.indexed_at),
        });
    }
    let rc = repo_clause(filter, "repo_id");
    let mut by_confidence = HashMap::new();
    {
        let mut st = conn.prepare(&format!(
            "SELECT confidence, count(*) FROM edges WHERE confidence IS NOT NULL {rc} GROUP BY confidence"
        ))?;
        for r in st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))? {
            let (c, n) = r?;
            by_confidence.insert(c, n);
        }
    }
    let mut languages = Vec::new();
    {
        let mut st = conn.prepare(&format!(
            "SELECT lang, count(*) c FROM files WHERE 1=1 {rc} GROUP BY lang ORDER BY c DESC"
        ))?;
        for r in st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))? {
            languages.push(r?);
        }
    }
    let db_bytes = ["", "-wal"]
        .iter()
        .filter_map(|suffix| {
            let mut p = store.path().as_os_str().to_owned();
            p.push(suffix);
            std::fs::metadata(PathBuf::from(p)).ok().map(|m| m.len())
        })
        .sum();
    Ok(Status {
        db: util::slash(store.path()),
        db_bytes,
        repos: repo_list.len(),
        files,
        symbols,
        edges,
        resolved,
        by_confidence,
        languages,
        repo_list,
    })
}

/// Repo (id, name, root) containing an absolute path, if indexed.
pub fn repo_containing(store: &Store, path: &Path) -> Result<Option<(i64, String, PathBuf)>> {
    let repos = store.repos()?;
    if let Some(hit) = deepest_repo(&repos, path, |root| Some(root.to_path_buf())) {
        return Ok(Some(hit));
    }
    // Roots are stored as given, while `path` may be canonical (the cwd, or a confined
    // argument): compare canonical forms too, so symlinked roots still match (macOS
    // /var -> /private/var, a symlinked ~/dev). Remote roots are never touched.
    let Ok(canon) = std::fs::canonicalize(path).map(util::strip_verbatim) else {
        return Ok(None);
    };
    Ok(deepest_repo(&repos, &canon, |root| {
        if util::is_remote_path(&root.to_string_lossy()) {
            return None;
        }
        std::fs::canonicalize(root).ok().map(util::strip_verbatim)
    }))
}

/// The repo with the longest root containing `path`, comparing against `root_of(root)`.
fn deepest_repo(
    repos: &[crate::store::RepoRow],
    path: &Path,
    root_of: impl Fn(&Path) -> Option<PathBuf>,
) -> Option<(i64, String, PathBuf)> {
    let mut best: Option<(i64, String, PathBuf)> = None;
    for r in repos {
        if root_of(&r.root).is_some_and(|root| util::rel_under(&root, path).is_some()) {
            let longer = best
                .as_ref()
                .is_none_or(|(_, _, b)| r.root.as_os_str().len() > b.as_os_str().len());
            if longer {
                best = Some((r.id, r.name.clone(), r.root.clone()));
            }
        }
    }
    best
}

/// Bind a list of ids as SQL parameters (helper for ad-hoc queries in tests).
pub fn count_where(store: &Store, table: &str, cond: &str, args: &[i64]) -> Result<i64> {
    let sql = format!("SELECT count(*) FROM {table} WHERE {cond}");
    Ok(store
        .conn()
        .query_row(&sql, params_from_iter(args.iter()), |r| r.get(0))?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_line_specs() {
        assert_eq!(split_file_line("src/main.rs:12"), Some(("src/main.rs", 12)));
        assert_eq!(split_file_line("C:\\x\\y.go:3"), Some(("C:\\x\\y.go", 3)));
        assert_eq!(split_file_line("Store::new"), None);
        assert_eq!(split_file_line("main"), None);
    }

    #[test]
    fn subsequence() {
        assert!(is_subsequence("hndlreq", "handleRequest"));
        assert!(!is_subsequence("xyz", "handleRequest"));
    }
}

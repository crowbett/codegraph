//! Cross-file resolution: turn each edge's (name, qualifier, file, enclosing symbol)
//! into a target symbol plus a confidence.
//!
//! Heuristic, not a type checker. The rules, in order:
//! 1. self-like qualifiers (`self`, `this`, `Self`, Go receiver) -> methods of the
//!    enclosing type (exact);
//! 2. qualifiers bound by an import (`pkg.F`, `ns.f`, `mod::f`) -> that module only
//!    (exact); a binding pointing outside the repo resolves to nothing;
//! 3. type qualifiers (`Type::new`, `Class.static`) -> methods with that container (likely);
//! 4. unqualified names -> same file (exact), explicit import bindings (exact), same Go
//!    package (exact), glob imports / same Rust crate / C include graph (likely);
//! 5. receiver-expression method calls (`x.foo()`) -> methods named `foo` (likely when the
//!    owning type is mentioned in the file, else name-only); common std method names skipped.

use std::sync::{Mutex, PoisonError};

use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
use std::path::Path;

use rusqlite::params;
use rusqlite::types::{ToSqlOutput, Value, ValueRef};

use crate::error::Result;
use crate::extract::{BindKind, Binding};
use crate::lang::Lang;
use crate::model::{Confidence, EdgeKind, SymKind};
use crate::store::Store;

#[path = "resolve_types.rs"]
mod types;

/// Rust std-trait methods a type commonly implements several times (once per type
/// parameter of the trait), so a name alone cannot pick the impl.
const TRAIT_OVERLOADS: &[&str] = &[
    "from",
    "try_from",
    "into",
    "try_into",
    "from_iter",
    "extend",
    "eq",
    "ne",
    "partial_cmp",
    "cmp",
    "add",
    "sub",
    "mul",
    "div",
    "index",
    "index_mut",
    "as_ref",
    "as_mut",
    "borrow",
];

/// Resolution counters for one repo.
#[derive(Debug, Clone, Copy, Default, serde::Serialize)]
pub struct ResolveStats {
    /// Non-import edges.
    pub edges: usize,
    /// Edges with a target.
    pub resolved: usize,
    /// Of which exact.
    pub exact: usize,
    /// Of which likely.
    pub likely: usize,
    /// Of which name-only.
    pub name_only: usize,
    /// Import edges resolved to an in-repo file.
    pub imports_resolved: usize,
    /// Import edges.
    pub imports: usize,
    /// Derived Go interface-satisfaction edges.
    pub implicit_impls: usize,
    /// Stored edges whose target this resolve changed.
    #[serde(skip)]
    pub updated: usize,
}

struct F {
    id: i64,
    path: String,
    lang: Lang,
    module: String,
    package: String,
    pkgname: Option<String>,
    dir: String,
}

struct S {
    id: i64,
    file: usize,
    parent: Option<i64>,
    name: String,
    kind: SymKind,
    container: Option<String>,
    exported: bool,
    is_default: bool,
    recv: Option<String>,
    span: u32,
    qkey: String,
    ret: Option<String>,
    fields: Vec<(String, String)>,
    /// First line of the definition (Go method shapes for interface satisfaction).
    sig: String,
    /// 1-based start line.
    line: u32,
}

struct E {
    id: i64,
    file: usize,
    from_symbol: Option<i64>,
    from_name: Option<String>,
    name: String,
    qualifier: Option<String>,
    kind: EdgeKind,
    extra: Option<String>,
    old_to: Option<i64>,
    old_conf: Option<String>,
    old_to_file: Option<i64>,
    /// Receiver type hint (`rt` in `extra`).
    hint: Option<String>,
    /// Rust method-call syntax.
    dot: bool,
    /// 1-based line of the reference.
    line: u32,
}

/// Local names an import introduces into a file.
#[derive(Default)]
struct Scope {
    /// local -> target files (empty = outside the repo).
    modules: HashMap<String, Vec<usize>>,
    /// local -> (original name, target files).
    names: HashMap<String, (String, Vec<usize>)>,
    /// local -> target files (default export).
    defaults: HashMap<String, Vec<usize>>,
    /// Glob-imported files.
    globs: Vec<usize>,
    /// C/C++: included files and their companion sources.
    includes: Vec<usize>,
    /// Whether the file has any import at all (JS global-script detection).
    any_import: bool,
}

struct Ctx {
    root: std::path::PathBuf,
    files: Vec<F>,
    syms: Vec<S>,
    by_name: HashMap<String, Vec<usize>>,
    /// (container, method name) -> methods, for typed receiver lookups.
    by_member: HashMap<(String, String), Vec<usize>>,
    by_id: HashMap<i64, usize>,
    by_file: Vec<Vec<usize>>,
    go_pkgs: HashMap<String, Vec<usize>>,
    rust_mods: HashMap<(String, String), Vec<usize>>,
    rust_crates: HashSet<String>,
    /// `#[path = ".."] mod name;`: (declaring file, name) -> the file it loads.
    path_mods: HashMap<(usize, String), usize>,
    /// The inverse: a file loaded through `#[path]` -> the file declaring it.
    path_parent: HashMap<usize, usize>,
    py_mods: HashMap<String, Vec<usize>>,
    js_mods: HashMap<String, usize>,
    /// Workspace packages (`package.json` `name` -> (package dir, entry path)).
    js_pkgs: HashMap<String, (String, Option<String>)>,
    paths: HashMap<String, usize>,
    /// File name (`util.h`) -> files, for `#include` suffix matching.
    by_basename: HashMap<String, Vec<usize>>,
    /// (including dir, include path, system) -> target: kernels repeat includes heavily.
    include_memo: Mutex<HashMap<(String, String, bool), Option<usize>>>,
    by_stem: HashMap<String, Vec<usize>>,
    mentioned: Vec<HashSet<String>>,
    bases: HashMap<String, Vec<String>>,
    /// Types implementing an in-repo trait that are not repo types themselves (`Vec` in
    /// `impl BufMut for Vec<u8>`), by id; see `types::Ty::External`.
    ext_names: Vec<String>,
    ext_ids: HashMap<String, u32>,
    ts_paths: Vec<(String, Vec<String>)>,
    ts_base: Option<String>,
    /// Per-file import scopes (filled after the import pass).
    scopes: Vec<Scope>,
    memo: Memo,
}

/// Evaluated receiver hints keyed by (file, enclosing symbol, hint).
type Memo = Mutex<HashMap<(usize, Option<i64>, String), types::Ty>>;

fn dir_of(p: &str) -> &str {
    p.rfind('/').map_or("", |i| &p[..i])
}

fn stem_of(p: &str) -> &str {
    let name = p.rfind('/').map_or(p, |i| &p[i + 1..]);
    name.rfind('.').map_or(name, |i| &name[..i])
}

fn strip_ext(p: &str) -> &str {
    let name_start = p.rfind('/').map_or(0, |i| i + 1);
    match p[name_start..].rfind('.') {
        Some(i) if i > 0 => &p[..name_start + i],
        _ => p,
    }
}

/// Lexically normalize `a/b/../c` -> `a/c`.
fn norm_rel(p: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for seg in p.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            s => out.push(s),
        }
    }
    out.join("/")
}

fn stoplist(lang: Lang) -> &'static [&'static str] {
    match lang {
        Lang::Rust => &[
            "clone",
            "to_string",
            "to_owned",
            "into",
            "iter",
            "iter_mut",
            "into_iter",
            "map",
            "and_then",
            "unwrap",
            "unwrap_or",
            "unwrap_or_else",
            "unwrap_or_default",
            "expect",
            "ok",
            "err",
            "ok_or",
            "ok_or_else",
            "map_err",
            "push",
            "pop",
            "len",
            "is_empty",
            "get",
            "get_mut",
            "insert",
            "remove",
            "contains",
            "contains_key",
            "entry",
            "or_insert",
            "or_insert_with",
            "or_default",
            "as_str",
            "as_ref",
            "as_mut",
            "as_bytes",
            "collect",
            "filter",
            "filter_map",
            "find",
            "any",
            "all",
            "next",
            "join",
            "split",
            "trim",
            "starts_with",
            "ends_with",
            "replace",
            "lock",
            "borrow",
            "borrow_mut",
            "extend",
            "sort",
            "sort_by",
            "sort_by_key",
            "sort_unstable",
            "dedup",
            "first",
            "last",
            "keys",
            "values",
            "take",
            "skip",
            "chain",
            "zip",
            "enumerate",
            "rev",
            "count",
            "sum",
            "min",
            "max",
            "fold",
            "flat_map",
            "flatten",
            "cloned",
            "copied",
            "parse",
            "fmt",
            "eq",
            "cmp",
            "partial_cmp",
            "hash",
            "to_vec",
            "retain",
            "truncate",
            "clear",
            "is_some",
            "is_none",
            "is_ok",
            "is_err",
            "as_deref",
            "map_or",
            "map_or_else",
            "then",
            "then_some",
            "peek",
            "position",
            "lines",
            "chars",
            "bytes",
            "flush",
            "write_all",
            "read_to_string",
            "exists",
            "parent",
            "file_name",
            "extension",
            "kind",
            "to_path_buf",
            "display",
            "trim_start",
            "trim_end",
            "to_lowercase",
            "to_uppercase",
            "get_or_insert_with",
            "as_slice",
            "min_by_key",
            "max_by_key",
            "unwrap_err",
            "context",
            "with_context",
            "saturating_sub",
            "checked_sub",
            "wrapping_add",
            "abs",
            "powi",
            "sqrt",
            "floor",
            "ceil",
            "round",
            "to_str",
            "to_string_lossy",
            "is_dir",
            "is_file",
            "strip_prefix",
            "strip_suffix",
            "split_whitespace",
            "splitn",
            "rsplit",
            "rfind",
            "char_indices",
            "windows",
            "chunks",
            "binary_search",
            "iter_rev",
            "last_mut",
            "first_mut",
            "swap",
            "reverse",
            "drain",
            "append",
            "into_inner",
            "downcast_ref",
            "send",
            "recv",
            "try_recv",
            "join_all",
            "await",
            "spawn",
            "elapsed",
            "as_secs",
            "as_millis",
            "duration_since",
        ],
        Lang::Go => &[
            "Lock",
            "Unlock",
            "RLock",
            "RUnlock",
            "Done",
            "Wait",
            "Len",
            "Less",
            "Swap",
            "Write",
            "Read",
            "WriteString",
            "WriteHeader",
            "Header",
            "Del",
            "Encode",
            "Decode",
            "Scan",
            "Next",
            "Err",
            "Error",
            "String",
            "Close",
            "Context",
            "WithContext",
            "Value",
            "Unix",
            "Sub",
            "Since",
            "Before",
            "After",
            "Format",
            "Seconds",
            "Milliseconds",
            "Bytes",
            "Reset",
            "Flush",
            "Printf",
            "Println",
            "Sprintf",
            "Fatalf",
            "Fatal",
            "Errorf",
            "Logf",
            "Helper",
            "Parallel",
            "Cleanup",
            "TempDir",
            "Setenv",
            "Skip",
            "Skipf",
        ],
        Lang::Python => &[
            "append",
            "extend",
            "pop",
            "get",
            "items",
            "keys",
            "values",
            "update",
            "join",
            "split",
            "strip",
            "rstrip",
            "lstrip",
            "format",
            "startswith",
            "endswith",
            "replace",
            "lower",
            "upper",
            "read",
            "write",
            "close",
            "encode",
            "decode",
            "copy",
            "sort",
            "add",
            "remove",
            "discard",
            "setdefault",
            "index",
            "count",
            "find",
            "group",
            "match",
            "search",
            "sub",
            "readlines",
            "readline",
            "exists",
            "mkdir",
            "json",
            "raise_for_status",
            "is_file",
            "is_dir",
            "read_text",
            "write_text",
            "glob",
            "iterdir",
            "resolve",
            "with_suffix",
            "splitlines",
            "isdigit",
            "info",
            "debug",
            "warning",
            "error",
            "exception",
            "fetchone",
            "fetchall",
            "execute",
            "commit",
            "cursor",
            "assertEqual",
            "assertTrue",
            "assertFalse",
            "assertIn",
            "assertRaises",
        ],
        Lang::TypeScript | Lang::Tsx | Lang::JavaScript => &[
            "push",
            "pop",
            "shift",
            "unshift",
            "map",
            "filter",
            "reduce",
            "forEach",
            "find",
            "findIndex",
            "some",
            "every",
            "includes",
            "indexOf",
            "slice",
            "splice",
            "concat",
            "join",
            "split",
            "trim",
            "toString",
            "then",
            "catch",
            "finally",
            "get",
            "set",
            "has",
            "delete",
            "add",
            "keys",
            "values",
            "entries",
            "log",
            "error",
            "warn",
            "info",
            "debug",
            "json",
            "text",
            "stringify",
            "parse",
            "replace",
            "match",
            "test",
            "startsWith",
            "endsWith",
            "toLowerCase",
            "toUpperCase",
            "addEventListener",
            "removeEventListener",
            "querySelector",
            "querySelectorAll",
            "getElementById",
            "appendChild",
            "removeChild",
            "preventDefault",
            "stopPropagation",
            "bind",
            "call",
            "apply",
            "resolve",
            "reject",
            "all",
            "assign",
            "freeze",
            "sort",
            "reverse",
            "fill",
            "from",
            "isArray",
            "now",
            "toFixed",
            "padStart",
            "padEnd",
            "charAt",
            "substring",
            "localeCompare",
            "flatMap",
            "at",
            "emit",
            "on",
            "off",
            "once",
            "send",
            "write",
            "end",
            "listen",
            "use",
            "setAttribute",
            "getAttribute",
            "createElement",
            "classList",
            "toggle",
            "contains",
            "focus",
            "blur",
            "click",
            "remove",
            "append",
            "setTimeout",
            "clearTimeout",
            "requestAnimationFrame",
            "fetch",
            "trimEnd",
            "trimStart",
            "repeat",
            "floor",
            "ceil",
            "round",
            "max",
            "min",
            "abs",
            "random",
            "expect",
            "toBe",
            "toEqual",
            "toHaveBeenCalled",
            "describe",
            "it",
        ],
        Lang::C | Lang::Cpp => &[
            "push_back",
            "emplace_back",
            "size",
            "begin",
            "end",
            "empty",
            "clear",
            "insert",
            "erase",
            "find",
            "at",
            "data",
            "c_str",
            "reserve",
            "resize",
            "front",
            "back",
            "pop_back",
            "count",
            "reset",
            "get",
            "release",
            "lock",
            "unlock",
            "str",
            "substr",
            "append",
            "swap",
            "load",
            "store",
            "emplace",
            "value",
            "has_value",
            "length",
            "push",
            "pop",
            "top",
            "rbegin",
            "rend",
            "cbegin",
            "cend",
            "notify_one",
            "notify_all",
            "wait",
            "join",
            "detach",
            "count_if",
            "fetch_add",
            "exchange",
            "compare_exchange_weak",
        ],
    }
}

fn kind_ok(edge: EdgeKind, sym: SymKind, lang: Lang) -> bool {
    match edge {
        EdgeKind::Call => {
            sym.is_callable()
                || matches!(sym, SymKind::Class | SymKind::Struct)
                || (lang == Lang::Go && sym == SymKind::Type)
        }
        EdgeKind::Ref => sym.is_type_like(),
        EdgeKind::Impl => matches!(sym, SymKind::Trait | SymKind::Interface | SymKind::Class),
        EdgeKind::Inherit => matches!(
            sym,
            SymKind::Class | SymKind::Interface | SymKind::Struct | SymKind::Trait
        ),
        EdgeKind::Value => matches!(sym, SymKind::Function | SymKind::Method),
        EdgeKind::Import => false,
    }
}

fn is_self_word(q: &str) -> bool {
    matches!(q, "self" | "this" | "Self" | "cls")
}

/// One `#[path] mod` declaration: (import edge id, the file it loads if indexed).
type PathDecl = (i64, Option<usize>);

/// `edges.extra` of derived Go interface-satisfaction edges (rebuilt on every resolve).
const IMPLICIT: &str = r#"{"implicit":true}"#;

/// Resolve every edge of a repo and write the results.
pub fn resolve_repo(store: &mut Store, repo_id: i64, root: &Path) -> Result<ResolveStats> {
    resolve_full(store, repo_id, root, false).map(|(stats, _)| stats)
}

/// Load the whole repo, resolve it and write what changed; with `keep`, hand back the
/// loaded state for later incremental re-resolves instead of freeing it.
fn resolve_full(
    store: &mut Store,
    repo_id: i64,
    root: &Path,
    keep: bool,
) -> Result<(ResolveStats, Option<Resident>)> {
    let t0 = std::time::Instant::now();
    let mut ctx = load_ctx(store, repo_id, root)?;
    let (mut edges, implicit) = load_edges(store, repo_id, &ctx)?;
    crate::util::trace("resolve.load", t0);
    let (stats, implicit) = resolve_and_write(store, repo_id, &mut ctx, &mut edges, &implicit, t0)?;
    if keep {
        let data_version = store.data_version()?;
        return Ok((
            stats,
            Some(Resident {
                repo_id,
                ctx,
                edges,
                implicit,
                data_version,
            }),
        ));
    }
    // Freeing millions of symbols and edges takes seconds; the caller doesn't wait for it.
    std::thread::spawn(move || drop((ctx, edges)));
    Ok((stats, None))
}

/// Resolve `edges`, write the rows whose target changed plus the derived edges (replacing
/// `implicit`), and bring the edges' stored values up to date in memory. Returns the
/// stats and the ids of the new derived edges.
fn resolve_and_write(
    store: &mut Store,
    repo_id: i64,
    ctx: &mut Ctx,
    edges: &mut [E],
    implicit: &[i64],
    t0: std::time::Instant,
) -> Result<(ResolveStats, Vec<i64>)> {
    let mut stats = ResolveStats::default();
    let outs = resolve_edges(ctx, edges, &mut stats, t0);
    crate::util::trace("resolve.pass2", t0);
    let tx = store.conn_mut().transaction()?;
    {
        let mut st = tx.prepare_cached(
            "UPDATE edges SET to_symbol = ?2, confidence = ?3, to_file = ?4,
                from_symbol = COALESCE(?5, from_symbol) WHERE id = ?1",
        )?;
        for (e, o) in edges.iter_mut().zip(&outs) {
            let changed = e.old_to != o.to
                || e.old_conf.as_deref() != o.conf
                || e.old_to_file != o.to_file
                || (o.from.is_some() && o.from != e.from_symbol);
            if changed {
                stats.updated += 1;
                st.execute(params![e.id, o.to, o.conf, o.to_file, o.from])?;
                e.old_to = o.to;
                e.old_conf = o.conf.map(str::to_string);
                e.old_to_file = o.to_file;
                if o.from.is_some() {
                    e.from_symbol = o.from;
                }
            }
        }
        // Derived edges are replaced in the same transaction as everything else.
        // By id: a filter on the repo's edges would walk millions of rows.
        let mut del = tx.prepare_cached("DELETE FROM edges WHERE id = ?1")?;
        for id in implicit {
            del.execute([id])?;
        }
    }
    let implicit = write_implicit_and_counts(&tx, repo_id, ctx, &mut stats)?;
    tx.commit()?;
    crate::util::trace("resolve.write", t0);
    Ok((stats, implicit))
}

/// A repo's resolution state kept in memory between re-indexes (`watch`, MCP refresh),
/// so an edit reloads only the edited files' symbols and edges instead of the whole
/// repo (seconds on a kernel-sized repo).
pub struct Resident {
    repo_id: i64,
    ctx: Ctx,
    edges: Vec<E>,
    /// Ids of the derived edges the last resolve wrote.
    implicit: Vec<i64>,
    /// `PRAGMA data_version` after the last write: another connection writing the graph
    /// changes it, and the state is then stale.
    data_version: i64,
}

/// Holds the [`Resident`] state of at most one repo: the last big one resolved.
pub struct ResolveCache {
    resident: Option<Resident>,
    min_edges: usize,
    hits: u64,
}

impl Default for ResolveCache {
    fn default() -> Self {
        Self::new()
    }
}

impl ResolveCache {
    /// Keeps repos with at least 100k edges (smaller ones resolve in well under a second
    /// from the store).
    pub fn new() -> Self {
        Self::with_min_edges(100_000)
    }

    /// Keeps repos with at least `min_edges` edges.
    pub fn with_min_edges(min_edges: usize) -> Self {
        Self {
            resident: None,
            min_edges,
            hits: 0,
        }
    }

    /// Forget the kept state (after writes this cache did not see, like a root rescan).
    pub fn clear(&mut self) {
        if let Some(r) = self.resident.take() {
            std::thread::spawn(move || drop(r));
        }
    }

    /// Re-resolves served from the kept state.
    pub fn hits(&self) -> u64 {
        self.hits
    }
}

/// Resolve a repo after an incremental re-index, from `cache` when it holds this repo's
/// state and nobody else wrote the graph since; otherwise load it whole (and keep it if
/// it is big). `changed`: (old file id, new file id) of every re-parsed file;
/// `structural`: files were added or removed, or `--full`.
pub fn resolve_with_cache(
    store: &mut Store,
    repo_id: i64,
    root: &Path,
    cache: &mut ResolveCache,
    changed: &[(i64, i64)],
    structural: bool,
) -> Result<ResolveStats> {
    let data_version = store.data_version()?;
    if let Some(r) = cache.resident.take() {
        let usable = !structural
            && !changed.is_empty()
            && r.repo_id == repo_id
            && r.data_version == data_version;
        if usable {
            if let Some((stats, r)) = resolve_changed(store, r, changed)? {
                cache.hits += 1;
                cache.resident = Some(r);
                if std::env::var_os("CODEGRAPH_VERIFY_RESIDENT").is_some() {
                    // A whole-repo resolve over the result must find nothing to change.
                    let check = resolve_repo(store, repo_id, root)?;
                    eprintln!(
                        "[verify] resident resolve: {} edge(s) differ",
                        check.updated
                    );
                    if let Some(r) = cache.resident.as_mut() {
                        // It rewrote the derived edges: pick up their new ids.
                        let mut st = store.conn().prepare(
                            "SELECT e.id FROM edges e JOIN files f ON f.id = e.file_id
                             WHERE f.repo_id = ?1 AND e.kind = 'impl' AND e.extra = ?2",
                        )?;
                        r.implicit = st
                            .query_map(params![repo_id, IMPLICIT], |row| row.get(0))?
                            .collect::<rusqlite::Result<Vec<i64>>>()?;
                        r.data_version = store.data_version()?;
                    }
                }
                return Ok(stats);
            }
        } else {
            std::thread::spawn(move || drop(r));
        }
    }
    let (stats, r) = resolve_full(store, repo_id, root, true)?;
    if let Some(r) = r {
        if r.edges.len() >= cache.min_edges {
            cache.resident = Some(r);
        } else {
            std::thread::spawn(move || drop(r));
        }
    }
    Ok(stats)
}

/// Splice the re-parsed files into `r` and re-resolve in memory. `None` (nothing written)
/// when a file changed more than its contents (path, language, module or package), which
/// needs the whole-repo path.
fn resolve_changed(
    store: &mut Store,
    mut r: Resident,
    changed: &[(i64, i64)],
) -> Result<Option<(ResolveStats, Resident)>> {
    let t0 = std::time::Instant::now();
    let old_idx: HashMap<i64, usize> = r
        .ctx
        .files
        .iter()
        .enumerate()
        .map(|(i, f)| (f.id, i))
        .collect();
    let list = changed
        .iter()
        .map(|c| c.1.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let mut rows: HashMap<i64, (String, String, String, String, Option<String>)> = HashMap::new();
    {
        let mut st = store.conn().prepare(&format!(
            "SELECT id, path, lang, module, package, pkgname FROM files WHERE id IN ({list})"
        ))?;
        let mut q = st.query([])?;
        while let Some(row) = q.next()? {
            rows.insert(
                row.get(0)?,
                (
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ),
            );
        }
    }
    let mut changed_fi: HashSet<usize> = HashSet::new();
    for (old, new) in changed {
        let (Some(&fi), Some(row)) = (old_idx.get(old), rows.get(new)) else {
            return Ok(None);
        };
        let f = &r.ctx.files[fi];
        let same = f.path == row.0
            && f.lang.as_str() == row.1
            && f.module == row.2
            && f.package == row.3
            && f.pkgname == row.4;
        if !same {
            return Ok(None);
        }
        changed_fi.insert(fi);
    }
    for (old, new) in changed {
        r.ctx.files[old_idx[old]].id = *new;
    }
    let file_idx: HashMap<i64, usize> = r
        .ctx
        .files
        .iter()
        .enumerate()
        .map(|(i, f)| (f.id, i))
        .collect();
    r.edges.retain(|e| !changed_fi.contains(&e.file));
    let mut new_syms: Vec<S> = Vec::new();
    {
        let conn = store.conn();
        let mut st = conn.prepare(&format!(
            "SELECT {SYM_COLS} FROM symbols WHERE file_id IN ({list})"
        ))?;
        let mut q = st.query([])?;
        while let Some(row) = q.next()? {
            if let Some(s) = sym_row(row, &file_idx)? {
                new_syms.push(s);
            }
        }
        let mut st = conn.prepare(&format!(
            "SELECT {EDGE_COLS} FROM edges WHERE file_id IN ({list})"
        ))?;
        let mut q = st.query([])?;
        while let Some(row) = q.next()? {
            match edge_row(row, &file_idx)? {
                Some(Ok(e)) => r.edges.push(e),
                Some(Err(id)) => r.implicit.push(id),
                None => {}
            }
        }
    }
    // New rows get the highest ids, so this is normally already sorted.
    if !r.edges.is_sorted_by_key(|e| e.id) {
        r.edges.par_sort_unstable_by_key(|e| e.id);
    }
    r.ctx.replace_file_syms(&changed_fi, new_syms);
    r.ctx.reset_derived();
    crate::util::trace("resolve.splice", t0);
    let (stats, implicit) =
        resolve_and_write(store, r.repo_id, &mut r.ctx, &mut r.edges, &r.implicit, t0)?;
    r.implicit = implicit;
    r.data_version = store.data_version()?;
    Ok(Some((stats, r)))
}
/// A file's freshly extracted references, not stored yet (bulk loads).
pub struct Pending {
    /// Row id of the file.
    pub file_id: i64,
    /// Row ids of its symbols, in extraction order (`RawRef::from` indexes them).
    pub sym_ids: Vec<i64>,
    /// References.
    pub refs: Vec<crate::extract::RawRef>,
    /// Imports.
    pub imports: Vec<crate::extract::RawImport>,
}

/// Bulk load of a repo that had no stored files: resolve the extracted references in
/// memory and insert each edge once, with its target. Storing them unresolved and
/// updating them afterwards rewrites the whole edges table (16 s of a 60k-file cold index).
pub fn resolve_and_insert(
    store: &mut Store,
    repo_id: i64,
    root: &Path,
    mut pending: Vec<Pending>,
) -> Result<ResolveStats> {
    let t0 = std::time::Instant::now();
    let mut ctx = load_ctx(store, repo_id, root)?;
    let file_idx: HashMap<i64, usize> = ctx
        .files
        .iter()
        .enumerate()
        .map(|(i, f)| (f.id, i))
        .collect();
    // Path order, like edges loaded from the store: resolution must not depend on the
    // order the parsers finished in.
    pending.sort_by_key(|p| file_idx.get(&p.file_id).copied().unwrap_or(usize::MAX));
    let mut edges: Vec<E> = Vec::new();
    let mut cols: Vec<u32> = Vec::new();
    for p in pending {
        let Some(&file) = file_idx.get(&p.file_id) else {
            continue;
        };
        for r in p.refs {
            cols.push(r.col);
            edges.push(E {
                id: edges.len() as i64,
                file,
                from_symbol: r.from.and_then(|i| p.sym_ids.get(i).copied()),
                from_name: r.from_name,
                name: r.name,
                qualifier: r.qualifier,
                kind: r.kind,
                extra: None,
                old_to: None,
                old_conf: None,
                old_to_file: None,
                hint: r.hint,
                dot: r.dot,
                line: r.line,
            });
        }
        for imp in p.imports {
            cols.push(1);
            edges.push(E {
                id: edges.len() as i64,
                file,
                from_symbol: None,
                from_name: None,
                extra: Some(crate::store::import_extra(&imp)),
                name: imp.path,
                qualifier: None,
                kind: EdgeKind::Import,
                old_to: None,
                old_conf: None,
                old_to_file: None,
                hint: None,
                dot: false,
                line: imp.line,
            });
        }
    }
    crate::util::trace("resolve.load", t0);
    let mut stats = ResolveStats::default();
    let outs = resolve_edges(&mut ctx, &edges, &mut stats, t0);
    crate::util::trace("resolve.pass2", t0);
    // `extra` JSON built in parallel; rows go in 64 per statement (per-statement overhead
    // dominated a row-at-a-time insert of millions of edges).
    let extras: Vec<Option<String>> = edges
        .par_iter()
        .map(|e| {
            if e.kind == EdgeKind::Import {
                e.extra.clone()
            } else {
                crate::store::ref_extra(e.hint.as_deref(), e.dot)
            }
        })
        .collect();
    let tx = store.conn_mut().transaction()?;
    {
        const ROWS: usize = 64;
        const HEAD: &str = "INSERT INTO edges(repo_id, file_id, from_symbol, from_name, name,
            qualifier, kind, line, col, extra, to_symbol, to_file, confidence) VALUES ";
        const ONE: &str = "(?,?,?,?,?,?,?,?,?,?,?,?,?)";
        let mut full = tx.prepare(&format!("{HEAD}{}", vec![ONE; ROWS].join(",")))?;
        let mut single = tx.prepare(&format!("{HEAD}{ONE}"))?;
        let int = |v: i64| ToSqlOutput::Owned(Value::Integer(v));
        let opt_int = |v: Option<i64>| ToSqlOutput::Owned(v.map_or(Value::Null, Value::Integer));
        let mut vals: Vec<ToSqlOutput<'_>> = Vec::with_capacity(13 * ROWS);
        let mut start = 0;
        while start < edges.len() {
            let end = (start + ROWS).min(edges.len());
            vals.clear();
            for i in start..end {
                let (e, o) = (&edges[i], &outs[i]);
                vals.extend([
                    int(repo_id),
                    int(ctx.files[e.file].id),
                    opt_int(o.from.or(e.from_symbol)),
                    sql_opt_text(e.from_name.as_deref()),
                    sql_text(&e.name),
                    sql_opt_text(e.qualifier.as_deref()),
                    sql_text(e.kind.as_str()),
                    int(i64::from(e.line)),
                    int(i64::from(cols[i])),
                    sql_opt_text(extras[i].as_deref()),
                    opt_int(o.to),
                    opt_int(o.to_file),
                    sql_opt_text(o.conf),
                ]);
            }
            if end - start == ROWS {
                full.execute(rusqlite::params_from_iter(vals.iter()))?;
            } else {
                for row in vals.chunks(13) {
                    single.execute(rusqlite::params_from_iter(row.iter()))?;
                }
            }
            start = end;
        }
    }
    write_implicit_and_counts(&tx, repo_id, &ctx, &mut stats)?;
    tx.commit()?;
    crate::util::trace("resolve.write", t0);
    std::thread::spawn(move || drop((ctx, edges)));
    Ok(stats)
}

/// A borrowed text parameter.
fn sql_text(v: &str) -> ToSqlOutput<'_> {
    ToSqlOutput::Borrowed(ValueRef::Text(v.as_bytes()))
}

/// A borrowed text parameter, or NULL.
fn sql_opt_text(v: Option<&str>) -> ToSqlOutput<'_> {
    ToSqlOutput::Borrowed(v.map_or(ValueRef::Null, |s| ValueRef::Text(s.as_bytes())))
}

/// Where one edge resolved to (imports: `to_file` only).
#[derive(Default, Clone, Copy)]
struct Out {
    to: Option<i64>,
    conf: Option<&'static str>,
    to_file: Option<i64>,
    /// Enclosing-symbol override resolved from `from_name`.
    from: Option<i64>,
}

/// Resolve `edges`: imports build per-file scopes, then everything else in parallel. One
/// [`Out`] per edge, in order.
fn resolve_edges(
    ctx: &mut Ctx,
    edges: &[E],
    stats: &mut ResolveStats,
    t0: std::time::Instant,
) -> Vec<Out> {
    let mut outs = vec![Out::default(); edges.len()];
    let mut scopes: Vec<Scope> = (0..ctx.files.len()).map(|_| Scope::default()).collect();

    // `#[path]` modules first: `use` paths resolved below may go through them. A name that
    // is also bound by a `use` in the same file, or declared twice, is a `#[cfg]` twin
    // (`#[cfg(not(feature = "std"))] #[path = "core.rs"] mod imp;` next to
    // `use std::io as imp;`): which one the build sees is unknown, so neither is followed.
    let mut used: HashSet<(usize, String)> = HashSet::new();
    // (declaring file, module name) -> every declaration: (edge id, loaded file).
    let mut cands: HashMap<(usize, String), Vec<PathDecl>> = HashMap::new();
    for e in edges.iter().filter(|e| e.kind == EdgeKind::Import) {
        if ctx.files[e.file].lang != Lang::Rust {
            continue;
        }
        let x = parse_extra(e.extra.as_deref());
        if !x.mod_path {
            used.extend(x.bindings.into_iter().map(|b| (e.file, b.local)));
            continue;
        }
        let Some(name) = x.bindings.into_iter().next().map(|b| b.local) else {
            continue;
        };
        let dir = dir_of(&ctx.files[e.file].path);
        let rel = norm_rel(&format!("{dir}/{}", e.name.replace('\\', "/")));
        let target = ctx
            .paths
            .get(&rel)
            .copied()
            .filter(|&t| t != e.file && ctx.files[t].lang == Lang::Rust);
        cands
            .entry((e.file, name))
            .or_default()
            .push((e.id, target));
    }
    let mut path_mod_targets: HashMap<i64, usize> = HashMap::new();
    for (key, decls) in cands {
        let [(id, Some(t))] = decls[..] else { continue };
        if used.contains(&key) {
            continue;
        }
        path_mod_targets.insert(id, t);
        ctx.path_parent.entry(t).or_insert(key.0);
        ctx.path_mods.insert(key, t);
    }

    crate::util::trace("resolve.pathmods", t0);
    // Pass 1: imports build per-file scopes.
    for (i, e) in edges.iter().enumerate() {
        if e.kind != EdgeKind::Import {
            continue;
        }
        stats.imports += 1;
        let x = parse_extra(e.extra.as_deref());
        if x.mod_path {
            let to_file = path_mod_targets.get(&e.id).copied();
            if to_file.is_some() {
                stats.imports_resolved += 1;
            }
            outs[i].to_file = to_file.map(|t| ctx.files[t].id);
            continue;
        }
        let (bindings, system) = (x.bindings, x.system);
        let targets = ctx.import_targets(e.file, &e.name, system);
        let scope = &mut scopes[e.file];
        scope.any_import = true;
        let to_file = targets.as_ref().and_then(|t| t.first().copied());
        if to_file.is_some() {
            stats.imports_resolved += 1;
        }
        ctx.bind(e.file, &e.name, targets.as_deref(), &bindings, scope);
        outs[i].to_file = to_file.map(|t| ctx.files[t].id);
    }
    // C/C++ companions: foo.h <-> foo.c / foo.cpp.
    for (fi, scope) in scopes.iter_mut().enumerate() {
        if !ctx.files[fi].lang.is_c_family() {
            continue;
        }
        let mut extra = Vec::new();
        for &inc in &scope.includes {
            extra.extend(ctx.companions(inc));
        }
        extra.extend(ctx.companions(fi));
        scope.includes.extend(extra);
        scope.includes.sort_unstable();
        scope.includes.dedup();
    }
    crate::util::trace("resolve.pass1", t0);
    ctx.fill_mentions(edges);
    let mut ext: Vec<String> = ctx
        .bases
        .keys()
        .filter(|k| !k.is_empty() && !ctx.by_name.contains_key(k.as_str()))
        .cloned()
        .collect();
    ext.sort_unstable();
    ctx.ext_ids = ext
        .iter()
        .enumerate()
        .map(|(i, n)| (n.clone(), i as u32))
        .collect();
    ctx.ext_names = ext;
    for (fi, scope) in scopes.iter().enumerate() {
        for (orig, _) in scope.names.values() {
            ctx.mentioned[fi].insert(orig.clone());
        }
    }
    ctx.scopes = scopes;
    let ctx: &Ctx = ctx;

    // Pass 2: symbol edges, resolved in parallel (the context is read-only apart from
    // its memo tables), then counted and diffed in order.
    crate::util::trace("resolve.mentions", t0);
    let pass2: Vec<usize> = (0..edges.len())
        .filter(|&i| edges[i].kind != EdgeKind::Import)
        .collect();
    let results: Vec<Resolved> = pass2
        .par_iter()
        .map(|&i| {
            let e = &edges[i];
            let res = ctx.resolve_edge(e, &ctx.scopes[e.file]);
            let from_override = e.from_name.as_ref().and_then(|n| {
                let fake = E {
                    id: 0,
                    file: e.file,
                    from_symbol: None,
                    from_name: None,
                    name: n.clone(),
                    qualifier: None,
                    kind: EdgeKind::Ref,
                    extra: None,
                    old_to: None,
                    old_conf: None,
                    old_to_file: None,
                    hint: None,
                    dot: false,
                    line: e.line,
                };
                ctx.resolve_edge(&fake, &ctx.scopes[e.file])
                    .map(|(s, _)| ctx.syms[s].id)
            });
            (res, from_override)
        })
        .collect();
    for (&i, (res, from_override)) in pass2.iter().zip(results) {
        stats.edges += 1;
        let (to, conf) = match res {
            Some((s, c)) => {
                stats.resolved += 1;
                match c {
                    Confidence::Exact => stats.exact += 1,
                    Confidence::Likely => stats.likely += 1,
                    Confidence::NameOnly => stats.name_only += 1,
                }
                (Some(ctx.syms[s].id), Some(c.as_str()))
            }
            None => (None, None),
        };
        let to_file = res.map(|(s, _)| ctx.files[ctx.syms[s].file].id);
        outs[i] = Out {
            to,
            conf,
            to_file,
            from: from_override,
        };
    }

    outs
}

/// Derived Go interface-satisfaction edges, and the per-repo counts `status` reads.
fn write_implicit_and_counts(
    tx: &rusqlite::Transaction<'_>,
    repo_id: i64,
    ctx: &Ctx,
    stats: &mut ResolveStats,
) -> Result<Vec<i64>> {
    let mut ids = Vec::new();
    let mut ins = tx.prepare_cached(
        "INSERT INTO edges(repo_id, file_id, from_symbol, name, kind, line, col, to_symbol,
            to_file, confidence, extra)
         SELECT ?1, s.file_id, s.id, ?2, 'impl', s.start_line, s.start_col, ?3, ?4,
            'likely', ?5
         FROM symbols s WHERE s.id = ?6",
    )?;
    for (t, i) in ctx.go_implicit_impls() {
        let iface = &ctx.syms[i];
        let n = ins.execute(params![
            repo_id,
            iface.name,
            iface.id,
            ctx.files[iface.file].id,
            IMPLICIT,
            ctx.syms[t].id
        ])?;
        if n > 0 {
            ids.push(tx.last_insert_rowid());
        }
        stats.implicit_impls += 1;
    }
    tx.execute(
        "UPDATE repos SET symbols = ?2, edges = ?3, resolved = ?4, exact = ?5, likely = ?6,
            name_only = ?7 WHERE id = ?1",
        params![
            repo_id,
            ctx.syms.len() as i64,
            (stats.edges + stats.implicit_impls) as i64,
            (stats.resolved + stats.implicit_impls) as i64,
            stats.exact as i64,
            (stats.likely + stats.implicit_impls) as i64,
            stats.name_only as i64,
        ],
    )?;
    Ok(ids)
}
/// Pass-2 result for one edge: (target, confidence) and the from-symbol override.
type Resolved = (Option<(usize, Confidence)>, Option<i64>);

#[derive(serde::Deserialize, Default)]
struct ImportExtra {
    #[serde(default)]
    bindings: Vec<Binding>,
    #[serde(default)]
    system: bool,
    /// Rust `#[path = ".."] mod name;` (see `extract::RawImport::mod_path`).
    #[serde(default)]
    mod_path: bool,
}

fn parse_extra(extra: Option<&str>) -> ImportExtra {
    extra
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default()
}

/// (receiver type hint, method-call syntax) from a reference edge's `extra`.
fn ref_hint(extra: Option<&str>) -> (Option<String>, bool) {
    let Some(v) = extra.and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok()) else {
        return (None, false);
    };
    (
        v["rt"].as_str().map(str::to_string),
        v["dot"].as_bool().unwrap_or(false),
    )
}

/// Rows of one repo in `table`, read on parallel read-only connections, each scanning a
/// slice of the repo's rowid range (`sql` binds ?1 repo, ?2..?3 the slice). One
/// connection steps ~1M rows/s, and a large repo has millions of edges. Results are in
/// no particular order. Falls back to the store's own connection for in-memory stores.
fn par_rows<T, F>(
    store: &Store,
    path: &Path,
    table: &str,
    repo_id: i64,
    sql: &str,
    map: F,
) -> Result<Vec<T>>
where
    T: Send,
    F: Fn(&rusqlite::Row<'_>) -> rusqlite::Result<Option<T>> + Sync,
{
    // The repo's rowid range: per file through `<table>_file` (a few index seeks per
    // file), or one scan while a cold load has that index dropped.
    let has_file_index: bool = store.conn().query_row(
        "SELECT count(*) > 0 FROM sqlite_master WHERE type = 'index' AND name = ?1",
        [format!("{table}_file")],
        |r| r.get(0),
    )?;
    let range_sql = if has_file_index {
        format!(
            "SELECT min(lo), max(hi) FROM (SELECT
                (SELECT min(id) FROM {table} WHERE file_id = f.id) AS lo,
                (SELECT max(id) FROM {table} WHERE file_id = f.id) AS hi
             FROM files f WHERE f.repo_id = ?1)"
        )
    } else {
        format!("SELECT min(id), max(id) FROM {table} WHERE repo_id = ?1")
    };
    let (lo, hi): (Option<i64>, Option<i64>) =
        store
            .conn()
            .query_row(&range_sql, [repo_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
    let (Some(lo), Some(hi)) = (lo, hi) else {
        return Ok(Vec::new());
    };
    let read = |conn: &rusqlite::Connection, a: i64, b: i64| -> Result<Vec<T>> {
        let mut st = conn.prepare_cached(sql)?;
        let mut rows = st.query(params![repo_id, a, b])?;
        let mut out = Vec::new();
        while let Some(r) = rows.next()? {
            if let Some(t) = map(r)? {
                out.push(t);
            }
        }
        Ok(out)
    };
    let span = hi - lo + 1;
    let chunks = if path.is_file() && span > PAR_ROWS_MIN {
        (rayon::current_num_threads() as i64 * 2).clamp(1, span / (PAR_ROWS_MIN / 4))
    } else {
        1
    };
    if chunks <= 1 {
        return read(store.conn(), lo, hi);
    }
    let step = (span + chunks - 1) / chunks;
    let parts: Vec<Result<Vec<T>>> = (0..chunks)
        .into_par_iter()
        .map(|i| {
            let a = lo + i * step;
            let b = (a + step - 1).min(hi);
            let conn = rusqlite::Connection::open_with_flags(
                path,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
                    | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )?;
            read(&conn, a, b)
        })
        .collect();
    let mut out = Vec::new();
    for p in parts {
        out.append(&mut p?);
    }
    Ok(out)
}

/// Below this many rowids a repo is read on one connection.
const PAR_ROWS_MIN: i64 = 200_000;

/// Columns [`sym_row`] decodes.
const SYM_COLS: &str = "id, file_id, parent_id, name, kind, container, exported, is_default, \
    recv, end_line - start_line, qkey, ret, fields, signature, start_line";

/// One `symbols` row (selected as [`SYM_COLS`]); `None` for a file or kind we skip.
fn sym_row(r: &rusqlite::Row<'_>, file_idx: &HashMap<i64, usize>) -> rusqlite::Result<Option<S>> {
    let fid: i64 = r.get(1)?;
    let kind: String = r.get(4)?;
    let (Some(&file), Some(kind)) = (file_idx.get(&fid), SymKind::parse(&kind)) else {
        return Ok(None);
    };
    let fields: Option<String> = r.get(12)?;
    Ok(Some(S {
        id: r.get(0)?,
        file,
        parent: r.get(2)?,
        name: r.get(3)?,
        kind,
        container: r.get(5)?,
        exported: r.get(6)?,
        is_default: r.get(7)?,
        recv: r.get(8)?,
        span: r.get::<_, i64>(9)?.max(0) as u32,
        qkey: r.get(10)?,
        ret: r.get(11)?,
        fields: fields
            .and_then(|f| serde_json::from_str(&f).ok())
            .unwrap_or_default(),
        sig: r.get(13)?,
        line: r.get(14)?,
    }))
}

/// Columns [`edge_row`] decodes.
const EDGE_COLS: &str = "id, file_id, from_symbol, from_name, name, qualifier, kind, extra, \
    to_symbol, confidence, to_file, line";

/// One `edges` row (selected as [`EDGE_COLS`]): `Ok` an edge to resolve, `Err` the id of a
/// derived edge the previous resolve wrote, `None` for a file or kind we skip.
fn edge_row(
    r: &rusqlite::Row<'_>,
    file_idx: &HashMap<i64, usize>,
) -> rusqlite::Result<Option<std::result::Result<E, i64>>> {
    let fid: i64 = r.get(1)?;
    let kind: String = r.get(6)?;
    let (Some(&file), Some(kind)) = (file_idx.get(&fid), EdgeKind::parse(&kind)) else {
        return Ok(None);
    };
    let extra: Option<String> = r.get(7)?;
    if extra.as_deref() == Some(IMPLICIT) {
        return Ok(Some(Err(r.get(0)?)));
    }
    let (hint, dot) = if kind == EdgeKind::Import {
        (None, false)
    } else {
        ref_hint(extra.as_deref())
    };
    Ok(Some(Ok(E {
        id: r.get(0)?,
        file,
        from_symbol: r.get(2)?,
        from_name: r.get(3)?,
        name: r.get(4)?,
        qualifier: r.get(5)?,
        kind,
        extra,
        old_to: r.get(8)?,
        old_conf: r.get(9)?,
        old_to_file: r.get(10)?,
        hint,
        dot,
        line: r.get(11)?,
    })))
}

/// by_name, by_member, by_id, by_file.
type SymTables = (
    HashMap<String, Vec<usize>>,
    HashMap<(String, String), Vec<usize>>,
    HashMap<i64, usize>,
    Vec<Vec<usize>>,
);

/// Symbol lookup tables, built side by side (millions of symbols on large repos).
fn sym_tables(syms: &[S], nfiles: usize) -> SymTables {
    let ((by_name, by_member), (by_id, by_file)) = rayon::join(
        || {
            rayon::join(
                || {
                    let mut m: HashMap<String, Vec<usize>> = HashMap::with_capacity(syms.len() / 2);
                    for (i, s) in syms.iter().enumerate() {
                        m.entry(s.name.clone()).or_default().push(i);
                    }
                    m
                },
                || {
                    let mut m: HashMap<(String, String), Vec<usize>> = HashMap::new();
                    for (i, s) in syms.iter().enumerate() {
                        if let (SymKind::Method, Some(c)) = (s.kind, &s.container) {
                            m.entry((c.clone(), s.name.clone())).or_default().push(i);
                        }
                    }
                    m
                },
            )
        },
        || {
            rayon::join(
                || {
                    let mut m: HashMap<i64, usize> = HashMap::with_capacity(syms.len());
                    for (i, s) in syms.iter().enumerate() {
                        m.insert(s.id, i);
                    }
                    m
                },
                || {
                    let mut v: Vec<Vec<usize>> = vec![Vec::new(); nfiles];
                    for (i, s) in syms.iter().enumerate() {
                        v[s.file].push(i);
                    }
                    v
                },
            )
        },
    );
    (by_name, by_member, by_id, by_file)
}
/// The repo's files and symbols, with their lookup tables.
fn load_ctx(store: &Store, repo_id: i64, root: &Path) -> Result<Ctx> {
    let t0 = std::time::Instant::now();
    let conn = store.conn();
    let mut files = Vec::new();
    let mut file_idx: HashMap<i64, usize> = HashMap::new();
    {
        let mut st = conn.prepare(
            "SELECT id, path, lang, module, package, pkgname FROM files WHERE repo_id = ?1 ORDER BY path",
        )?;
        let rows = st.query_map([repo_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, Option<String>>(5)?,
            ))
        })?;
        for row in rows {
            let (id, path, lang, module, package, pkgname) = row?;
            let Some(lang) = Lang::parse(&lang) else {
                continue;
            };
            file_idx.insert(id, files.len());
            let dir = dir_of(&path).to_string();
            files.push(F {
                id,
                path,
                lang,
                module,
                package,
                pkgname,
                dir,
            });
        }
    }
    crate::util::trace("resolve.load.files", t0);
    let path = store.path().to_path_buf();
    // Path order, not row order: files are written as parsers finish, and ties in
    // resolution are broken by position, so it must not depend on write order.
    let mut syms: Vec<S> = par_rows(
        store,
        &path,
        "symbols",
        repo_id,
        &format!("SELECT {SYM_COLS} FROM symbols WHERE +repo_id = ?1 AND id BETWEEN ?2 AND ?3"),
        |r| sym_row(r, &file_idx),
    )?;
    // `files` is in path order, so (file index, id) is (path, id).
    syms.par_sort_unstable_by_key(|s| (s.file, s.id));
    crate::util::trace("resolve.load.syms", t0);
    let (by_name, by_member, by_id, by_file) = sym_tables(&syms, files.len());
    let mut ctx = Ctx {
        root: root.to_path_buf(),
        by_file,
        mentioned: vec![HashSet::new(); files.len()],
        bases: HashMap::new(),
        ext_names: Vec::new(),
        ext_ids: HashMap::new(),
        files,
        syms,
        by_name,
        by_member,
        by_id,
        go_pkgs: HashMap::new(),
        rust_mods: HashMap::new(),
        rust_crates: HashSet::new(),
        path_mods: HashMap::new(),
        path_parent: HashMap::new(),
        py_mods: HashMap::new(),
        js_mods: HashMap::new(),
        js_pkgs: HashMap::new(),
        paths: HashMap::new(),
        by_basename: HashMap::new(),
        include_memo: Mutex::new(HashMap::new()),
        by_stem: HashMap::new(),
        ts_paths: Vec::new(),
        ts_base: None,
        scopes: Vec::new(),
        memo: Mutex::new(HashMap::new()),
    };
    for (i, f) in ctx.files.iter().enumerate() {
        ctx.paths.insert(f.path.clone(), i);
        let base = f.path.rsplit('/').next().unwrap_or(&f.path).to_string();
        ctx.by_basename.entry(base).or_default().push(i);
        ctx.by_stem
            .entry(stem_of(&f.path).to_string())
            .or_default()
            .push(i);
        match f.lang {
            Lang::Go => ctx.go_pkgs.entry(f.package.clone()).or_default().push(i),
            Lang::Rust => {
                ctx.rust_crates.insert(f.package.clone());
                ctx.rust_mods
                    .entry((f.package.clone(), f.module.clone()))
                    .or_default()
                    .push(i);
            }
            Lang::Python => ctx.py_mods.entry(f.module.clone()).or_default().push(i),
            Lang::TypeScript | Lang::Tsx | Lang::JavaScript => {
                ctx.js_mods.insert(f.module.clone(), i);
            }
            Lang::C | Lang::Cpp => {}
        }
    }
    ctx.load_tsconfig();
    ctx.load_js_packages();
    crate::util::trace("resolve.load.ctx", t0);
    Ok(ctx)
}

/// The repo's stored edges, and the ids of the derived edges the previous resolve wrote.
fn load_edges(store: &Store, repo_id: i64, ctx: &Ctx) -> Result<(Vec<E>, Vec<i64>)> {
    let t0 = std::time::Instant::now();
    let path = store.path().to_path_buf();
    let file_idx: HashMap<i64, usize> = ctx
        .files
        .iter()
        .enumerate()
        .map(|(i, f)| (f.id, i))
        .collect();
    // Ok: an edge to resolve; Err: the id of a derived edge from the previous resolve.
    let rows: Vec<std::result::Result<E, i64>> = par_rows(
        store,
        &path,
        "edges",
        repo_id,
        &format!("SELECT {EDGE_COLS} FROM edges WHERE +repo_id = ?1 AND id BETWEEN ?2 AND ?3"),
        |r| edge_row(r, &file_idx),
    )?;
    let mut edges = Vec::with_capacity(rows.len());
    let mut implicit = Vec::new();
    for r in rows {
        match r {
            Ok(e) => edges.push(e),
            Err(id) => implicit.push(id),
        }
    }
    edges.par_sort_unstable_by_key(|e| e.id);
    crate::util::trace("resolve.load.edges", t0);
    Ok((edges, implicit))
}

impl Ctx {
    /// Swap the symbols of re-parsed files `fis` for `new`, keeping `syms` in exactly the
    /// (file, id) order a fresh load gives: resolution breaks ties by symbol index, so
    /// appending would change results. Each file's symbols are one contiguous slice; it is
    /// replaced in place and every later index in the lookup tables shifts by the size
    /// difference (a pass over the tables, much cheaper than rebuilding them).
    fn replace_file_syms(&mut self, fis: &HashSet<usize>, new: Vec<S>) {
        let mut by_file: HashMap<usize, Vec<S>> = HashMap::new();
        for s in new {
            by_file.entry(s.file).or_default().push(s);
        }
        let mut order: Vec<usize> = fis.iter().copied().collect();
        order.sort_unstable();
        for fi in order {
            let mut add = by_file.remove(&fi).unwrap_or_default();
            add.sort_unstable_by_key(|s| s.id);
            self.splice_file_syms(fi, add);
        }
    }

    fn splice_file_syms(&mut self, fi: usize, add: Vec<S>) {
        // Start of `fi`'s slice: after every symbol of an earlier file.
        let start = self.syms.partition_point(|s| s.file < fi);
        let old_n = self.syms[start..].partition_point(|s| s.file == fi);
        let new_n = add.len();
        let end = start + old_n;
        let shift = |j: usize| -> Option<usize> {
            if j < start {
                Some(j)
            } else if j < end {
                None
            } else {
                Some(j + new_n - old_n)
            }
        };
        for list in self.by_name.values_mut().chain(self.by_member.values_mut()) {
            if list.last().is_some_and(|&j| j < start) {
                continue;
            }
            list.retain_mut(|j| match shift(*j) {
                Some(k) => {
                    *j = k;
                    true
                }
                None => false,
            });
        }
        self.by_id.retain(|_, j| match shift(*j) {
            Some(k) => {
                *j = k;
                true
            }
            None => false,
        });
        for list in &mut self.by_file {
            for j in list.iter_mut() {
                if let Some(k) = shift(*j) {
                    *j = k;
                }
            }
        }
        self.syms.splice(start..end, add);
        self.by_file[fi] = (start..start + new_n).collect();
        for i in start..start + new_n {
            let s = &self.syms[i];
            self.by_id.insert(s.id, i);
            let list = self.by_name.entry(s.name.clone()).or_default();
            let at = list.partition_point(|&j| j < i);
            list.insert(at, i);
            if let (SymKind::Method, Some(c)) = (s.kind, &s.container) {
                let list = self
                    .by_member
                    .entry((c.clone(), s.name.clone()))
                    .or_default();
                let at = list.partition_point(|&j| j < i);
                list.insert(at, i);
            }
        }
    }

    /// Clear what a resolve derives (scopes, mentions, bases, `#[path]` modules, external
    /// type ids, the type memo) before resolving the same context again.
    fn reset_derived(&mut self) {
        self.scopes = Vec::new();
        self.mentioned = vec![HashSet::new(); self.files.len()];
        self.bases.clear();
        self.path_mods.clear();
        self.path_parent.clear();
        self.ext_names.clear();
        self.ext_ids.clear();
        self.memo = Mutex::new(HashMap::new());
    }

    /// `package.json` files of JS/TS packages inside the repo (monorepo workspaces), so a
    /// bare import of a sibling package (`@scope/core`) resolves to its sources.
    fn load_js_packages(&mut self) {
        let mut dirs: HashSet<String> = HashSet::new();
        for f in &self.files {
            if !f.lang.is_js_family() {
                continue;
            }
            let mut d = f.dir.as_str();
            loop {
                if !dirs.insert(d.to_string()) {
                    break;
                }
                match d.rfind('/') {
                    Some(i) => d = &d[..i],
                    None if !d.is_empty() => d = "",
                    None => break,
                }
            }
        }
        // Sorted, shallowest first: a duplicate package name (examples, fixtures) keeps
        // the outermost package, deterministically.
        let mut dirs: Vec<String> = dirs.into_iter().collect();
        dirs.sort_by(|a, b| {
            a.matches('/')
                .count()
                .cmp(&b.matches('/').count())
                .then(a.cmp(b))
        });
        for d in dirs {
            let p = if d.is_empty() {
                self.root.join("package.json")
            } else {
                self.root.join(&d).join("package.json")
            };
            // Repo-controlled: no symlinks/devices/UNC targets, bounded size.
            let Some(text) =
                crate::util::read_repo_file(&self.root, &p, crate::util::MAX_REPO_FILE_BYTES)
            else {
                continue;
            };
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
                continue;
            };
            let Some(name) = v["name"].as_str() else {
                continue;
            };
            let exports = &v["exports"];
            let dot = if exports.is_object() {
                &exports["."]
            } else {
                exports
            };
            let entry = [
                &v["types"],
                &v["typings"],
                &v["source"],
                dot,
                &dot["types"],
                &dot["import"],
                &dot["default"],
                &v["module"],
                &v["main"],
            ]
            .iter()
            .find_map(|x| x.as_str())
            .map(|e| e.trim_start_matches("./").to_string());
            self.js_pkgs.entry(name.to_string()).or_insert((d, entry));
        }
    }

    /// A bare specifier naming a workspace package (`@scope/pkg/sub`, `pkg`).
    fn js_workspace(&self, path: &str) -> Option<usize> {
        let mut segs = path.splitn(3, '/');
        let first = segs.next()?;
        let (name, sub) = if first.starts_with('@') {
            let second = segs.next()?;
            (format!("{first}/{second}"), segs.next())
        } else {
            let rest: Vec<&str> = path.splitn(2, '/').collect();
            (first.to_string(), rest.get(1).copied())
        };
        let (dir, entry) = self.js_pkgs.get(&name)?;
        let join = |p: &str| {
            if dir.is_empty() {
                p.to_string()
            } else {
                format!("{dir}/{p}")
            }
        };
        if let Some(sub) = sub {
            self.js_probe(&join(sub))
                .or_else(|| self.js_probe(&join(&format!("src/{sub}"))))
        } else {
            // The declared entry often points at build output (`dist/index.js`):
            // try it, then the same path under src/, then src/index and index.
            let mut cands: Vec<String> = Vec::new();
            if let Some(e) = entry {
                cands.push(join(e));
                for (from, to) in [("dist/", "src/"), ("lib/", "src/"), ("build/", "src/")] {
                    if let Some(rest) = e.strip_prefix(from) {
                        cands.push(join(&format!("{to}{rest}")));
                    }
                }
            }
            cands.push(join("src/index"));
            cands.push(join("index"));
            cands.iter().find_map(|c| {
                let c = c.strip_suffix(".d.ts").unwrap_or(c);
                self.js_probe(c)
            })
        }
    }

    fn load_tsconfig(&mut self) {
        // Repo-controlled: no symlinks/devices/UNC targets, bounded size.
        let Some(text) = crate::util::read_repo_file(
            &self.root,
            &self.root.join("tsconfig.json"),
            crate::util::MAX_REPO_FILE_BYTES,
        ) else {
            return;
        };
        // tsconfig allows comments and trailing commas: strip line comments, then try.
        let cleaned: String = text
            .lines()
            .map(|l| {
                let t = l.trim_start();
                if t.starts_with("//") {
                    ""
                } else {
                    l
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&cleaned) else {
            return;
        };
        let opts = &v["compilerOptions"];
        let base = opts["baseUrl"]
            .as_str()
            .map(|b| norm_rel(b.trim_start_matches("./")));
        if let Some(paths) = opts["paths"].as_object() {
            for (k, targets) in paths {
                let ts: Vec<String> = targets
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|t| t.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                self.ts_paths.push((k.clone(), ts));
            }
        }
        self.ts_base = base;
    }

    fn fill_mentions(&mut self, edges: &[E]) {
        for e in edges {
            if e.kind != EdgeKind::Import {
                self.mentioned[e.file].insert(e.name.clone());
                // `Store::new()` / `Store.create()` mention `Store` too.
                if let Some(q) = &e.qualifier {
                    let last = q.rsplit(['.', ':']).next().unwrap_or(q);
                    if !last.is_empty() && last.chars().all(|c| c.is_alphanumeric() || c == '_') {
                        self.mentioned[e.file].insert(last.to_string());
                    }
                }
            }
            if matches!(e.kind, EdgeKind::Inherit | EdgeKind::Impl) {
                let from = e.from_name.clone().or_else(|| {
                    e.from_symbol
                        .and_then(|id| self.by_id.get(&id))
                        .map(|&i| self.syms[i].name.clone())
                });
                if let Some(from) = from {
                    self.bases.entry(from).or_default().push(e.name.clone());
                }
            }
        }
    }

    /// Files a C/C++ file is paired with (same stem: foo.h <-> foo.c/.cpp).
    fn companions(&self, fi: usize) -> Vec<usize> {
        let f = &self.files[fi];
        let stem = stem_of(&f.path);
        let Some(same) = self.by_stem.get(stem) else {
            return Vec::new();
        };
        let c: Vec<usize> = same
            .iter()
            .copied()
            .filter(|&o| o != fi && self.files[o].lang.is_c_family())
            .collect();
        let same_dir: Vec<usize> = c
            .iter()
            .copied()
            .filter(|&o| self.files[o].dir == f.dir)
            .collect();
        if same_dir.is_empty() && c.len() <= 2 {
            c
        } else {
            same_dir
        }
    }

    // ---- import targets --------------------------------------------------

    /// Files an import path points at. `None` = could not be resolved in-repo
    /// (external package); `Some(vec![])` never happens.
    fn import_targets(&self, fi: usize, path: &str, system: bool) -> Option<Vec<usize>> {
        let f = &self.files[fi];
        match f.lang {
            Lang::Go => self.go_pkgs.get(path).cloned(),
            Lang::Rust => self.rust_path(fi, path).map(|(files, _)| files),
            Lang::Python => self.py_module(fi, path),
            Lang::TypeScript | Lang::Tsx | Lang::JavaScript => {
                self.js_module(fi, path).map(|x| vec![x])
            }
            Lang::C | Lang::Cpp => self.c_include(fi, path, system).map(|x| vec![x]),
        }
    }

    /// Resolve a Rust path to (module files, remaining item name).
    fn rust_path(&self, fi: usize, path: &str) -> Option<(Vec<usize>, Option<String>)> {
        let f = &self.files[fi];
        let segs: Vec<&str> = path.split("::").filter(|s| !s.is_empty()).collect();
        let first = *segs.first()?;
        let own_crate = f
            .package
            .split('@')
            .next()
            .unwrap_or(&f.package)
            .to_string();
        let (krate, mut base, rest): (String, Vec<String>, &[&str]) = match first {
            // A file outside `src/` (`crate@path` package) loaded through `#[path]` belongs
            // to the crate of its declarer. A `src/` file keeps its own crate even when a
            // test crate also includes it.
            "crate" if f.package.contains('@') => (
                self.files[self.path_root(fi)].package.clone(),
                Vec::new(),
                &segs[1..],
            ),
            "crate" => (f.package.clone(), Vec::new(), &segs[1..]),
            "self" => (f.package.clone(), split_mod(&f.module), &segs[1..]),
            "super" => {
                let mut b = split_mod(&f.module);
                let mut i = 0;
                while segs.get(i) == Some(&"super") {
                    b.pop();
                    i += 1;
                }
                (f.package.clone(), b, &segs[i..])
            }
            c if self.rust_crates.contains(c) && c != own_crate => {
                (c.to_string(), Vec::new(), &segs[1..])
            }
            c if c == own_crate && self.rust_crates.contains(c) => {
                (own_crate.clone(), Vec::new(), &segs[1..])
            }
            _ => {
                // Relative to the current module, then the crate root.
                let cur = split_mod(&f.module);
                let mut cand = cur.clone();
                cand.push(first.to_string());
                let key = (f.package.clone(), cand.join("::"));
                if self.rust_mods.contains_key(&key) {
                    (f.package.clone(), cur, &segs[..])
                } else {
                    let key = (f.package.clone(), first.to_string());
                    if self.rust_mods.contains_key(&key) {
                        (f.package.clone(), Vec::new(), &segs[..])
                    } else {
                        // `mod common;` in tests/it.rs -> tests/common/mod.rs.
                        let t = self.rust_mod_file(fi, first)?;
                        let tf = &self.files[t];
                        (tf.package.clone(), split_mod(&tf.module), &segs[1..])
                    }
                }
            }
        };
        // Also try the library crate for integration tests / bins (`use mycrate::x`).
        let krate = if self.rust_mods.contains_key(&(krate.clone(), String::new()))
            || self.rust_mods.keys().any(|(c, _)| *c == krate)
        {
            krate
        } else {
            own_crate
        };
        let mut krate = krate;
        let mut i = 0;
        while i < rest.len() {
            let mut next = base.clone();
            next.push(rest[i].to_string());
            if self
                .rust_mods
                .contains_key(&(krate.clone(), next.join("::")))
            {
                base = next;
                i += 1;
            } else if let Some(t) = self
                .rust_mods
                .get(&(krate.clone(), base.join("::")))
                .and_then(|fs| fs.iter().find_map(|&bf| self.rust_mod_file(bf, rest[i])))
            {
                // `crate::testenv::X` where tests/it.rs declares `mod testenv;`.
                let tf = &self.files[t];
                krate.clone_from(&tf.package);
                base = split_mod(&tf.module);
                i += 1;
            } else {
                break;
            }
        }
        let files = self.rust_mods.get(&(krate, base.join("::")))?.clone();
        let item = rest.get(i).map(|s| (*s).to_string());
        Some((files, item))
    }

    /// The outermost file whose `#[path]` chain loads `fi` (`fi` itself if none does).
    fn path_root(&self, fi: usize) -> usize {
        let mut cur = fi;
        // Bounded: a cycle of `#[path]` declarations must not hang resolution.
        for _ in 0..64 {
            match self.path_parent.get(&cur) {
                Some(&p) if p != fi => cur = p,
                _ => break,
            }
        }
        cur
    }

    /// The file a `mod name;` declared in file `fi` loads (`name.rs` or `name/mod.rs`
    /// next to it), for module trees outside `src/` (integration tests, examples).
    fn rust_mod_file(&self, fi: usize, name: &str) -> Option<usize> {
        if let Some(&t) = self.path_mods.get(&(fi, name.to_string())) {
            return Some(t);
        }
        let f = &self.files[fi];
        // A crate root or `mod.rs` loads siblings; `a.rs` loads from `a/`.
        let stem = stem_of(&f.path);
        let dir = if f.module.is_empty() || stem == "mod" {
            f.dir.clone()
        } else if f.dir.is_empty() {
            stem.to_string()
        } else {
            format!("{}/{stem}", f.dir)
        };
        let join = |p: String| {
            if dir.is_empty() {
                p
            } else {
                format!("{dir}/{p}")
            }
        };
        [join(format!("{name}.rs")), join(format!("{name}/mod.rs"))]
            .iter()
            .find_map(|p| self.paths.get(p).copied())
            .filter(|&t| self.files[t].lang == Lang::Rust)
    }

    fn py_module(&self, fi: usize, path: &str) -> Option<Vec<usize>> {
        let f = &self.files[fi];
        let dots = path.chars().take_while(|&c| c == '.').count();
        let rest = &path[dots..];
        let module = if dots > 0 {
            let mut base: Vec<&str> = if f.package.is_empty() {
                Vec::new()
            } else {
                f.package.split('.').collect()
            };
            for _ in 1..dots {
                base.pop();
            }
            if !rest.is_empty() {
                base.extend(rest.split('.'));
            }
            base.join(".")
        } else {
            rest.to_string()
        };
        if let Some(x) = self.py_mods.get(&module) {
            return Some(x.clone());
        }
        if dots == 0 && !module.is_empty() {
            // Repo root is not the Python root: unique suffix match.
            let suffix = format!(".{module}");
            let mut hits: Vec<usize> = self
                .py_mods
                .iter()
                .filter(|(k, _)| k.ends_with(&suffix))
                .flat_map(|(_, v)| v.iter().copied())
                .collect();
            // HashMap order is random per process: without this the first of two
            // candidates flipped between runs.
            hits.sort_unstable();
            if !hits.is_empty() && hits.len() <= 2 {
                return Some(hits);
            }
        }
        None
    }

    fn js_probe(&self, base: &str) -> Option<usize> {
        let base = norm_rel(base);
        if let Some(&i) = self.js_mods.get(&base) {
            return Some(i);
        }
        let no_ext = strip_ext(&base);
        if no_ext != base {
            if let Some(&i) = self.js_mods.get(no_ext) {
                return Some(i);
            }
        }
        self.js_mods.get(&format!("{base}/index")).copied()
    }

    fn js_module(&self, fi: usize, path: &str) -> Option<usize> {
        let f = &self.files[fi];
        if path.starts_with('.') {
            let joined = if f.dir.is_empty() {
                path.to_string()
            } else {
                format!("{}/{path}", f.dir)
            };
            return self.js_probe(&joined);
        }
        for (pat, targets) in &self.ts_paths {
            let base = self.ts_base.clone().unwrap_or_default();
            let hit = match pat.strip_suffix('*') {
                Some(prefix) => path.strip_prefix(prefix).map(str::to_string),
                None => (pat == path).then(String::new),
            };
            if let Some(tail) = hit {
                for t in targets {
                    let t = t.replace('*', &tail);
                    let full = if base.is_empty() {
                        t.clone()
                    } else {
                        format!("{base}/{t}")
                    };
                    if let Some(i) = self.js_probe(&full) {
                        return Some(i);
                    }
                }
            }
        }
        if let Some(base) = &self.ts_base {
            if let Some(i) = self.js_probe(&format!("{base}/{path}")) {
                return Some(i);
            }
        }
        self.js_workspace(path)
    }

    fn c_include(&self, fi: usize, path: &str, system: bool) -> Option<usize> {
        let f = &self.files[fi];
        let key = (f.dir.clone(), path.to_string(), system);
        if let Some(&hit) = self
            .include_memo
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&key)
        {
            return hit;
        }
        let hit = self.c_include_uncached(fi, path, system);
        self.include_memo
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key, hit);
        hit
    }

    fn c_include_uncached(&self, fi: usize, path: &str, system: bool) -> Option<usize> {
        let f = &self.files[fi];
        if !system {
            let joined = if f.dir.is_empty() {
                norm_rel(path)
            } else {
                norm_rel(&format!("{}/{path}", f.dir))
            };
            if let Some(&i) = self.paths.get(&joined) {
                return Some(i);
            }
        }
        // Suffix match among files with the same name (not every path in the repo).
        let base = path.rsplit('/').next().unwrap_or(path);
        let suffix = format!("/{path}");
        let cands = self.by_basename.get(base)?;
        cands
            .iter()
            .copied()
            .filter(|&i| {
                let p = &self.files[i].path;
                p.ends_with(&suffix) || p.as_str() == path
            })
            // Prefer the candidate sharing the longest directory prefix, then the shortest
            // path, then the path itself (deterministic ties).
            .min_by_key(|&i| {
                let p = &self.files[i].path;
                let common = p
                    .bytes()
                    .zip(f.path.bytes())
                    .take_while(|(a, b)| a == b)
                    .count();
                (usize::MAX - common, p.len(), p.as_str())
            })
    }

    /// Record the bindings an import introduces.
    fn bind(
        &self,
        fi: usize,
        path: &str,
        targets: Option<&[usize]>,
        bindings: &[Binding],
        scope: &mut Scope,
    ) {
        let f = &self.files[fi];
        let targets: Vec<usize> = targets.map(<[usize]>::to_vec).unwrap_or_default();
        if f.lang.is_c_family() {
            scope.includes.extend(targets.iter().copied());
            return;
        }
        for b in bindings {
            match b.kind {
                BindKind::Module => {
                    let local = if b.local.is_empty() {
                        // Go import without alias: the target's declared package name.
                        targets
                            .first()
                            .and_then(|&t| self.files[t].pkgname.clone())
                            .unwrap_or_else(|| go_default_name(path))
                    } else {
                        b.local.clone()
                    };
                    // Python `import a.b.c` also binds the top package `a`.
                    if f.lang == Lang::Python && local == path && local.contains('.') {
                        let head = local.split('.').next().unwrap_or(&local).to_string();
                        scope
                            .modules
                            .entry(head.clone())
                            .or_insert_with(|| self.py_module(fi, &head).unwrap_or_default());
                    }
                    scope.modules.insert(local, targets.clone());
                }
                BindKind::Default => {
                    scope.defaults.insert(b.local.clone(), targets.clone());
                }
                BindKind::Glob => scope.globs.extend(targets.iter().copied()),
                BindKind::Name => {
                    let local = if b.local.is_empty() {
                        b.name.clone()
                    } else {
                        b.local.clone()
                    };
                    match f.lang {
                        Lang::Rust => {
                            // `use a::b::c` binds `c`: either a module or an item.
                            match self.rust_path(fi, path) {
                                Some((files, None)) => {
                                    scope.modules.insert(local, files);
                                }
                                Some((files, Some(item))) => {
                                    scope.names.insert(local, (item, files));
                                }
                                None => {
                                    scope.names.insert(local, (b.name.clone(), Vec::new()));
                                }
                            }
                        }
                        Lang::Python => {
                            // `from pkg import mod` may bind a submodule.
                            let sub = if path.ends_with('.') {
                                format!("{path}{}", b.name)
                            } else {
                                format!("{path}.{}", b.name)
                            };
                            match self.py_module(fi, &sub) {
                                Some(files) => {
                                    scope.modules.insert(local, files);
                                }
                                None => {
                                    scope.names.insert(local, (b.name.clone(), targets.clone()));
                                }
                            }
                        }
                        _ => {
                            scope.names.insert(local, (b.name.clone(), targets.clone()));
                        }
                    }
                }
            }
        }
    }

    // ---- symbol lookup ---------------------------------------------------

    /// Candidates named `name` in `files` (following JS/Python re-exports one level).
    fn lookup_in(&self, files: &[usize], name: &str, edge: EdgeKind, depth: u8) -> Vec<usize> {
        // No symbol may carry `name` itself (`export { f as g }`, `pub use f as g`): the
        // re-exports below still apply.
        let all: &[usize] = self.by_name.get(name).map_or(&[], Vec::as_slice);
        let set: HashSet<usize> = files.iter().copied().collect();
        let direct: Vec<usize> = all
            .iter()
            .copied()
            .filter(|&s| {
                set.contains(&self.syms[s].file)
                    && self.syms[s].parent.is_none_or(|p| {
                        // top-level, or a module-nested item (Rust inline mod)
                        self.by_id
                            .get(&p)
                            .is_some_and(|&pi| self.syms[pi].kind == SymKind::Module)
                    })
                    && kind_ok(edge, self.syms[s].kind, self.files[self.syms[s].file].lang)
                    // Go `pkg.F` never names a method.
                    && (self.syms[s].kind != SymKind::Method
                        || self.files[self.syms[s].file].lang != Lang::Go
                        || !matches!(edge, EdgeKind::Call | EdgeKind::Value))
            })
            .collect();
        if !direct.is_empty() || depth == 0 {
            return direct;
        }
        // Re-exports in the target module (`pub use a::b::f;`, `pub use a::*;`,
        // `from .x import f`): follow them.
        let mut out = Vec::new();
        for &t in files {
            let Some(sc) = self.scopes.get(t) else {
                continue;
            };
            if let Some((orig, tf)) = sc.names.get(name) {
                if !tf.is_empty() && tf.as_slice() != [t] {
                    out.extend(self.lookup_in(tf, orig, edge, depth - 1));
                }
            }
            // `pub use a::*` / `export * from './a'`
            let lang = self.files[t].lang;
            if !sc.globs.is_empty() && lang == Lang::Rust {
                out.extend(self.lookup_in(&sc.globs, name, edge, depth - 1));
            } else if !sc.globs.is_empty() && lang.is_js_family() {
                // `export *` passes on exported names only.
                out.extend(
                    self.lookup_in(&sc.globs, name, edge, depth - 1)
                        .into_iter()
                        .filter(|&s| self.syms[s].exported),
                );
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    fn default_export(&self, files: &[usize]) -> Option<usize> {
        files.iter().find_map(|&fi| {
            self.by_file[fi]
                .iter()
                .copied()
                .find(|&s| self.syms[s].is_default)
        })
    }

    fn proximity(&self, fi: usize, s: usize) -> (u8, u32, i64) {
        let f = &self.files[fi];
        let o = &self.files[self.syms[s].file];
        let tier = if self.syms[s].file == fi {
            0
        } else if o.module == f.module && o.lang == f.lang {
            1
        } else if o.package == f.package {
            2
        } else if o.dir.starts_with(&f.dir) || f.dir.starts_with(&o.dir) {
            3
        } else {
            4
        };
        (tier, u32::MAX - self.syms[s].span, s as i64)
    }

    /// Same-file candidates ranked by lexical visibility from the symbol enclosing the
    /// reference: defined in the innermost enclosing scope first, then outer scopes, then
    /// top level; a definition nested in some other function is a last resort.
    /// Among equally visible candidates, the definition closest before `line` wins
    /// (sibling test callbacks each declaring their own `class A`), then the closest after.
    fn lexical_best(&self, from: Option<i64>, line: u32, cands: &[usize]) -> Option<usize> {
        let mut chain: Vec<i64> = Vec::new();
        let mut cur = from;
        while let Some(id) = cur {
            chain.push(id);
            cur = self.by_id.get(&id).and_then(|&i| self.syms[i].parent);
        }
        cands.iter().copied().min_by_key(|&s| {
            let sym = &self.syms[s];
            let rank = match sym.parent {
                None => chain.len(),
                Some(p) => chain.iter().position(|&c| c == p).unwrap_or(usize::MAX),
            };
            let dist = if sym.line <= line {
                (0, line - sym.line)
            } else {
                (1, sym.line - line)
            };
            (rank, dist, u32::MAX - sym.span, s)
        })
    }

    fn best(&self, fi: usize, cands: &[usize]) -> Option<usize> {
        cands.iter().copied().min_by_key(|&s| self.proximity(fi, s))
    }

    fn pick(
        &self,
        fi: usize,
        cands: &[usize],
        single: Confidence,
        many: Confidence,
    ) -> Option<(usize, Confidence)> {
        let b = self.best(fi, cands)?;
        Some((b, if cands.len() == 1 { single } else { many }))
    }

    /// Type name of the enclosing method/class of symbol `from`.
    fn self_type(&self, from: Option<i64>) -> (Option<String>, Option<String>) {
        let mut cur = from.and_then(|id| self.by_id.get(&id).copied());
        let mut recv = None;
        while let Some(i) = cur {
            let s = &self.syms[i];
            if recv.is_none() {
                recv.clone_from(&s.recv);
            }
            if s.kind == SymKind::Method {
                if let Some(c) = &s.container {
                    return (Some(c.clone()), recv);
                }
            }
            if s.kind.is_type_like() {
                return (Some(s.name.clone()), recv);
            }
            cur = s.parent.and_then(|p| self.by_id.get(&p).copied());
        }
        (None, recv)
    }

    fn resolve_edge(&self, e: &E, scope: &Scope) -> Option<(usize, Confidence)> {
        let fi = e.file;
        let lang = self.files[fi].lang;
        let cands: Vec<usize> = self
            .by_name
            .get(&e.name)
            .into_iter()
            .flatten()
            .copied()
            .filter(|&s| {
                kind_ok(
                    e.kind,
                    self.syms[s].kind,
                    self.files[self.syms[s].file].lang,
                )
            })
            .collect();
        match e.qualifier.as_deref() {
            Some(q) if !q.is_empty() => self.resolve_qualified(e, q, &cands, scope, lang),
            _ => self.resolve_bare(e, &cands, scope, lang),
        }
    }

    fn resolve_qualified(
        &self,
        e: &E,
        q: &str,
        cands: &[usize],
        scope: &Scope,
        lang: Lang,
    ) -> Option<(usize, Confidence)> {
        let fi = e.file;
        let (self_ty, recv) = self.self_type(e.from_symbol);
        // 1. self-like (unless TS declared `this` explicitly: `function f(this: T)`, whose
        // hint is typed and goes through 1b).
        let declared_this = q == "this" && e.hint.as_deref().is_some_and(|h| h != "S");
        if !declared_this && (is_self_word(q) || recv.as_deref() == Some(q)) {
            if let Some(ty) = &self_ty {
                let m: Vec<usize> = cands
                    .iter()
                    .copied()
                    .filter(|&s| self.syms[s].container.as_deref() == Some(ty.as_str()))
                    .collect();
                if let Some(r) = self.pick(fi, &m, Confidence::Exact, Confidence::Likely) {
                    return Some(r);
                }
            }
            // Inherited / trait-default method: walk base types. Anything else is a
            // field holding a function, an embedded type, or a method we cannot see.
            if let Some(ty) = self_ty {
                let mut frontier = vec![ty];
                let mut seen: HashSet<String> = HashSet::new();
                for _ in 0..4 {
                    let mut next = Vec::new();
                    for t in &frontier {
                        for b in self.bases.get(t).into_iter().flatten() {
                            if seen.insert(b.clone()) {
                                if let Some(r) = self.by_container(fi, cands, b, Confidence::Likely)
                                {
                                    return Some(r);
                                }
                                next.push(b.clone());
                            }
                        }
                    }
                    if next.is_empty() {
                        break;
                    }
                    frontier = next;
                }
            }
            return None;
        }
        // 1b. Typed receiver (`x.f()` where the type of `x` is known or provably external).
        match self.resolve_typed(e) {
            types::Typed::NoOpinion => {}
            types::Typed::Resolved(r) => return r,
        }
        if lang == Lang::Rust && e.dot {
            // `x.f()` in Rust is a method call on a value, never a module path. With no
            // type evidence a name match is right less than a third of the time.
            return self
                .receiver_guess(e, cands, scope, lang)
                .filter(|&(_, c)| c != Confidence::NameOnly);
        }
        // 2. module binding (Go pkg, TS namespace, Python module alias, Rust `use a::m`).
        let head = q.split("::").next().unwrap_or(q);
        let module_hit = scope.modules.get(q).or_else(|| {
            if lang == Lang::Rust {
                scope.modules.get(head)
            } else {
                None
            }
        });
        if let Some(targets) = module_hit {
            if targets.is_empty() {
                return None;
            }
            let files: Vec<usize> = if lang == Lang::Rust && q.contains("::") {
                // `m::sub::f` where `m` is a bound module.
                let path = q.replacen(head, "", 1);
                let base = self.files[targets[0]].module.clone();
                let full = format!("crate::{base}{path}");
                match self.rust_path(targets[0], &full) {
                    Some((f, None)) => f,
                    Some((_, Some(item))) => {
                        return self.by_container(fi, cands, &item, Confidence::Likely);
                    }
                    None => return None,
                }
            } else {
                targets.clone()
            };
            let m = self.lookup_in(&files, &e.name, e.kind, 2);
            return self.pick(fi, &m, Confidence::Exact, Confidence::Likely);
        }
        // Python dotted module qualifier bound as `import a.b`.
        if lang == Lang::Python && q.contains('.') {
            if let Some(targets) = scope.modules.get(head) {
                let sub = q.to_string();
                let files = self.py_module(fi, &sub).unwrap_or_else(|| targets.clone());
                let m = self.lookup_in(&files, &e.name, e.kind, 2);
                return self.pick(fi, &m, Confidence::Exact, Confidence::Likely);
            }
        }
        // 3. Rust paths (`crate::a::f`, `super::f`, `a::b::Type::new`).
        if lang == Lang::Rust && (q.contains("::") || matches!(q, "crate" | "super")) {
            let full = q.to_string();
            match self.rust_path(fi, &full) {
                Some((files, None)) => {
                    let m = self.lookup_in(&files, &e.name, e.kind, 2);
                    return self.pick(fi, &m, Confidence::Exact, Confidence::Likely);
                }
                Some((_, Some(item))) => {
                    return self.by_container(fi, cands, &item, Confidence::Exact);
                }
                None => {
                    let last = q.rsplit("::").next().unwrap_or(q);
                    return self.by_container(fi, cands, last, Confidence::Likely);
                }
            }
        }
        if lang == Lang::Rust
            && !q.contains(['.', '(', '[', '…'])
            && q.chars()
                .next()
                .is_some_and(|c| c.is_lowercase() || c == '_')
        {
            // `util::helper()` where `util` is a module of this crate.
            if let Some((files, None)) = self.rust_path(fi, q) {
                let m = self.lookup_in(&files, &e.name, e.kind, 2);
                return self.pick(fi, &m, Confidence::Exact, Confidence::Likely);
            }
        }
        // Namespace qualifiers (`geo::total`, TS `namespace` members, inline Rust `mod`).
        if let Some(r) = self.by_namespace(fi, q, cands) {
            return Some(r);
        }
        // 4. Name binding used as qualifier (`import {Foo}` ... `Foo.bar()`).
        if let Some((orig, targets)) = scope.names.get(q) {
            if targets.is_empty() && lang != Lang::Rust {
                return None;
            }
            return self.by_container(fi, cands, orig, Confidence::Exact);
        }
        // 5. Type qualifier (`Type::new`, `Class.create`).
        let simple = !q.contains(['.', '(', '[', ':', '-', '>', '…', '"', '\'']);
        if simple {
            let is_type = self
                .by_name
                .get(q)
                .is_some_and(|v| v.iter().any(|&s| self.syms[s].kind.is_type_like()));
            if is_type {
                return self.by_container(fi, cands, q, Confidence::Likely);
            }
            let looks_type = q.chars().next().is_some_and(char::is_uppercase)
                && lang != Lang::Go
                && !q
                    .chars()
                    .all(|c| c.is_uppercase() || c == '_' || c.is_ascii_digit());
            if looks_type {
                // A type we don't know (std / external): don't guess.
                return None;
            }
        }
        // 6. Receiver expression.
        self.receiver_guess(e, cands, scope, lang)
    }

    /// Candidates whose qualified name ends with `<namespace>.<name>` when the
    /// qualifier's last segment names a module/namespace symbol.
    fn by_namespace(&self, fi: usize, q: &str, cands: &[usize]) -> Option<(usize, Confidence)> {
        let norm = q.replace("::", ".");
        let last = norm.rsplit('.').next().unwrap_or(&norm);
        let is_ns = self
            .by_name
            .get(last)
            .is_some_and(|v| v.iter().any(|&s| self.syms[s].kind == SymKind::Module));
        if !is_ns {
            return None;
        }
        let m: Vec<usize> = cands
            .iter()
            .copied()
            .filter(|&s| {
                let k = &self.syms[s].qkey;
                let tail = format!(".{}", self.syms[s].name);
                k.strip_suffix(&tail)
                    .is_some_and(|head| head == norm || head.ends_with(&format!(".{norm}")))
            })
            .collect();
        self.pick(fi, &m, Confidence::Exact, Confidence::Likely)
    }

    fn by_container(
        &self,
        fi: usize,
        cands: &[usize],
        ty: &str,
        conf: Confidence,
    ) -> Option<(usize, Confidence)> {
        let m: Vec<usize> = cands
            .iter()
            .copied()
            .filter(|&s| self.syms[s].container.as_deref() == Some(ty))
            .collect();
        if m.is_empty() {
            return None;
        }
        // `BytesMut::from(x)`: one method per trait impl (`From<&[u8]>`, `From<Bytes>`...);
        // which one depends on the argument type, which we don't know.
        if m.len() > 1
            && self.files[self.syms[m[0]].file].lang == Lang::Rust
            && TRAIT_OVERLOADS.contains(&self.syms[m[0]].name.as_str())
        {
            return None;
        }
        let worse = if conf == Confidence::Exact {
            Confidence::Likely
        } else {
            Confidence::NameOnly
        };
        self.pick(fi, &m, conf, worse)
    }

    fn receiver_guess(
        &self,
        e: &E,
        cands: &[usize],
        scope: &Scope,
        lang: Lang,
    ) -> Option<(usize, Confidence)> {
        if e.kind != EdgeKind::Call {
            // `f(x.Name)` is almost always a field read, not a method value; method values
            // are resolved only through a known receiver type (resolve_typed).
            return None;
        }
        // Very common std method names: only with direct evidence, and never above name-only.
        let common = stoplist(lang).contains(&e.name.as_str());
        let fi = e.file;
        let f = &self.files[fi];
        let reachable: HashSet<usize> = scope
            .modules
            .values()
            .flatten()
            .chain(scope.names.values().flat_map(|(_, t)| t.iter()))
            .chain(scope.defaults.values().flatten())
            .chain(scope.globs.iter())
            .chain(scope.includes.iter())
            .copied()
            .collect();
        let same_family = |o: Lang| {
            o == lang
                || (lang.is_c_family() && o.is_c_family())
                || (lang.is_js_family() && o.is_js_family())
        };
        let mut strong = Vec::new();
        let mut pool = Vec::new();
        for &s in cands {
            let sym = &self.syms[s];
            let of = &self.files[sym.file];
            if sym.kind != SymKind::Method || !same_family(of.lang) {
                continue;
            }
            let mentioned = sym
                .container
                .as_ref()
                .is_some_and(|c| self.mentioned[fi].contains(c));
            if mentioned {
                strong.push(s);
                pool.push(s);
            } else if sym.file == fi || of.package == f.package || reachable.contains(&sym.file) {
                pool.push(s);
            }
        }
        if strong.len() == 1 {
            let c = if common {
                Confidence::NameOnly
            } else {
                Confidence::Likely
            };
            return Some((strong[0], c));
        }
        if common {
            return None;
        }
        let pool = if strong.is_empty() { &pool } else { &strong };
        if pool.is_empty() || pool.len() > 8 {
            // No evidence the receiver's type is one of ours, or too ambiguous.
            return None;
        }
        self.best(fi, pool).map(|s| (s, Confidence::NameOnly))
    }

    fn resolve_bare(
        &self,
        e: &E,
        cands: &[usize],
        scope: &Scope,
        lang: Lang,
    ) -> Option<(usize, Confidence)> {
        if e.hint.as_deref() == Some("L") {
            // A local variable or parameter, not a repo symbol.
            return None;
        }
        let fi = e.file;
        let f = &self.files[fi];
        let (self_ty, _) = self.self_type(e.from_symbol);
        let callable_bare = |s: usize| {
            let sym = &self.syms[s];
            sym.kind != SymKind::Method
                || (lang == Lang::Cpp && sym.container.is_some() && sym.container == self_ty)
                || !matches!(e.kind, EdgeKind::Call | EdgeKind::Value)
        };
        // 1. Same file, innermost lexical scope first.
        let local: Vec<usize> = cands
            .iter()
            .copied()
            .filter(|&s| self.syms[s].file == fi && callable_bare(s))
            .collect();
        if !local.is_empty() {
            return self
                .lexical_best(e.from_symbol, e.line, &local)
                .map(|s| (s, Confidence::Exact));
        }
        // 2. Explicit import bindings.
        if let Some((orig, targets)) = scope.names.get(&e.name) {
            if targets.is_empty() {
                return None;
            }
            let m = self.lookup_in(targets, orig, e.kind, 2);
            let m = if m.is_empty() {
                self.reexported(targets, orig, e.kind)
            } else {
                m
            };
            return self.pick(fi, &m, Confidence::Exact, Confidence::Likely);
        }
        if let Some(targets) = scope.defaults.get(&e.name) {
            return self
                .default_export(targets)
                .filter(|&s| kind_ok(e.kind, self.syms[s].kind, lang) || lang.is_js_family())
                .map(|s| (s, Confidence::Exact));
        }
        if let Some(targets) = scope.modules.get(&e.name) {
            // `const m = require('./m'); m()` -> its default export.
            return self
                .default_export(targets)
                .map(|s| (s, Confidence::Likely));
        }
        // 3. Same Go package / Rust crate.
        match lang {
            Lang::Go => {
                let m: Vec<usize> = cands
                    .iter()
                    .copied()
                    .filter(|&s| {
                        let o = &self.files[self.syms[s].file];
                        o.lang == Lang::Go && o.package == f.package && callable_bare(s)
                    })
                    .collect();
                if !m.is_empty() {
                    return self.pick(fi, &m, Confidence::Exact, Confidence::Likely);
                }
            }
            Lang::Rust => {
                let m: Vec<usize> = cands
                    .iter()
                    .copied()
                    .filter(|&s| {
                        let o = &self.files[self.syms[s].file];
                        o.lang == Lang::Rust
                            && o.package == f.package
                            && o.module == f.module
                            && callable_bare(s)
                    })
                    .collect();
                if !m.is_empty() {
                    return self.pick(fi, &m, Confidence::Exact, Confidence::Likely);
                }
            }
            _ => {}
        }
        // 4. Glob imports and C include graph.
        let mut pool: Vec<usize> = scope.globs.clone();
        if lang.is_c_family() {
            pool.extend(scope.includes.iter().copied());
        }
        if !pool.is_empty() {
            let m: Vec<usize> = self
                .lookup_in(&pool, &e.name, e.kind, 0)
                .into_iter()
                .filter(|&s| callable_bare(s))
                .collect();
            if !m.is_empty() {
                return self.pick(fi, &m, Confidence::Likely, Confidence::Likely);
            }
        }
        // 5. Wider fallbacks per language.
        let same_lang = |s: usize| {
            let o = self.files[self.syms[s].file].lang;
            o == lang || (lang.is_c_family() && o.is_c_family())
        };
        match lang {
            Lang::Rust => {
                let m: Vec<usize> = cands
                    .iter()
                    .copied()
                    .filter(|&s| {
                        let o = &self.files[self.syms[s].file];
                        o.lang == Lang::Rust && o.package == f.package && callable_bare(s)
                    })
                    .collect();
                self.pick(fi, &m, Confidence::Likely, Confidence::NameOnly)
            }
            Lang::C | Lang::Cpp => {
                let m: Vec<usize> = cands
                    .iter()
                    .copied()
                    .filter(|&s| same_lang(s) && callable_bare(s))
                    .collect();
                self.pick(fi, &m, Confidence::Likely, Confidence::NameOnly)
            }
            Lang::JavaScript if !scope.any_import => {
                // Classic script: shared global scope.
                let m: Vec<usize> = cands
                    .iter()
                    .copied()
                    .filter(|&s| {
                        self.files[self.syms[s].file].lang == Lang::JavaScript && callable_bare(s)
                    })
                    .collect();
                self.pick(fi, &m, Confidence::Likely, Confidence::NameOnly)
            }
            _ => None,
        }
    }

    /// Follow `export * from` / `export {x} from` style re-exports one level.
    fn reexported(&self, targets: &[usize], name: &str, kind: EdgeKind) -> Vec<usize> {
        // Barrel files: symbols named `name` in any file the target imports.
        let mut out = Vec::new();
        for &t in targets {
            let dir = &self.files[t].dir;
            let Some(all) = self.by_name.get(name) else {
                continue;
            };
            out.extend(all.iter().copied().filter(|&s| {
                let sf = &self.files[self.syms[s].file];
                sf.dir.starts_with(dir.as_str())
                    && self.syms[s].exported
                    && self.syms[s].parent.is_none()
                    && kind_ok(kind, self.syms[s].kind, sf.lang)
            }));
        }
        out
    }
}

fn split_mod(m: &str) -> Vec<String> {
    if m.is_empty() {
        Vec::new()
    } else {
        m.split("::").map(str::to_string).collect()
    }
}

/// Default local name of a Go import path whose package is outside the repo.
fn go_default_name(path: &str) -> String {
    let mut segs: Vec<&str> = path.split('/').collect();
    let mut last = segs.pop().unwrap_or(path);
    if last.len() >= 2
        && last.starts_with('v')
        && last[1..].chars().all(|c| c.is_ascii_digit())
        && !segs.is_empty()
    {
        last = segs.pop().unwrap_or(last);
    }
    let last = last.strip_prefix("go-").unwrap_or(last);
    let last = last.strip_suffix("-go").unwrap_or(last);
    let last = last.strip_suffix(".go").unwrap_or(last);
    // gopkg.in/yaml.v3 -> yaml
    let last = match last.rfind(".v") {
        Some(i) if last[i + 2..].chars().all(|c| c.is_ascii_digit()) && i + 2 < last.len() => {
            &last[..i]
        }
        _ => last,
    };
    last.replace(['-', '.'], "_")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn go_names() {
        assert_eq!(go_default_name("github.com/mattn/go-sqlite3"), "sqlite3");
        assert_eq!(go_default_name("github.com/jackc/pgx/v5"), "pgx");
        assert_eq!(go_default_name("fmt"), "fmt");
        assert_eq!(go_default_name("gopkg.in/yaml.v3"), "yaml");
    }

    #[test]
    fn paths() {
        assert_eq!(norm_rel("a/b/../c/./d"), "a/c/d");
        assert_eq!(strip_ext("a/b.ts"), "a/b");
        assert_eq!(strip_ext("a.b/c"), "a.b/c");
        assert_eq!(stem_of("x/y/foo.hpp"), "foo");
    }
}

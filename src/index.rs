//! Indexing: discover repos under the configured roots, detect changed files
//! (mtime/size, then content hash), parse them in parallel, write the graph, and
//! re-resolve every repo that changed.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use rayon::prelude::*;
use serde::Serialize;

use crate::config::Config;
use crate::error::{Error, Result};
use crate::extract::{self, FileFacts};
use crate::lang::Lang;
use crate::layout::Layout;
use crate::resolve::{self, ResolveStats};
use crate::store::{self, NewFile, Store};
use crate::util;

/// Options for one indexing run.
#[derive(Debug, Clone, Default)]
pub struct IndexOptions {
    /// Re-parse everything, ignoring hashes.
    pub full: bool,
    /// Stop parsing after this long (remaining files are picked up next time).
    pub budget: Option<Duration>,
}

/// Per-repo summary.
#[derive(Debug, Clone, Serialize)]
pub struct RepoStats {
    /// Repo name.
    pub name: String,
    /// Root.
    pub root: String,
    /// Source files seen.
    pub files: usize,
    /// Files (re)parsed.
    pub parsed: usize,
    /// Files removed from the graph.
    pub removed: usize,
    /// Resolution counters (when re-resolved).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolve: Option<ResolveStats>,
}

/// Summary of an indexing run.
#[derive(Debug, Clone, Default, Serialize)]
pub struct IndexStats {
    /// Repos seen.
    pub repos: usize,
    /// Source files seen.
    pub files: usize,
    /// Files (re)parsed.
    pub parsed: usize,
    /// Files whose content was unchanged.
    pub unchanged: usize,
    /// Files removed (deleted on disk or now ignored).
    pub removed: usize,
    /// Files skipped for size.
    pub skipped_large: usize,
    /// Files parsed with syntax errors (still indexed).
    pub parse_errors: usize,
    /// Files left for next time because the budget ran out.
    pub pending: usize,
    /// Symbols in the graph after the run.
    pub symbols: i64,
    /// Non-import edges in the graph after the run.
    pub edges: i64,
    /// Of which resolved to a symbol.
    pub resolved_edges: i64,
    /// Wall time.
    pub elapsed_ms: u128,
    /// Time spent reading + parsing (wall, parallel).
    pub parse_ms: u128,
    /// Time spent writing the graph.
    pub write_ms: u128,
    /// Time spent resolving edges.
    pub resolve_ms: u128,
    /// Per-repo details (only repos that changed).
    pub changed_repos: Vec<RepoStats>,
}

#[derive(Debug, Clone)]
struct Found {
    abs: PathBuf,
    rel: String,
    lang: Lang,
    size: u64,
    mtime: i64,
}

enum Outcome {
    Unchanged,
    Touch { id: i64, mtime: i64, size: i64 },
    Parsed(Box<(FileFacts, String)>),
    Pending,
    Failed,
}

/// Find the repo root for a path: nearest ancestor with `.git`, stopping at `stop`.
pub fn repo_root_of(path: &Path, stop: Option<&Path>) -> Option<PathBuf> {
    let mut cur = if path.is_dir() {
        Some(path)
    } else {
        path.parent()
    };
    while let Some(d) = cur {
        if d.join(".git").exists() {
            return Some(d.to_path_buf());
        }
        if stop.is_some_and(|s| util::rel_under(d, s).is_some()) {
            break;
        }
        cur = d.parent();
    }
    None
}

fn repo_name(root_of_index: &Path, repo: &Path) -> String {
    match util::rel_under(root_of_index, repo) {
        Some(r) if !r.is_empty() => r,
        _ => repo
            .file_name()
            .map_or_else(|| util::slash(repo), |n| n.to_string_lossy().to_string()),
    }
}

/// Walk `root` (gitignore-aware) and return supported source files.
fn walk(root: &Path, cfg: &Config) -> Vec<(PathBuf, u64, i64)> {
    let skip: HashSet<String> = cfg.skip_dirs.iter().map(|s| s.to_lowercase()).collect();
    let langs: HashSet<Lang> = cfg.languages.iter().copied().collect();
    let out = Mutex::new(Vec::new());
    let walker = ignore::WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .git_global(false)
        .git_exclude(true)
        .require_git(false)
        .follow_links(false)
        .filter_entry(move |e| {
            if e.file_type().is_some_and(|t| t.is_dir()) && e.depth() > 0 {
                let n = e.file_name().to_string_lossy().to_lowercase();
                return !skip.contains(&n);
            }
            true
        })
        .build_parallel();
    walker.run(|| {
        let out = &out;
        let langs = &langs;
        Box::new(move |entry| {
            let Ok(entry) = entry else {
                return ignore::WalkState::Continue;
            };
            if !entry.file_type().is_some_and(|t| t.is_file()) {
                return ignore::WalkState::Continue;
            }
            let path = entry.path();
            if path
                .file_name()
                .is_some_and(|n| util::is_secret_file(&n.to_string_lossy()))
            {
                return ignore::WalkState::Continue;
            }
            let Some(lang) = Lang::from_path(path) else {
                return ignore::WalkState::Continue;
            };
            // `.h` may be upgraded to C++ later; filter on both.
            if !(langs.contains(&lang) || (lang == Lang::C && langs.contains(&Lang::Cpp))) {
                return ignore::WalkState::Continue;
            }
            if let Ok(meta) = entry.metadata() {
                if let Ok(mut v) = out.lock() {
                    v.push((path.to_path_buf(), meta.len(), util::mtime_ns(&meta)));
                }
            }
            ignore::WalkState::Continue
        })
    });
    let mut v = out.into_inner().unwrap_or_default();
    v.sort();
    v
}

/// Group walked files into repos.
fn group_repos(
    root: &Path,
    files: Vec<(PathBuf, u64, i64)>,
) -> HashMap<PathBuf, Vec<(PathBuf, u64, i64)>> {
    let mut memo: HashMap<PathBuf, PathBuf> = HashMap::new();
    let mut out: HashMap<PathBuf, Vec<(PathBuf, u64, i64)>> = HashMap::new();
    for f in files {
        let dir = f.0.parent().map(Path::to_path_buf).unwrap_or_default();
        let repo = if let Some(r) = memo.get(&dir) {
            r.clone()
        } else {
            let r = find_repo(&dir, root, &mut memo);
            memo.insert(dir.clone(), r.clone());
            r
        };
        out.entry(repo).or_default().push(f);
    }
    out
}

fn find_repo(dir: &Path, root: &Path, memo: &mut HashMap<PathBuf, PathBuf>) -> PathBuf {
    if let Some(r) = memo.get(dir) {
        return r.clone();
    }
    if dir.join(".git").exists() {
        return dir.to_path_buf();
    }
    let at_root = util::rel_under(root, dir).is_none_or(|r| r.is_empty());
    if at_root {
        return root.to_path_buf();
    }
    let r = match dir.parent() {
        Some(p) => find_repo(p, root, memo),
        None => root.to_path_buf(),
    };
    memo.insert(dir.to_path_buf(), r.clone());
    r
}

/// `.h` files in directories with C++ sources (and no C sources) are C++.
fn classify(repo: &Path, files: Vec<(PathBuf, u64, i64)>, cfg: &Config) -> Vec<Found> {
    let mut cpp_dirs: HashSet<PathBuf> = HashSet::new();
    let mut c_dirs: HashSet<PathBuf> = HashSet::new();
    for (p, ..) in &files {
        let ext = p
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        let dir = p.parent().map(Path::to_path_buf).unwrap_or_default();
        match Lang::from_path(p) {
            Some(Lang::Cpp) => {
                cpp_dirs.insert(dir);
            }
            Some(Lang::C) if ext == "c" => {
                c_dirs.insert(dir);
            }
            _ => {}
        }
    }
    let langs: HashSet<Lang> = cfg.languages.iter().copied().collect();
    let max = cfg.max_file_bytes();
    files
        .into_iter()
        .filter_map(|(abs, size, mtime)| {
            let mut lang = Lang::from_path(&abs)?;
            if lang == Lang::C && abs.extension().is_some_and(|e| e.eq_ignore_ascii_case("h")) {
                let dir = abs.parent().map(Path::to_path_buf).unwrap_or_default();
                if cpp_dirs.contains(&dir) && !c_dirs.contains(&dir) {
                    lang = Lang::Cpp;
                }
            }
            if !langs.contains(&lang) {
                return None;
            }
            let rel = util::rel_under(repo, &abs)?;
            if size > max {
                return Some(Found {
                    abs,
                    rel,
                    lang,
                    size: u64::MAX,
                    mtime,
                });
            }
            Some(Found {
                abs,
                rel,
                lang,
                size,
                mtime,
            })
        })
        .collect()
}

fn hash_hex(bytes: &[u8]) -> String {
    format!("{:016x}", xxhash_rust::xxh3::xxh3_64(bytes))
}

/// The content hash stored in `files.hash` (xxh3-64, hex), for callers that need to
/// check a file on disk is still the one that was indexed.
pub(crate) fn content_hash(bytes: &[u8]) -> String {
    hash_hex(bytes)
}

/// Index every configured root.
pub fn index_all(store: &mut Store, cfg: &Config, opts: &IndexOptions) -> Result<IndexStats> {
    let roots: Vec<PathBuf> = cfg.roots.clone();
    index_roots(store, cfg, &roots, opts)
}

/// Index the given roots (each is walked; git repos below become graph repos).
pub fn index_roots(
    store: &mut Store,
    cfg: &Config,
    roots: &[PathBuf],
    opts: &IndexOptions,
) -> Result<IndexStats> {
    let start = Instant::now();
    let mut stats = IndexStats::default();
    let mut seen_repos: HashSet<PathBuf> = HashSet::new();
    // Cold or full runs rewrite (nearly) everything: build the search index once at the
    // end instead of row by row.
    let empty = store
        .conn()
        .query_row("SELECT count(*) FROM files", [], |r| r.get::<_, i64>(0))?
        == 0;
    let bulk = opts.full || empty;
    if bulk {
        store.suspend_fts(empty)?;
    } else {
        store.recover_fts(true)?;
    }
    let result = index_roots_inner(store, cfg, roots, opts, start, &mut stats, &mut seen_repos);
    if bulk {
        // Keep the indexing error if both fail; a failed rebuild stays marked dirty and is
        // repaired by the next writer.
        let t0 = Instant::now();
        let resumed = store.resume_fts();
        util::trace("index.rebuild_indexes", t0);
        result?;
        resumed?;
    } else {
        result?;
    }
    finish(store, &mut stats, start)?;
    Ok(stats)
}

/// The path a directory is compared by: canonical (links resolved, case as on disk)
/// when it exists, else absolute.
fn canonical_or_absolute(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).map_or_else(|_| util::absolute(p), util::strip_verbatim)
}

/// Roots to walk, in the given order, with duplicates and roots nested inside another
/// root dropped: the outer walk already covers them, and walking both would index each
/// nested repo twice with two different names. Returns the kept roots (absolute) and
/// one warning line when anything was dropped.
fn plan_roots(roots: &[PathBuf]) -> Result<(Vec<PathBuf>, Option<String>)> {
    let mut abs = Vec::with_capacity(roots.len());
    for root in roots {
        let root = util::absolute(root);
        if !root.is_dir() {
            return Err(Error::NotFound(format!(
                "index root {} does not exist",
                root.display()
            )));
        }
        let canon = canonical_or_absolute(&root);
        abs.push((root, canon));
    }
    let mut kept: Vec<PathBuf> = Vec::new();
    let mut dropped: Vec<String> = Vec::new();
    for (i, (root, canon)) in abs.iter().enumerate() {
        // Nested inside another root (which is kept whatever its position), or a
        // repeat of an earlier one.
        let outer = abs.iter().enumerate().find(|(j, (_, other))| {
            *j != i
                && match util::rel_under(other, canon) {
                    Some(rel) if rel.is_empty() => *j < i,
                    Some(_) => true,
                    None => false,
                }
        });
        if let Some((_, (outer_root, _))) = outer {
            dropped.push(format!(
                "{} is inside {}",
                util::slash(root),
                util::slash(outer_root)
            ));
        } else {
            kept.push(root.clone());
        }
    }
    let warning = (!dropped.is_empty()).then(|| {
        format!(
            "index roots overlap ({}): nested repos are indexed once, named relative to the outermost root",
            dropped.join("; ")
        )
    });
    Ok((kept, warning))
}

fn index_roots_inner(
    store: &mut Store,
    cfg: &Config,
    roots: &[PathBuf],
    opts: &IndexOptions,
    start: Instant,
    stats: &mut IndexStats,
    seen_repos: &mut HashSet<PathBuf>,
) -> Result<()> {
    let (roots, warning) = plan_roots(roots)?;
    if let Some(w) = warning {
        eprintln!("codegraph: warning: {w}");
    }
    // Repos already indexed this run, by canonical path: two roots can still reach one
    // repo through a link.
    let mut seen_canon: HashSet<PathBuf> = HashSet::new();
    for root in &roots {
        let t0 = Instant::now();
        let files = walk(root, cfg);
        util::trace("index.walk", t0);
        let groups = group_repos(root, files);
        util::trace("index.group_repos", t0);
        let mut repos: Vec<_> = groups.into_iter().collect();
        repos.sort_by(|a, b| a.0.cmp(&b.0));
        for (repo_root, files) in repos {
            if !seen_canon.insert(canonical_or_absolute(&repo_root)) {
                continue;
            }
            seen_repos.insert(repo_root.clone());
            let name = repo_name(root, &repo_root);
            index_repo(
                store, cfg, &name, &repo_root, files, opts, start, stats, None,
            )?;
        }
        // Repos under this root that no longer have any source files.
        for r in store.repos()? {
            if util::rel_under(root, &r.root).is_some() && !seen_repos.contains(&r.root) {
                let n = store.conn().query_row(
                    "SELECT count(*) FROM files WHERE repo_id = ?1",
                    [r.id],
                    |row| row.get::<_, i64>(0),
                )?;
                stats.removed += n as usize;
                store.delete_repo(r.id)?;
            }
        }
    }
    Ok(())
}

/// Refresh one repo (MCP auto-refresh / on-demand indexing of the cwd repo).
pub fn refresh_repo(
    store: &mut Store,
    cfg: &Config,
    repo_root: &Path,
    name: &str,
    opts: &IndexOptions,
) -> Result<IndexStats> {
    refresh_repo_inner(store, cfg, repo_root, name, opts, None)
}

/// [`refresh_repo`] for long-running callers (`watch`, the MCP server): `cache` keeps a
/// big repo's resolution state in memory between refreshes, so an edit re-resolves
/// without reloading the whole repo.
pub fn refresh_repo_cached(
    store: &mut Store,
    cfg: &Config,
    repo_root: &Path,
    name: &str,
    opts: &IndexOptions,
    cache: &mut resolve::ResolveCache,
) -> Result<IndexStats> {
    refresh_repo_inner(store, cfg, repo_root, name, opts, Some(cache))
}

fn refresh_repo_inner(
    store: &mut Store,
    cfg: &Config,
    repo_root: &Path,
    name: &str,
    opts: &IndexOptions,
    cache: Option<&mut resolve::ResolveCache>,
) -> Result<IndexStats> {
    let start = Instant::now();
    let mut stats = IndexStats::default();
    store.recover_fts(false)?;
    let files = walk(repo_root, cfg);
    // Drop files that belong to nested repos.
    let mut memo = HashMap::new();
    let files: Vec<_> = files
        .into_iter()
        .filter(|(p, ..)| {
            let dir = p.parent().map(Path::to_path_buf).unwrap_or_default();
            find_repo(&dir, repo_root, &mut memo) == repo_root
        })
        .collect();
    index_repo(
        store, cfg, name, repo_root, files, opts, start, &mut stats, cache,
    )?;
    // Refreshes run per MCP call and per watch batch: count this repo only, from the
    // counts its last resolve stored.
    stats.symbols = store
        .repos()?
        .iter()
        .find(|r| r.root == repo_root)
        .map_or(0, |r| r.counts[0]);
    if let Some(res) = stats.changed_repos.last().and_then(|r| r.resolve) {
        stats.edges = res.edges as i64;
        stats.resolved_edges = res.resolved as i64;
    }
    stats.elapsed_ms = start.elapsed().as_millis();
    Ok(stats)
}

fn finish(store: &Store, stats: &mut IndexStats, start: Instant) -> Result<()> {
    // Per-repo counts written by each resolve: no scan of millions of edges.
    let (mut symbols, mut edges, mut resolved) = (0, 0, 0);
    for r in store.repos()? {
        symbols += r.counts[0];
        edges += r.counts[1];
        resolved += r.counts[2];
    }
    stats.symbols = symbols;
    stats.edges = edges;
    stats.resolved_edges = resolved;
    stats.elapsed_ms = start.elapsed().as_millis();
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn index_repo(
    store: &mut Store,
    cfg: &Config,
    name: &str,
    repo_root: &Path,
    files: Vec<(PathBuf, u64, i64)>,
    opts: &IndexOptions,
    start: Instant,
    stats: &mut IndexStats,
    cache: Option<&mut resolve::ResolveCache>,
) -> Result<()> {
    let t0 = Instant::now();
    let found = classify(repo_root, files, cfg);
    util::trace("index.classify", t0);
    let repo_id = store.upsert_repo(name, repo_root)?;
    let mut existing = store.repo_files(repo_id)?;
    util::trace("index.repo_files", t0);
    // The last run of this repo died between its write commit and its resolve
    // (`indexed_at` is reset inside the write transaction and set after the resolve):
    // its edges are unresolved or dangling, or, after a cold load, were never inserted.
    // With edges stored a whole-repo resolve repairs it; without any, the stored files
    // are dropped and the repo is loaded cold again (a budgeted run then converges, the
    // files it did not reach are simply new next time).
    let mut repair_resolve = false;
    let mut wipe = false;
    if !existing.is_empty() && store.repo_indexed_at(repo_id)? == 0 {
        if store.repo_has_edges(repo_id)? {
            repair_resolve = true;
        } else {
            wipe = true;
            existing.clear();
        }
    }
    stats.repos += 1;
    stats.files += found.len();

    let t_parse = Instant::now();
    let parse_one = |f: &Found| -> Outcome {
        if f.size == u64::MAX {
            return Outcome::Failed;
        }
        let old = existing.get(&f.rel);
        if !opts.full {
            if let Some(o) = old {
                if o.mtime == f.mtime && o.size == f.size as i64 {
                    return Outcome::Unchanged;
                }
            }
        }
        if opts.budget.is_some_and(|b| start.elapsed() > b) {
            return Outcome::Pending;
        }
        let Ok(bytes) = std::fs::read(&f.abs) else {
            return Outcome::Failed;
        };
        let hash = hash_hex(&bytes);
        if !opts.full {
            if let Some(o) = old {
                if o.hash == hash {
                    return Outcome::Touch {
                        id: o.id,
                        mtime: f.mtime,
                        size: f.size as i64,
                    };
                }
            }
        }
        match extract::extract(f.lang, &bytes) {
            Ok(facts) => Outcome::Parsed(Box::new((facts, hash))),
            Err(_) => Outcome::Failed,
        }
    };

    let mut layout = Layout::new(repo_root);
    let mut rs = RepoStats {
        name: name.to_string(),
        root: util::slash(repo_root),
        files: found.len(),
        parsed: 0,
        removed: 0,
        resolve: None,
    };
    let present: HashSet<&str> = found.iter().map(|f| f.rel.as_str()).collect();
    // A repo with no stored files (cold index, new repo): keep its references in memory,
    // resolve them, and insert each edge once with its target.
    let mut pending: Option<Vec<resolve::Pending>> = existing.is_empty().then(Vec::new);
    // For the resolve cache: (old id, new id) of re-parsed files, and whether files were
    // added or removed.
    let mut changes = Changes::default();
    // Parse in parallel and write as results arrive: the single SQLite writer overlaps
    // the parsers, and a bounded channel caps how many parsed files sit in memory.
    let (sender, receiver) = std::sync::mpsc::sync_channel::<(usize, Outcome)>(512);
    let mut parse_wall = 0u128;
    let tx = store.conn_mut().transaction()?;
    store::begin_repo_write(&tx, repo_id, wipe)?;
    let write_result: Result<()> = std::thread::scope(|scope| {
        let producer = scope.spawn(|| {
            found
                .par_iter()
                .enumerate()
                .for_each_with(sender, |s, (i, f)| {
                    // A closed channel means the writer failed; stop quietly.
                    let _ = s.send((i, parse_one(f)));
                });
            t_parse.elapsed().as_millis()
        });
        let mut res = Ok(());
        for (i, o) in receiver {
            if res.is_err() {
                continue;
            }
            res = write_outcome(
                &tx,
                repo_id,
                &found[i],
                o,
                &existing,
                &mut layout,
                &mut rs,
                stats,
                pending.as_mut(),
                &mut changes,
            );
        }
        parse_wall = producer.join().unwrap_or_default();
        res
    });
    write_result?;
    stats.parse_ms += parse_wall;
    // Writing overlaps parsing: count the time the writer ran past the parsers, plus the
    // deletions and the commit below.
    let tail = t_parse.elapsed().as_millis().saturating_sub(parse_wall);
    let t_write = Instant::now();
    for (rel, row) in &existing {
        if !present.contains(rel.as_str()) {
            store::delete_file(&tx, row.id)?;
            rs.removed += 1;
            changes.structural = true;
        }
    }
    tx.commit()?;
    util::trace("index.parse_and_write", t0);
    stats.write_ms += tail + t_write.elapsed().as_millis();
    stats.parsed += rs.parsed;
    stats.removed += rs.removed;
    if rs.parsed > 0 || rs.removed > 0 || opts.full || repair_resolve {
        let t_res = Instant::now();
        rs.resolve = Some(match (pending, cache) {
            (Some(p), cache) => {
                if let Some(c) = cache {
                    c.clear();
                }
                resolve::resolve_and_insert(store, repo_id, repo_root, p)?
            }
            (None, Some(c)) => resolve::resolve_with_cache(
                store,
                repo_id,
                repo_root,
                c,
                &changes.reparsed,
                changes.structural || opts.full || repair_resolve,
            )?,
            (None, None) => resolve::resolve_repo(store, repo_id, repo_root)?,
        });
        stats.resolve_ms += t_res.elapsed().as_millis();
        stats.changed_repos.push(rs);
    }
    store.set_indexed(repo_id, util::now_unix())?;
    Ok(())
}

/// Apply one parse outcome inside the repo's write transaction.
/// Files an incremental index changed, for the resolve cache.
#[derive(Default)]
struct Changes {
    /// (old file id, new file id) of re-parsed files.
    reparsed: Vec<(i64, i64)>,
    /// Files were added or removed.
    structural: bool,
}

#[allow(clippy::too_many_arguments)]
fn write_outcome(
    tx: &rusqlite::Connection,
    repo_id: i64,
    f: &Found,
    o: Outcome,
    existing: &HashMap<String, store::FileRow>,
    layout: &mut Layout,
    rs: &mut RepoStats,
    stats: &mut IndexStats,
    pending: Option<&mut Vec<resolve::Pending>>,
    changes: &mut Changes,
) -> Result<()> {
    match o {
        Outcome::Unchanged => stats.unchanged += 1,
        Outcome::Touch { id, mtime, size } => {
            stats.unchanged += 1;
            store::touch_file(tx, id, mtime, size)?;
        }
        Outcome::Pending => stats.pending += 1,
        Outcome::Failed => {
            if f.size == u64::MAX {
                stats.skipped_large += 1;
            }
            if let Some(o) = existing.get(&f.rel) {
                store::delete_file(tx, o.id)?;
                rs.removed += 1;
                changes.structural = true;
            }
        }
        Outcome::Parsed(b) => {
            let (mut facts, hash) = *b;
            if facts.has_errors {
                stats.parse_errors += 1;
            }
            let unit = layout.unit(&f.rel, f.lang, facts.package.as_deref());
            let (file_id, sym_ids) = store::replace_file(
                tx,
                repo_id,
                &NewFile {
                    path: &f.rel,
                    lang: f.lang,
                    hash: &hash,
                    mtime: f.mtime,
                    size: f.size as i64,
                    unit: &unit,
                    facts: &facts,
                },
                pending.is_none(),
            )?;
            match existing.get(&f.rel) {
                Some(o) => changes.reparsed.push((o.id, file_id)),
                None => changes.structural = true,
            }
            if let Some(p) = pending {
                p.push(resolve::Pending {
                    file_id,
                    sym_ids,
                    refs: std::mem::take(&mut facts.refs),
                    imports: std::mem::take(&mut facts.imports),
                });
            }
            rs.parsed += 1;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_detection() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("a/.git")).unwrap();
        std::fs::create_dir_all(root.join("a/src/deep")).unwrap();
        std::fs::create_dir_all(root.join("loose")).unwrap();
        let mut memo = HashMap::new();
        assert_eq!(
            find_repo(&root.join("a/src/deep"), root, &mut memo),
            root.join("a")
        );
        assert_eq!(find_repo(&root.join("loose"), root, &mut memo), root);
        assert_eq!(repo_name(root, &root.join("a")), "a");
    }

    #[test]
    fn overlapping_roots_index_nested_repos_once() {
        let dir = tempfile::tempdir().unwrap();
        let outer = dir.path().join("dev");
        let inner = outer.join("go");
        let repo = inner.join("pulse");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::write(repo.join("p.go"), "package pulse\nfunc Beat() {}\n").unwrap();

        // The planner keeps the outermost root whatever the order, drops the nested one
        // and repeats, and warns once.
        let (kept, warn) = plan_roots(&[inner.clone(), outer.clone(), inner.clone()]).unwrap();
        assert_eq!(kept, [util::absolute(&outer)]);
        let warn = warn.unwrap();
        assert!(warn.starts_with("index roots overlap ("), "{warn}");
        assert!(warn.contains("is inside"), "{warn}");
        let (kept, warn) = plan_roots(std::slice::from_ref(&outer)).unwrap();
        assert_eq!(kept, [util::absolute(&outer)]);
        assert!(warn.is_none());
        assert!(plan_roots(&[dir.path().join("missing")]).is_err());

        let mut store = Store::open(&dir.path().join("g.db")).unwrap();
        let cfg = Config {
            roots: Vec::new(),
            ..Config::default()
        };
        let repos = |store: &Store| -> Vec<(String, PathBuf)> {
            store
                .repos()
                .unwrap()
                .into_iter()
                .map(|r| (r.name, r.root))
                .collect()
        };
        let expected = vec![("go/pulse".to_string(), util::absolute(&repo))];
        // Outer first, inner first, and a second run: one repo, one name.
        for roots in [
            vec![outer.clone(), inner.clone()],
            vec![inner.clone(), outer.clone()],
            vec![outer.clone(), inner.clone()],
        ] {
            let st = index_roots(&mut store, &cfg, &roots, &IndexOptions::default()).unwrap();
            assert_eq!(st.repos, 1, "{roots:?}");
            assert_eq!(st.files, 1, "{roots:?}");
            assert_eq!(repos(&store), expected, "{roots:?}");
        }
        let st = index_roots(&mut store, &cfg, &[outer], &IndexOptions::default()).unwrap();
        assert_eq!((st.parsed, st.unchanged), (0, 1));
    }
}

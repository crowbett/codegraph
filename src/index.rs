//! Indexing: discover repos under the configured roots, detect changed files
//! (mtime/size, then content hash), parse them in parallel, write the graph, and
//! re-resolve every repo that changed.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use rayon::prelude::*;
use serde::Serialize;

use crate::clock::Instant;
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
    /// Files that could not be read (kept as last indexed).
    pub failed: usize,
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
    /// Files that exist but could not be read (locked, permission denied); their last
    /// indexed state is kept.
    pub failed: usize,
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
    Touch {
        id: i64,
        mtime: i64,
        size: i64,
    },
    Parsed(Box<(FileFacts, String)>),
    Pending,
    /// Gone, over the cap, or unparseable: dropped from the graph.
    Failed,
    /// Exists but could not be read right now (locked by an editor or a scanner,
    /// permission denied): its last indexed state is kept.
    Unreadable,
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

/// The walker every index pass uses: gitignore-aware (`.gitignore`, `.ignore`,
/// `.git/info/exclude`, no global excludes), hidden entries skipped, links not
/// followed, `skip_dirs` pruned.
fn walk_builder(root: &Path, cfg: &Config, only: Option<PathBuf>) -> ignore::WalkBuilder {
    let skip: HashSet<String> = cfg.skip_dirs.iter().map(|s| s.to_lowercase()).collect();
    let mut b = ignore::WalkBuilder::new(root);
    b.hidden(true)
        .git_ignore(true)
        .git_global(false)
        .git_exclude(true)
        .require_git(false)
        .follow_links(false)
        // One filter: `filter_entry` replaces rather than chains.
        .filter_entry(move |e| {
            if only
                .as_deref()
                .is_some_and(|t| util::rel_under(e.path(), t).is_none())
            {
                return false;
            }
            if e.file_type().is_some_and(|t| t.is_dir()) && e.depth() > 0 {
                let n = e.file_name().to_string_lossy().to_lowercase();
                return !skip.contains(&n);
            }
            true
        });
    b
}

/// Whether a source file is one the walker yields: a regular file under `repo_root`
/// that an index pass would include (not gitignored, hidden, under a `skip_dirs`
/// directory, a link, a secret file, or an unsupported or disabled language).
pub fn walker_admits(repo_root: &Path, path: &Path, cfg: &Config) -> bool {
    let Some(rel) = util::rel_under(repo_root, path).filter(|r| !r.is_empty()) else {
        return false;
    };
    if path
        .file_name()
        .is_some_and(|n| util::is_secret_file(&n.to_string_lossy()))
    {
        return false;
    }
    let langs: HashSet<Lang> = cfg.languages.iter().copied().collect();
    let lang_ok = Lang::from_path(path)
        .is_some_and(|l| langs.contains(&l) || (l == Lang::C && langs.contains(&Lang::Cpp)));
    if !lang_ok {
        return false;
    }
    // Walk only the chain of directories leading to `path`; the walker applies the
    // ignore rules exactly as an index pass does.
    let target = repo_root.join(rel);
    walk_builder(repo_root, cfg, Some(target))
        .build()
        .flatten()
        .any(|e| {
            e.file_type().is_some_and(|t| t.is_file())
                && util::rel_under(e.path(), path) == Some(String::new())
        })
}

/// A walked file: path, size, mtime (ns).
type Entry = (PathBuf, u64, i64);

/// What a walk found: source files of enabled languages, and the module-layout marker
/// files ([`crate::layout::MARKER_FILES`]) whose change means a repo must be re-unitized.
#[derive(Default)]
struct Walked {
    files: Vec<Entry>,
    markers: Vec<Entry>,
}

/// Walk `root` (gitignore-aware) and return supported source files and marker files.
fn walk(root: &Path, cfg: &Config) -> Walked {
    let langs: HashSet<Lang> = cfg.languages.iter().copied().collect();
    let out = Mutex::new(Walked::default());
    let walker = walk_builder(root, cfg, None).build_parallel();
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
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            if util::is_secret_file(&name) {
                return ignore::WalkState::Continue;
            }
            if crate::layout::is_marker_file(&name) {
                if let Ok(meta) = entry.metadata() {
                    if let Ok(mut w) = out.lock() {
                        w.markers
                            .push((path.to_path_buf(), meta.len(), util::mtime_ns(&meta)));
                    }
                }
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
                if let Ok(mut w) = out.lock() {
                    w.files
                        .push((path.to_path_buf(), meta.len(), util::mtime_ns(&meta)));
                }
            }
            ignore::WalkState::Continue
        })
    });
    let mut w = out.into_inner().unwrap_or_default();
    w.files.sort();
    w.markers.sort();
    w
}

/// Fingerprint of a repo's marker files: repo-relative path plus a hash of the content
/// (they are tiny, and a checkout or stash that rewrites `go.mod` with the same bytes
/// must not cost a whole-repo pass). A marker that cannot be read (a link, oversize)
/// counts by size only.
fn markers_signature(repo_root: &Path, markers: &[Entry]) -> String {
    let mut lines: Vec<String> = markers
        .iter()
        .filter_map(|(p, size, _)| {
            let rel = util::rel_under(repo_root, p)?;
            let content = util::read_repo_bytes(repo_root, p, util::MAX_REPO_FILE_BYTES)
                .map_or_else(|| format!("?{size}"), |b| hash_hex(&b));
            Some(format!("{rel}\t{content}"))
        })
        .collect();
    lines.sort();
    hash_hex(lines.join("\n").as_bytes())
}

/// Group walked files into repos.
fn group_repos(root: &Path, files: Vec<Entry>) -> HashMap<PathBuf, Vec<Entry>> {
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
        let walked = walk(root, cfg);
        util::trace("index.walk", t0);
        let groups = group_repos(root, walked.files);
        let mut marker_groups = group_repos(root, walked.markers);
        util::trace("index.group_repos", t0);
        let mut repos: Vec<_> = groups.into_iter().collect();
        repos.sort_by(|a, b| a.0.cmp(&b.0));
        for (repo_root, files) in repos {
            if !seen_canon.insert(canonical_or_absolute(&repo_root)) {
                continue;
            }
            seen_repos.insert(repo_root.clone());
            let name = repo_name(root, &repo_root);
            let markers = marker_groups.remove(&repo_root).unwrap_or_default();
            index_repo(
                store, cfg, &name, &repo_root, files, &markers, opts, start, stats, None,
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
    let walked = walk(repo_root, cfg);
    // Drop files that belong to nested repos.
    let mut memo = HashMap::new();
    let mut own = |v: Vec<Entry>| -> Vec<Entry> {
        v.into_iter()
            .filter(|(p, ..)| {
                let dir = p.parent().map(Path::to_path_buf).unwrap_or_default();
                find_repo(&dir, repo_root, &mut memo) == repo_root
            })
            .collect()
    };
    let files = own(walked.files);
    let markers = own(walked.markers);
    index_repo(
        store, cfg, name, repo_root, files, &markers, opts, start, &mut stats, cache,
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
    files: Vec<Entry>,
    markers: &[Entry],
    opts: &IndexOptions,
    start: Instant,
    stats: &mut IndexStats,
    cache: Option<&mut resolve::ResolveCache>,
) -> Result<()> {
    let t0 = Instant::now();
    let found = classify(repo_root, files, cfg);
    let marker_sig = markers_signature(repo_root, markers);
    util::trace("index.classify", t0);
    // The write lock is taken up front (IMMEDIATE), so the state read below cannot
    // change under us before our own `begin_repo_write`.
    let now = util::now_unix();
    let tx = store
        .conn_mut()
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let repo_id = store::upsert_repo(&tx, name, repo_root)?;
    let mut existing = store::repo_files(&tx, repo_id)?;
    util::trace("index.repo_files", t0);
    // The last run of this repo died between its write commit and its resolve
    // (`indexed_at` is reset inside the write transaction and set after the resolve;
    // a recent reset is another connection still at work, see `repo_write_is_dead`):
    // its edges are unresolved or dangling, or, after a cold load, were never inserted.
    // With edges stored a whole-repo resolve repairs it; without any, the stored files
    // are dropped and the repo is loaded cold again (a budgeted run then converges, the
    // files it did not reach are simply new next time).
    let mut repair_resolve = resolve_failed_before(repo_root);
    let mut wipe = false;
    if !existing.is_empty() && store::repo_write_is_dead(&tx, repo_id, now)? {
        if store::repo_has_edges(&tx, repo_id)? {
            repair_resolve = true;
        } else {
            wipe = true;
            existing.clear();
        }
    }
    // A changed go.mod/Cargo.toml/package.json/tsconfig.json changes the unit (module
    // path) of files that did not change themselves. Rather than forcing one full pass
    // (which a budgeted MCP refresh could not finish, leaving the rest stale once the
    // new fingerprint is stored), every stored row is invalidated: the normal
    // incremental path then re-parses them, over as many passes as the budget needs.
    let markers_changed = store::repo_markers(&tx, repo_id)? != marker_sig;
    let full = opts.full;
    if markers_changed && !existing.is_empty() {
        eprintln!(
            "codegraph: {name}: module layout files changed (go.mod, Cargo.toml, package.json, tsconfig.json); re-indexing the repo"
        );
        store::invalidate_repo_files(&tx, repo_id)?;
        for row in existing.values_mut() {
            row.mtime = -1;
            row.hash.clear();
        }
    }
    if markers_changed {
        store::set_repo_markers(&tx, repo_id, &marker_sig)?;
    }
    stats.repos += 1;
    stats.files += found.len();

    let t_parse = Instant::now();
    let max_bytes = cfg.max_file_bytes();
    let parse_one = |f: &Found| -> Outcome {
        if f.size == u64::MAX {
            return Outcome::Failed;
        }
        let old = existing.get(&f.rel);
        if !full {
            if let Some(o) = old {
                if o.mtime == f.mtime && o.size == f.size as i64 {
                    return Outcome::Unchanged;
                }
            }
        }
        if opts.budget.is_some_and(|b| start.elapsed() > b) {
            return Outcome::Pending;
        }
        // Bounded by the cap, checked on the open handle: the size the walk saw is
        // advisory (the file may have grown or been swapped since).
        let bytes = match util::read_bounded(&f.abs, max_bytes) {
            Ok(b) => b,
            // A sharing violation (an editor or Defender holding the file) clears in
            // seconds: keep what we have. Anything else (gone, too big, permission
            // denied, not a regular file) is not going to change by itself: drop it.
            Err(e) if is_sharing_violation(&e) => return Outcome::Unreadable,
            Err(_) => return Outcome::Failed,
        };
        let hash = hash_hex(&bytes);
        if !full {
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
        parse(f, &bytes, hash)
    };

    let mut layout = Layout::new(repo_root);
    let mut rs = RepoStats {
        name: name.to_string(),
        root: util::slash(repo_root),
        files: found.len(),
        parsed: 0,
        removed: 0,
        failed: 0,
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
    store::begin_repo_write(&tx, repo_id, wipe, now)?;
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
        // The producer only dies on a panic that escaped `parse_one` (rayon itself, or
        // the channel): files it never sent are not in the graph, so fail loudly
        // instead of committing a silently partial repo.
        match producer.join() {
            Ok(ms) => parse_wall = ms,
            Err(_) => {
                if res.is_ok() {
                    res = Err(Error::Internal("parser thread panicked".into()));
                }
            }
        }
        res
    });
    write_result?;
    if rs.failed > 0 {
        eprintln!(
            "codegraph: {name}: {} file(s) could not be read (locked or permission denied); kept as last indexed",
            rs.failed
        );
    }
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
    // Nothing changed in a known repo: roll back the bookkeeping writes
    // (`indexed_at`/`write_started`) instead of committing them. A commit would bump
    // `data_version` and make every other connection (the `watch` resolve cache) drop
    // state for no reason.
    let noop = pending.is_none()
        && rs.parsed == 0
        && rs.removed == 0
        && !changes.touched
        && !repair_resolve
        && !wipe
        && !markers_changed
        && !full;
    if noop {
        tx.rollback()?;
        util::trace("index.parse_and_write", t0);
        stats.write_ms += tail + t_write.elapsed().as_millis();
        return Ok(());
    }
    // Dates the commit-to-resolve window (a cold index may parse for longer than the
    // dead-run threshold; that time must not count).
    store::restamp_write_started(&tx, repo_id, util::now_unix())?;
    tx.commit()?;
    util::trace("index.parse_and_write", t0);
    stats.write_ms += tail + t_write.elapsed().as_millis();
    stats.parsed += rs.parsed;
    stats.removed += rs.removed;
    if rs.parsed > 0 || rs.removed > 0 || full || repair_resolve {
        let t_res = Instant::now();
        let resolved = match (pending, cache) {
            (Some(p), cache) => {
                if let Some(c) = cache {
                    c.clear();
                }
                resolve::resolve_and_insert(store, repo_id, repo_root, p)
            }
            (None, Some(c)) => resolve::resolve_with_cache(
                store,
                repo_id,
                repo_root,
                c,
                &changes.reparsed,
                changes.structural || full || repair_resolve,
            ),
            (None, None) => resolve::resolve_repo(store, repo_id, repo_root),
        };
        // The write is committed; a resolve that fails here (SQLITE_BUSY under a short
        // lock wait, say) leaves the repo unresolved with `indexed_at = 0`, which the
        // next pass would take for "in progress" and skip as a no-op. Remember it so
        // the next pass in this process repairs it.
        match resolved {
            Ok(r) => rs.resolve = Some(r),
            Err(e) => {
                note_resolve_failed(repo_root);
                return Err(e);
            }
        }
        stats.resolve_ms += t_res.elapsed().as_millis();
        stats.changed_repos.push(rs);
    }
    resolve_succeeded(repo_root);
    store.set_indexed(repo_id, util::now_unix())?;
    Ok(())
}

/// Parse one file. A parser bug on one hostile file must not take the whole run (or the
/// MCP server) down: the panic is caught here and the file is dropped from the graph.
fn parse(f: &Found, bytes: &[u8], hash: String) -> Outcome {
    let parsed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        extract::extract(f.lang, bytes)
    }));
    match parsed {
        Ok(Ok(facts)) => Outcome::Parsed(Box::new((facts, hash))),
        Ok(Err(_)) => Outcome::Failed,
        Err(_) => {
            eprintln!(
                "codegraph: parser panicked on {}; file skipped",
                util::slash(&f.abs)
            );
            Outcome::Failed
        }
    }
}

/// Index in-memory sources as one new repo named `name` at `root`, through the steps a
/// cold `index` of the same files on disk takes (classify, parse, write, resolve, search
/// index), for hosts without a filesystem (the wasm32 browser demo). `files` are
/// (repo-relative path, content). Module-layout files (`go.mod`, `Cargo.toml`,
/// `package.json`, `tsconfig.json`) are read from disk under `root` as usual, so on wasm32
/// they are absent. Fails if the graph already has a repo at `root`.
pub fn index_sources(
    store: &mut Store,
    name: &str,
    root: &Path,
    files: &[(&str, &[u8])],
) -> Result<IndexStats> {
    let start = Instant::now();
    let mut stats = IndexStats::default();
    if store.repos()?.iter().any(|r| r.root == root) {
        return Err(Error::Usage(format!(
            "{} is already indexed",
            util::slash(root)
        )));
    }
    let empty = store
        .conn()
        .query_row("SELECT count(*) FROM files", [], |r| r.get::<_, i64>(0))?
        == 0;
    store.suspend_fts(empty)?;
    let result = index_sources_inner(store, name, root, files, &mut stats);
    let resumed = store.resume_fts();
    result?;
    resumed?;
    finish(store, &mut stats, start)?;
    Ok(stats)
}

fn index_sources_inner(
    store: &mut Store,
    name: &str,
    root: &Path,
    files: &[(&str, &[u8])],
    stats: &mut IndexStats,
) -> Result<()> {
    let content: HashMap<PathBuf, &[u8]> =
        files.iter().map(|(rel, b)| (root.join(rel), *b)).collect();
    let entries: Vec<Entry> = files
        .iter()
        .map(|(rel, b)| (root.join(rel), b.len() as u64, 0))
        .collect();
    let found = classify(root, entries, &Config::default());
    let now = util::now_unix();
    let tx = store
        .conn_mut()
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let repo_id = store::upsert_repo(&tx, name, root)?;
    store::set_repo_markers(&tx, repo_id, &markers_signature(root, &[]))?;
    stats.repos += 1;
    stats.files += found.len();
    let mut layout = Layout::new(root);
    let mut rs = RepoStats {
        name: name.to_string(),
        root: util::slash(root),
        files: found.len(),
        parsed: 0,
        removed: 0,
        failed: 0,
        resolve: None,
    };
    let existing = HashMap::new();
    let mut pending = Vec::new();
    let mut changes = Changes::default();
    store::begin_repo_write(&tx, repo_id, false, now)?;
    for f in &found {
        let outcome = match content.get(&f.abs) {
            Some(b) if f.size != u64::MAX => parse(f, b, hash_hex(b)),
            _ => Outcome::Failed,
        };
        write_outcome(
            &tx,
            repo_id,
            f,
            outcome,
            &existing,
            &mut layout,
            &mut rs,
            stats,
            Some(&mut pending),
            &mut changes,
        )?;
    }
    store::restamp_write_started(&tx, repo_id, util::now_unix())?;
    tx.commit()?;
    stats.parsed += rs.parsed;
    rs.resolve = Some(resolve::resolve_and_insert(store, repo_id, root, pending)?);
    stats.changed_repos.push(rs);
    store.set_indexed(repo_id, util::now_unix())?;
    Ok(())
}

/// Windows `ERROR_SHARING_VIOLATION` (32) / `ERROR_LOCK_VIOLATION` (33): another process
/// has the file open without sharing, which clears on its own.
fn is_sharing_violation(e: &std::io::Error) -> bool {
    cfg!(windows) && matches!(e.raw_os_error(), Some(32 | 33))
}

/// Repos whose last resolve in this process failed after their write committed; the
/// next pass of each runs a whole-repo resolve whatever else it finds.
fn resolve_failed() -> &'static Mutex<HashSet<PathBuf>> {
    static FAILED: std::sync::OnceLock<Mutex<HashSet<PathBuf>>> = std::sync::OnceLock::new();
    FAILED.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Record that `root`'s resolve failed after its write committed (also for tests).
#[doc(hidden)]
pub fn note_resolve_failed(root: &Path) {
    if let Ok(mut set) = resolve_failed().lock() {
        set.insert(root.to_path_buf());
    }
}

fn resolve_failed_before(root: &Path) -> bool {
    resolve_failed().lock().is_ok_and(|set| set.contains(root))
}

fn resolve_succeeded(root: &Path) {
    if let Ok(mut set) = resolve_failed().lock() {
        set.remove(root);
    }
}

/// Apply one parse outcome inside the repo's write transaction.
/// Files an incremental index changed, for the resolve cache.
#[derive(Default)]
struct Changes {
    /// (old file id, new file id) of re-parsed files.
    reparsed: Vec<(i64, i64)>,
    /// Files were added or removed.
    structural: bool,
    /// Some file's mtime/size row was updated (content unchanged).
    touched: bool,
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
            changes.touched = true;
        }
        Outcome::Pending => stats.pending += 1,
        Outcome::Unreadable => {
            stats.failed += 1;
            rs.failed += 1;
        }
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

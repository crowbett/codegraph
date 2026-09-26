//! `codegraph watch`: keep the graph fresh by re-indexing repos as their files change.
//!
//! File-system events (the `notify` crate: ReadDirectoryChangesW / inotify / FSEvents)
//! under the watched roots are filtered cheaply (skip_dirs, hidden and `.git` internals,
//! unsupported extensions, credential files), mapped to their repo, and debounced: a batch
//! is flushed after a quiet window, or after a maximum latency while events keep coming.
//! Each dirty repo is then re-indexed with [`index::refresh_repo`], which applies exactly
//! the filters of `index` (.gitignore, skip_dirs, languages, max_file_kb, secret files)
//! and never follows symlinks or junctions. A `.git` directory appearing (a new repo) or a
//! known repo vanishing triggers an incremental re-index of the whole root instead, so
//! repos are added, regrouped and dropped exactly as `index` would.
//!
//! SQLite: the store runs in WAL mode with a busy timeout, so readers (the MCP server)
//! never block, and a second writer waits. A refresh that still fails with "database is
//! busy" (another process holding a long write transaction) is retried on a later flush.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use notify::{EventKind, RecursiveMode, Watcher};
use serde::Serialize;

use crate::config::Config;
use crate::error::{Error, Result};
use crate::index::{self, IndexOptions, IndexStats};
use crate::lang::Lang;
use crate::store::Store;
use crate::util;

/// How often the loop wakes up to check the stop flag.
const TICK: Duration = Duration::from_millis(100);
/// Attempts for a refresh that keeps failing with "database is busy".
const MAX_BUSY_ATTEMPTS: u32 = 5;
/// Changed paths listed per `reindex` event (the count is always exact).
const MAX_LISTED: usize = 50;

/// Watcher options.
#[derive(Debug, Clone)]
pub struct WatchOptions {
    /// Flush a batch after this long without new events.
    pub debounce: Duration,
    /// Flush a batch at the latest this long after its first event.
    pub max_latency: Duration,
    /// Run an incremental `index` of the roots before watching (catches up on changes
    /// made while nothing was watching).
    pub initial_index: bool,
    /// Add repos that are not in the graph yet (and drop vanished ones). Off for the
    /// MCP server, which only keeps repos already in the graph fresh.
    pub add_repos: bool,
}

impl Default for WatchOptions {
    fn default() -> Self {
        Self {
            debounce: Duration::from_millis(300),
            max_latency: Duration::from_secs(2),
            initial_index: true,
            add_repos: true,
        }
    }
}

/// What the watcher reports (NDJSON lines with `--json`: `{"type": "...", ...}`).
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WatchEvent {
    /// Watching; the initial index (if any) is done.
    Ready {
        /// Watched roots.
        roots: Vec<String>,
        /// Repos in the graph under the roots.
        repos: usize,
        /// Initial index summary (when `initial_index`).
        #[serde(skip_serializing_if = "Option::is_none")]
        initial: Option<IndexStats>,
    },
    /// One repo was re-indexed after changes.
    Reindex {
        /// Repo name.
        repo: String,
        /// Repo root.
        root: String,
        /// Changed paths seen in this batch (repo-relative, at most 50 listed).
        files: Vec<String>,
        /// Number of changed paths seen.
        changed: usize,
        /// Wall time of the re-index.
        ms: u128,
        /// Index counters for this repo.
        stats: IndexStats,
    },
    /// A whole root was re-indexed (a repo appeared or vanished, or events were lost).
    Rescan {
        /// Root.
        root: String,
        /// Why.
        reason: String,
        /// Wall time.
        ms: u128,
        /// Index counters.
        stats: IndexStats,
    },
    /// Something failed; the watcher keeps running.
    Error {
        /// Repo or root concerned, if any.
        #[serde(skip_serializing_if = "Option::is_none")]
        repo: Option<String>,
        /// kit error code.
        code: String,
        /// Human message.
        message: String,
    },
}

/// Classification of one event path.
#[derive(Debug, PartialEq, Eq)]
enum PathClass {
    /// Not relevant to the graph.
    Ignore,
    /// A source file, `.gitignore`, or a possible directory: re-index its repo.
    Repo,
    /// A `.git` entry appeared or vanished directly under this directory: a repo boundary
    /// may have changed.
    Boundary,
}

/// Classify a root-relative path (forward slashes).
fn classify(rel: &str, skip: &HashSet<String>, langs: &HashSet<Lang>) -> PathClass {
    if rel.is_empty() {
        return PathClass::Ignore;
    }
    let comps: Vec<&str> = rel.split('/').collect();
    let (last, dirs) = match comps.split_last() {
        Some((l, d)) => (*l, d),
        None => return PathClass::Ignore,
    };
    // Anything inside a skipped, hidden or `.git` directory (build output, git internals).
    if dirs
        .iter()
        .any(|d| d.starts_with('.') || skip.contains(&d.to_lowercase()))
    {
        return PathClass::Ignore;
    }
    if last.eq_ignore_ascii_case(".git") {
        return PathClass::Boundary;
    }
    if last.eq_ignore_ascii_case(".gitignore") {
        return PathClass::Repo;
    }
    if last.starts_with('.') || skip.contains(&last.to_lowercase()) || util::is_secret_file(last) {
        return PathClass::Ignore;
    }
    let p = Path::new(last);
    match Lang::from_path(p) {
        Some(l) if langs.contains(&l) || (l == Lang::C && langs.contains(&Lang::Cpp)) => {
            PathClass::Repo
        }
        // No extension: maybe a directory (renamed, moved in, or deleted), which on some
        // platforms arrives as a single event for the whole subtree.
        None if p.extension().is_none() => PathClass::Repo,
        Some(_) | None => PathClass::Ignore,
    }
}

/// Incremental, unbudgeted indexing.
fn incremental() -> IndexOptions {
    IndexOptions {
        full: false,
        budget: None,
    }
}

#[derive(Debug, Default)]
struct RepoWork {
    root_idx: usize,
    files: BTreeSet<String>,
    attempts: u32,
}

#[derive(Debug, Default)]
struct Batch {
    first: Option<Instant>,
    last: Option<Instant>,
    repos: BTreeMap<PathBuf, RepoWork>,
    rescans: BTreeMap<usize, String>,
}

impl Batch {
    fn mark(&mut self, now: Instant) {
        self.first.get_or_insert(now);
        self.last = Some(now);
    }

    fn is_empty(&self) -> bool {
        self.repos.is_empty() && self.rescans.is_empty()
    }

    fn due(&self, now: Instant, o: &WatchOptions) -> bool {
        match (self.first, self.last) {
            (Some(f), Some(l)) if !self.is_empty() => {
                now.duration_since(l) >= o.debounce || now.duration_since(f) >= o.max_latency
            }
            _ => false,
        }
    }

    /// Time until the batch is due (for the receive timeout).
    fn wait(&self, now: Instant, o: &WatchOptions) -> Duration {
        match (self.first, self.last) {
            (Some(f), Some(l)) if !self.is_empty() => {
                let a = (l + o.debounce).saturating_duration_since(now);
                let b = (f + o.max_latency).saturating_duration_since(now);
                a.min(b).min(TICK)
            }
            _ => TICK,
        }
    }
}

fn is_busy(e: &Error) -> bool {
    matches!(
        e,
        Error::Db(rusqlite::Error::SqliteFailure(f, _))
            if matches!(f.code, rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
    )
}

fn err_event(repo: Option<String>, e: &Error) -> WatchEvent {
    WatchEvent::Error {
        repo,
        code: e.code().to_string(),
        message: e.to_string(),
    }
}

/// The watch loop state.
struct Watch<'a> {
    store: &'a mut Store,
    cfg: &'a Config,
    roots: Vec<PathBuf>,
    opts: WatchOptions,
    skip: HashSet<String>,
    langs: HashSet<Lang>,
}

impl Watch<'_> {
    /// Known repos: root -> name.
    fn known(&self) -> BTreeMap<String, (PathBuf, String)> {
        self.store
            .repos()
            .unwrap_or_default()
            .into_iter()
            .map(|r| (util::slash(&r.root).to_lowercase(), (r.root, r.name)))
            .collect()
    }

    fn add_path(&self, batch: &mut Batch, path: &Path, now: Instant) {
        let Some((idx, rel)) = self
            .roots
            .iter()
            .enumerate()
            .find_map(|(i, r)| util::rel_under(r, path).map(|rel| (i, rel)))
        else {
            return;
        };
        match classify(&rel, &self.skip, &self.langs) {
            PathClass::Ignore => {}
            PathClass::Boundary => {
                if self.opts.add_repos {
                    batch
                        .rescans
                        .entry(idx)
                        .or_insert_with(|| format!("repo boundary changed: {rel}"));
                    batch.mark(now);
                }
            }
            PathClass::Repo => {
                let root = &self.roots[idx];
                let repo = index::repo_root_of(path, Some(root)).unwrap_or_else(|| root.clone());
                let repo_rel = util::rel_under(&repo, path).unwrap_or(rel);
                let w = batch.repos.entry(repo).or_insert_with(|| RepoWork {
                    root_idx: idx,
                    ..RepoWork::default()
                });
                if !repo_rel.is_empty() {
                    w.files.insert(repo_rel);
                }
                batch.mark(now);
            }
        }
    }

    /// Flush a due batch. Busy failures are put back into `retry`.
    fn flush(&mut self, batch: Batch, retry: &mut Batch, emit: &mut dyn FnMut(&WatchEvent)) {
        let mut rescans = batch.rescans;
        let known = self.known();
        let mut work = Vec::new();
        for (repo, w) in batch.repos {
            match known.get(&util::slash(&repo).to_lowercase()) {
                Some((root, name)) => work.push((root.clone(), name.clone(), w)),
                // Not in the graph yet: a root rescan adds (and names) it exactly as
                // `index` would, and only if it has source files.
                None if self.opts.add_repos => {
                    rescans.entry(w.root_idx).or_insert_with(|| {
                        format!("new files outside known repos: {}", util::slash(&repo))
                    });
                }
                None => {}
            }
        }
        // A known repo whose directory is gone: let a root rescan drop it.
        if self.opts.add_repos && !work.is_empty() {
            for (root, _) in known.values() {
                if root.is_dir() {
                    continue;
                }
                if let Some(idx) = self
                    .roots
                    .iter()
                    .position(|r| util::rel_under(r, root).is_some())
                {
                    rescans
                        .entry(idx)
                        .or_insert_with(|| format!("repo vanished: {}", util::slash(root)));
                }
            }
        }
        for (idx, reason) in &rescans {
            let root = self.roots[*idx].clone();
            let t = Instant::now();
            match index::index_roots(
                self.store,
                self.cfg,
                std::slice::from_ref(&root),
                &incremental(),
            ) {
                Ok(stats) => emit(&WatchEvent::Rescan {
                    root: util::slash(&root),
                    reason: reason.clone(),
                    ms: t.elapsed().as_millis(),
                    stats,
                }),
                Err(e) if is_busy(&e) => {
                    retry.rescans.insert(*idx, reason.clone());
                    retry.mark(Instant::now());
                }
                Err(e) => emit(&err_event(Some(util::slash(&root)), &e)),
            }
        }
        for (root, name, work) in work {
            if rescans.contains_key(&work.root_idx) {
                continue; // covered by the rescan
            }
            let t = Instant::now();
            match index::refresh_repo(self.store, self.cfg, &root, &name, &incremental()) {
                Ok(stats) => emit(&WatchEvent::Reindex {
                    repo: name,
                    root: util::slash(&root),
                    changed: work.files.len(),
                    files: work.files.into_iter().take(MAX_LISTED).collect(),
                    ms: t.elapsed().as_millis(),
                    stats,
                }),
                Err(e) if is_busy(&e) && work.attempts + 1 < MAX_BUSY_ATTEMPTS => {
                    eprintln!("codegraph watch: {name}: database busy, retrying");
                    retry.repos.insert(
                        root,
                        RepoWork {
                            attempts: work.attempts + 1,
                            ..work
                        },
                    );
                    retry.mark(Instant::now());
                }
                Err(e) => emit(&err_event(Some(name), &e)),
            }
        }
    }
}

/// Watch `roots` until `stop` is set, re-indexing changed repos into `store`.
///
/// Events are reported through `emit`. Returns an error only when watching cannot start
/// (a root is missing or a UNC path, or the OS watcher fails); per-batch failures are
/// reported as [`WatchEvent::Error`] and the loop keeps going.
pub fn watch(
    store: &mut Store,
    cfg: &Config,
    roots: &[PathBuf],
    opts: &WatchOptions,
    stop: &AtomicBool,
    emit: &mut dyn FnMut(&WatchEvent),
) -> Result<()> {
    let mut abs = Vec::new();
    for r in roots {
        if util::is_remote_path(&r.to_string_lossy()) {
            return Err(Error::Usage(format!(
                "`{}`: UNC and device paths are not allowed",
                r.display()
            )));
        }
        let a = util::absolute(r);
        if !a.is_dir() {
            return Err(Error::NotFound(format!(
                "watch root {} does not exist",
                a.display()
            )));
        }
        if !abs.contains(&a) {
            abs.push(a);
        }
    }
    if abs.is_empty() {
        return Err(Error::Usage("no roots to watch".into()));
    }

    let (tx, rx) = mpsc::channel::<notify::Result<notify::Event>>();
    let mut watcher = notify::recommended_watcher(tx)
        .map_err(|e| Error::Internal(format!("start file watcher: {e}")))?;
    for r in &abs {
        watcher
            .watch(r, RecursiveMode::Recursive)
            .map_err(|e| Error::Internal(format!("watch {}: {e}", r.display())))?;
    }

    let mut w = Watch {
        store,
        cfg,
        roots: abs,
        opts: opts.clone(),
        skip: cfg.skip_dirs.iter().map(|s| s.to_lowercase()).collect(),
        langs: cfg.languages.iter().copied().collect(),
    };

    let initial = if opts.initial_index {
        Some(index::index_roots(w.store, cfg, &w.roots, &incremental())?)
    } else {
        None
    };
    let repos = w
        .known()
        .into_values()
        .filter(|(r, _)| w.roots.iter().any(|x| util::rel_under(x, r).is_some()))
        .count();
    emit(&WatchEvent::Ready {
        roots: w.roots.iter().map(|r| util::slash(r)).collect(),
        repos,
        initial,
    });

    let mut batch = Batch::default();
    while !stop.load(Ordering::SeqCst) {
        let now = Instant::now();
        match rx.recv_timeout(batch.wait(now, &w.opts)) {
            Ok(Ok(ev)) => {
                let now = Instant::now();
                if ev.need_rescan() {
                    if w.opts.add_repos {
                        for i in 0..w.roots.len() {
                            batch.rescans.insert(i, "file events were dropped".into());
                        }
                        batch.mark(now);
                    }
                } else if !matches!(ev.kind, EventKind::Access(_)) {
                    for p in &ev.paths {
                        w.add_path(&mut batch, p, now);
                    }
                }
            }
            Ok(Err(e)) => {
                emit(&WatchEvent::Error {
                    repo: None,
                    code: "io".into(),
                    message: format!("file watcher: {e}"),
                });
                // Events may have been lost: catch up.
                if w.opts.add_repos {
                    for i in 0..w.roots.len() {
                        batch
                            .rescans
                            .insert(i, "file watcher reported an error".into());
                    }
                    batch.mark(Instant::now());
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(Error::Internal("file watcher stopped".into()));
            }
        }
        if batch.due(Instant::now(), &w.opts) {
            let mut retry = Batch::default();
            let due = std::mem::take(&mut batch);
            w.flush(due, &mut retry, emit);
            batch = retry;
        }
    }
    drop(watcher);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sets() -> (HashSet<String>, HashSet<Lang>) {
        let cfg = Config::default();
        (
            cfg.skip_dirs.iter().map(|s| s.to_lowercase()).collect(),
            cfg.languages.iter().copied().collect(),
        )
    }

    #[test]
    fn classify_paths() {
        let (skip, langs) = sets();
        let c = |p: &str| classify(p, &skip, &langs);
        assert_eq!(c("repo/src/main.go"), PathClass::Repo);
        assert_eq!(c("repo/include/x.h"), PathClass::Repo);
        assert_eq!(c("repo/.gitignore"), PathClass::Repo);
        assert_eq!(c("repo/src"), PathClass::Repo, "possible directory");
        assert_eq!(c("repo/.git"), PathClass::Boundary);
        assert_eq!(c("repo/.git/index.lock"), PathClass::Ignore);
        assert_eq!(c("repo/.git/objects/ab"), PathClass::Ignore);
        assert_eq!(c("repo/target/debug/build/x.rs"), PathClass::Ignore);
        assert_eq!(c("repo/Target/x.rs"), PathClass::Ignore);
        assert_eq!(c("repo/node_modules/a/index.js"), PathClass::Ignore);
        assert_eq!(c("repo/node_modules"), PathClass::Ignore);
        assert_eq!(c("repo/.venv/lib/x.py"), PathClass::Ignore);
        assert_eq!(c("repo/README.md"), PathClass::Ignore);
        assert_eq!(c("repo/main.go~"), PathClass::Ignore);
        assert_eq!(c("repo/.env"), PathClass::Ignore);
        assert_eq!(c("repo/secrets.py"), PathClass::Ignore);
        assert_eq!(c("repo/.main.go.swp"), PathClass::Ignore);
        assert_eq!(c(""), PathClass::Ignore);
    }

    #[test]
    fn classify_respects_languages() {
        let (skip, _) = sets();
        let only_go: HashSet<Lang> = [Lang::Go].into_iter().collect();
        assert_eq!(classify("r/a.go", &skip, &only_go), PathClass::Repo);
        assert_eq!(classify("r/a.py", &skip, &only_go), PathClass::Ignore);
    }

    #[test]
    fn batch_timing() {
        let o = WatchOptions::default();
        let t0 = Instant::now();
        let mut b = Batch::default();
        assert!(!b.due(t0, &o));
        b.repos.insert(PathBuf::from("r"), RepoWork::default());
        b.mark(t0);
        assert!(!b.due(t0 + Duration::from_millis(100), &o));
        assert!(b.due(t0 + o.debounce, &o));
        // Events keep arriving: the max latency still flushes.
        b.mark(t0 + Duration::from_millis(1900));
        assert!(!b.due(t0 + Duration::from_millis(1950), &o));
        assert!(b.due(t0 + o.max_latency, &o));
        assert!(b.wait(t0, &o) <= TICK);
    }

    #[test]
    fn busy_detection() {
        let busy = Error::Db(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
            None,
        ));
        assert!(is_busy(&busy));
        assert!(!is_busy(&Error::Internal("x".into())));
    }
}

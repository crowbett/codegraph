//! Scale benchmark: cold / no-op / one-file re-index timings on large pinned repos.
//!
//! Repos come from `bench/scale.toml` (shallow clones into `.cache/bench/scale/<name>`).
//! Each run indexes its repos in one `index_roots` call into a throwaway database under
//! `.cache/bench/scale-db/<run>/`, N times: cold (fresh DB), no-op (nothing changed), and
//! one file touched (a comment appended to the repo's `touch` file, restored afterwards).
//! Results: `.cache/bench/scale.json` plus a markdown table on stdout.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use codegraph::config::Config;
use codegraph::index::{self, IndexOptions, IndexStats};
use codegraph::store::Store;

use crate::corpus::Dirs;

/// `bench/scale.toml`.
#[derive(Debug, Deserialize)]
struct Manifest {
    #[serde(rename = "repo")]
    repos: Vec<Repo>,
    #[serde(rename = "run")]
    runs: Vec<Run>,
}

/// One pinned repo.
#[derive(Debug, Clone, Deserialize)]
struct Repo {
    name: String,
    url: String,
    /// Tag or branch for `git clone --branch`.
    rev: String,
    /// Expected commit of `rev` (checked after cloning when non-empty).
    #[serde(default)]
    commit: String,
    /// Repo-relative file touched for the one-file re-index.
    touch: String,
}

/// One benchmark run: these repos indexed together.
#[derive(Debug, Deserialize)]
struct Run {
    name: String,
    repos: Vec<String>,
}

/// Options for `scale`.
pub struct Opts {
    /// Only these runs (empty = all).
    pub runs: Vec<String>,
    /// Iterations per run.
    pub iterations: usize,
    /// Clone and count only.
    pub prepare_only: bool,
    /// JSON output path.
    pub out: Option<PathBuf>,
}

/// Timings and counters of one `index_roots` call.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Phase {
    pub wall_ms: u128,
    pub parse_ms: u128,
    pub write_ms: u128,
    pub resolve_ms: u128,
    /// Remainder: walk + change detection bookkeeping + final counts.
    pub other_ms: u128,
    pub files: usize,
    pub parsed: usize,
    pub symbols: i64,
    pub edges: i64,
    pub resolved_edges: i64,
}

impl Phase {
    fn from(st: &IndexStats, wall_ms: u128) -> Self {
        Self {
            wall_ms,
            parse_ms: st.parse_ms,
            write_ms: st.write_ms,
            resolve_ms: st.resolve_ms,
            other_ms: wall_ms.saturating_sub(st.parse_ms + st.write_ms + st.resolve_ms),
            files: st.files,
            parsed: st.parsed,
            symbols: st.symbols,
            edges: st.edges,
            resolved_edges: st.resolved_edges,
        }
    }
}

/// One iteration of a run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Iteration {
    pub cold: Phase,
    pub noop: Phase,
    pub one_file: Phase,
    /// Main DB file after a WAL checkpoint, bytes.
    pub db_bytes: u64,
}

/// Per-repo pin and file count.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoInfo {
    pub name: String,
    pub url: String,
    pub rev: String,
    pub commit: String,
    pub files: usize,
}

/// Results of one run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunResult {
    pub name: String,
    pub repos: Vec<RepoInfo>,
    pub iterations: Vec<Iteration>,
    /// Medians over iterations.
    pub median: Iteration,
    /// Peak working set of the bench process after this run (bytes; Windows only).
    pub peak_working_set: Option<u64>,
}

/// The JSON document.
#[derive(Debug, Serialize, Deserialize)]
pub struct Report {
    pub codegraph: String,
    pub threads: usize,
    pub runs: Vec<RunResult>,
}

fn git() -> Command {
    let mut c = Command::new("git");
    c.args([
        "-c",
        "core.fsmonitor=",
        "-c",
        "core.hooksPath=NUL",
        "-c",
        "protocol.ext.allow=never",
        "-c",
        "core.longpaths=true",
    ]);
    c
}

fn run_git(mut c: Command, what: &str) -> Result<()> {
    let st = c.status().with_context(|| format!("spawn {what}"))?;
    if !st.success() {
        bail!("{what} failed: {st}");
    }
    Ok(())
}

fn scale_dir(dirs: &Dirs) -> PathBuf {
    dirs.cache.join("scale")
}

/// Windows device names cannot be checked out (`aux.c` in the kernel): leave them out
/// of the working tree with a sparse checkout.
const SPARSE: &str = "/*\n!con.*\n!prn.*\n!aux.*\n!nul.*\n!con\n!prn\n!aux\n!nul\n";

/// Shallow clone at the pinned tag; verify the commit.
fn fetch(dirs: &Dirs, r: &Repo) -> Result<String> {
    let dst = scale_dir(dirs).join(&r.name);
    if !dst.join(".git").is_dir() {
        std::fs::create_dir_all(scale_dir(dirs))?;
        eprintln!("{}: cloning {} @ {} (shallow)", r.name, r.url, r.rev);
        let mut c = git();
        c.args(["clone", "-q", "--depth", "1", "--no-checkout", "--branch"])
            .arg(&r.rev)
            .arg(&r.url)
            .arg(&dst);
        run_git(c, &format!("git clone {}", r.name))?;
    }
    let marker = dst.join(".git").join("codegraph-bench-checkout");
    if !marker.is_file() {
        let info = dst.join(".git").join("info");
        std::fs::create_dir_all(&info)?;
        std::fs::write(info.join("sparse-checkout"), SPARSE)?;
        for (k, v) in [
            ("core.sparseCheckout", "true"),
            ("core.sparseCheckoutCone", "false"),
        ] {
            let mut c = git();
            c.arg("-C").arg(&dst).args(["config", k, v]);
            run_git(c, &format!("git config {k}"))?;
        }
        // protectNTFS=false only lets the index hold the device-named entries; the
        // sparse patterns keep them out of the working tree.
        let mut c = git();
        c.arg("-C")
            .arg(&dst)
            .args(["-c", "core.protectNTFS=false", "checkout", "-q", "HEAD"]);
        run_git(c, &format!("checkout {}", r.name))?;
        std::fs::write(&marker, &r.rev)?;
    }
    let out = git()
        .arg("-C")
        .arg(&dst)
        .args(["rev-parse", "HEAD"])
        .output()
        .context("git rev-parse")?;
    let head = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !r.commit.is_empty() && head != r.commit {
        bail!(
            "{}: HEAD is {head}, scale.toml pins {} for {}",
            r.name,
            r.commit,
            r.rev
        );
    }
    Ok(head)
}

fn load(dirs: &Dirs) -> Result<Manifest> {
    let p = dirs.bench.join("scale.toml");
    let text = std::fs::read_to_string(&p).with_context(|| format!("read {}", p.display()))?;
    Ok(toml::from_str(&text)?)
}

fn remove_db(dir: &Path) -> Result<()> {
    if dir.exists() {
        std::fs::remove_dir_all(dir).with_context(|| format!("remove {}", dir.display()))?;
    }
    std::fs::create_dir_all(dir)?;
    Ok(())
}

fn timed(store: &mut Store, cfg: &Config, roots: &[PathBuf]) -> Result<Phase> {
    let t = Instant::now();
    let st = index::index_roots(store, cfg, roots, &IndexOptions::default())?;
    Ok(Phase::from(&st, t.elapsed().as_millis()))
}

/// Restores the touched file even when indexing fails.
struct Restore {
    path: PathBuf,
    bytes: Vec<u8>,
}

impl Drop for Restore {
    fn drop(&mut self) {
        let _ = std::fs::write(&self.path, &self.bytes);
    }
}

fn one_iteration(
    db_dir: &Path,
    cfg: &Config,
    roots: &[PathBuf],
    touch: &Path,
) -> Result<(Iteration, IndexStats)> {
    remove_db(db_dir)?;
    let db = db_dir.join("graph.db");
    let mut store = Store::open(&db)?;
    let t = Instant::now();
    let cold_stats = index::index_roots(&mut store, cfg, roots, &IndexOptions::default())?;
    let cold = Phase::from(&cold_stats, t.elapsed().as_millis());
    eprintln!(
        "  cold: {} files, {} ms (parse {} / write {} / resolve {})",
        cold.files, cold.wall_ms, cold.parse_ms, cold.write_ms, cold.resolve_ms
    );
    let noop = timed(&mut store, cfg, roots)?;
    eprintln!("  no-op: {} ms", noop.wall_ms);

    let original = std::fs::read(touch).with_context(|| format!("read {}", touch.display()))?;
    let guard = Restore {
        path: touch.to_path_buf(),
        bytes: original.clone(),
    };
    let mut edited = original;
    edited.extend_from_slice(b"\n/* codegraph-bench touch */\n");
    std::fs::write(touch, &edited)?;
    let one_file = timed(&mut store, cfg, roots)?;
    drop(guard);
    eprintln!(
        "  1 file: {} ms ({} parsed, resolve {} ms)",
        one_file.wall_ms, one_file.parsed, one_file.resolve_ms
    );

    store
        .conn()
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
    drop(store);
    let db_bytes = std::fs::metadata(&db).map(|m| m.len()).unwrap_or(0);
    Ok((
        Iteration {
            cold,
            noop,
            one_file,
            db_bytes,
        },
        cold_stats,
    ))
}

fn median_u128(mut v: Vec<u128>) -> u128 {
    v.sort_unstable();
    v.get(v.len() / 2).copied().unwrap_or(0)
}

fn median_phase(ps: &[&Phase]) -> Phase {
    let m = |f: fn(&Phase) -> u128| median_u128(ps.iter().map(|p| f(p)).collect());
    let last = ps.last().map(|p| (*p).clone()).unwrap_or_default();
    Phase {
        wall_ms: m(|p| p.wall_ms),
        parse_ms: m(|p| p.parse_ms),
        write_ms: m(|p| p.write_ms),
        resolve_ms: m(|p| p.resolve_ms),
        other_ms: m(|p| p.other_ms),
        ..last
    }
}

fn median(its: &[Iteration]) -> Iteration {
    let cold: Vec<&Phase> = its.iter().map(|i| &i.cold).collect();
    let noop: Vec<&Phase> = its.iter().map(|i| &i.noop).collect();
    let one: Vec<&Phase> = its.iter().map(|i| &i.one_file).collect();
    Iteration {
        cold: median_phase(&cold),
        noop: median_phase(&noop),
        one_file: median_phase(&one),
        db_bytes: its.last().map_or(0, |i| i.db_bytes),
    }
}

/// Peak working set of this process (Windows).
#[cfg(windows)]
#[allow(unsafe_code)]
fn peak_working_set() -> Option<u64> {
    use windows_sys::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    let size = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
    // SAFETY: PROCESS_MEMORY_COUNTERS is plain data; the pseudo-handle from
    // GetCurrentProcess needs no closing; `cb` tells the API the buffer size.
    unsafe {
        let mut pmc: PROCESS_MEMORY_COUNTERS = std::mem::zeroed();
        pmc.cb = size;
        if GetProcessMemoryInfo(GetCurrentProcess(), &raw mut pmc, size) != 0 {
            return Some(pmc.PeakWorkingSetSize as u64);
        }
    }
    None
}

#[cfg(not(windows))]
fn peak_working_set() -> Option<u64> {
    None
}

/// Run the scale benchmark.
pub fn run(dirs: &Dirs, opts: &Opts) -> Result<()> {
    let m = load(dirs)?;
    let runs: Vec<&Run> = m
        .runs
        .iter()
        .filter(|r| opts.runs.is_empty() || opts.runs.contains(&r.name))
        .collect();
    if runs.is_empty() {
        bail!("no run matches {:?}", opts.runs);
    }
    let mut heads = std::collections::HashMap::new();
    for run in &runs {
        for name in &run.repos {
            if heads.contains_key(name) {
                continue;
            }
            let r = m
                .repos
                .iter()
                .find(|r| &r.name == name)
                .with_context(|| format!("run {} names unknown repo {name}", run.name))?;
            heads.insert(name.clone(), fetch(dirs, r)?);
        }
    }
    let cfg = Config {
        roots: Vec::new(),
        ..Config::default()
    };
    if opts.prepare_only {
        return Ok(());
    }

    let mut results = Vec::new();
    for run in runs {
        let repos: Vec<&Repo> = run
            .repos
            .iter()
            .filter_map(|n| m.repos.iter().find(|r| &r.name == n))
            .collect();
        let roots: Vec<PathBuf> = repos
            .iter()
            .map(|r| codegraph::util::absolute(&scale_dir(dirs).join(&r.name)))
            .collect();
        let Some(first) = repos.first() else {
            continue;
        };
        let touch = roots[0].join(&first.touch);
        let db_dir = dirs.cache.join("scale-db").join(&run.name);
        eprintln!("run {} ({} repos)", run.name, repos.len());
        let mut its = Vec::new();
        let mut per_repo_files = Vec::new();
        for i in 0..opts.iterations.max(1) {
            eprintln!(" iteration {}", i + 1);
            let (it, cold_stats) = one_iteration(&db_dir, &cfg, &roots, &touch)?;
            per_repo_files = repos
                .iter()
                .zip(&roots)
                .map(|(r, root)| {
                    let files = cold_stats
                        .changed_repos
                        .iter()
                        .filter(|x| codegraph::util::rel_under(root, Path::new(&x.root)).is_some())
                        .map(|x| x.files)
                        .sum();
                    RepoInfo {
                        name: r.name.clone(),
                        url: r.url.clone(),
                        rev: r.rev.clone(),
                        commit: heads.get(&r.name).cloned().unwrap_or_default(),
                        files,
                    }
                })
                .collect();
            its.push(it);
        }
        results.push(RunResult {
            name: run.name.clone(),
            repos: per_repo_files,
            median: median(&its),
            iterations: its,
            peak_working_set: peak_working_set(),
        });
    }

    let report = Report {
        codegraph: codegraph::VERSION.to_string(),
        threads: std::thread::available_parallelism().map_or(1, std::num::NonZero::get),
        runs: results,
    };
    let out = opts
        .out
        .clone()
        .unwrap_or_else(|| dirs.cache.join("scale.json"));
    if let Some(p) = out.parent() {
        std::fs::create_dir_all(p)?;
    }
    std::fs::write(&out, serde_json::to_string_pretty(&report)?)?;
    eprintln!("wrote {}", out.display());
    print!("{}", table(&report));
    Ok(())
}

fn secs(ms: u128) -> String {
    format!("{:.1} s", ms as f64 / 1000.0)
}

fn mb(b: u64) -> String {
    format!("{:.0} MB", b as f64 / 1_048_576.0)
}

/// Markdown summary (medians).
pub fn table(r: &Report) -> String {
    let mut s = String::new();
    let _ = writeln!(
        s,
        "| run | files | symbols | edges (resolved) | cold | parse / write / resolve / other | no-op | 1 file | peak WS | DB |"
    );
    let _ = writeln!(s, "|---|---:|---:|---:|---:|---|---:|---:|---:|---:|");
    for run in &r.runs {
        let m = &run.median;
        let _ = writeln!(
            s,
            "| {} | {} | {} | {} ({}) | {} | {} / {} / {} / {} | {} | {} | {} | {} |",
            run.name,
            m.cold.files,
            m.cold.symbols,
            m.cold.edges,
            m.cold.resolved_edges,
            secs(m.cold.wall_ms),
            secs(m.cold.parse_ms),
            secs(m.cold.write_ms),
            secs(m.cold.resolve_ms),
            secs(m.cold.other_ms),
            secs(m.noop.wall_ms),
            secs(m.one_file.wall_ms),
            run.peak_working_set.map_or_else(|| "n/a".into(), mb),
            mb(m.db_bytes),
        );
    }
    let _ = writeln!(
        s,
        "\nmedian of {} iteration(s), {} threads; pins:",
        r.runs.first().map_or(0, |x| x.iterations.len()),
        r.threads
    );
    let mut seen = std::collections::HashSet::new();
    for run in &r.runs {
        for repo in &run.repos {
            if seen.insert(repo.name.clone()) {
                let _ = writeln!(
                    s,
                    "- {} {} @ {} ({}): {} files",
                    repo.name, repo.url, repo.rev, repo.commit, repo.files
                );
            }
        }
    }
    s
}

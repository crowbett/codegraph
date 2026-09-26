//! Corpus manifest (`bench/corpus.toml`) and `prepare`: pinned clones + SCIP indexes.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use serde::Deserialize;

/// The manifest.
#[derive(Debug, Deserialize)]
pub struct Corpus {
    /// Pinned indexer versions.
    pub tools: Tools,
    /// Repos.
    #[serde(rename = "repo")]
    pub repos: Vec<Repo>,
}

/// Pinned indexer versions.
#[derive(Debug, Deserialize)]
pub struct Tools {
    #[serde(rename = "scip-go")]
    pub scip_go: String,
    #[serde(rename = "scip-typescript")]
    pub scip_typescript: String,
    #[serde(rename = "scip-python")]
    pub scip_python: String,
}

/// One corpus repo.
#[derive(Debug, Clone, Deserialize)]
pub struct Repo {
    pub name: String,
    pub lang: String,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub local: Option<String>,
    pub rev: String,
    #[serde(default)]
    pub ts_projects: Vec<String>,
    /// Python: import roots for pyright (`src` layouts), written to pyrightconfig.json.
    #[serde(default)]
    pub py_extra_paths: Vec<String>,
}

/// Directory layout under `bench/`.
pub struct Dirs {
    pub bench: PathBuf,
    pub cache: PathBuf,
}

impl Dirs {
    pub fn new() -> Self {
        let bench = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        // Outside bench/ (its own Cargo workspace) so cloned Rust repos are not nested in a
        // workspace; `.cache/` at the repo root is gitignored.
        let cache = std::env::var_os("CODEGRAPH_BENCH_CACHE").map_or_else(
            || bench.join("..").join(".cache").join("bench"),
            PathBuf::from,
        );
        Self { bench, cache }
    }
    pub fn corpus(&self, name: &str) -> PathBuf {
        self.cache.join("corpus").join(name)
    }
    pub fn scip(&self, name: &str) -> PathBuf {
        self.cache.join("scip").join(format!("{name}.scip"))
    }
    fn tools(&self) -> PathBuf {
        self.cache.join("tools")
    }
}

pub fn load(dirs: &Dirs) -> Result<Corpus> {
    let p = dirs.bench.join("corpus.toml");
    let text = std::fs::read_to_string(&p).with_context(|| format!("read {}", p.display()))?;
    Ok(toml::from_str(&text)?)
}

fn home() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map_or_else(|| PathBuf::from("."), PathBuf::from)
}

fn expand(p: &str) -> PathBuf {
    match p.strip_prefix("~/") {
        Some(rest) => home().join(rest),
        None => PathBuf::from(p),
    }
}

/// `git` with repo-config code execution disabled (KIT.md: a repo's own config can run code).
fn git() -> Command {
    let mut c = Command::new("git");
    c.args([
        "-c",
        "core.fsmonitor=",
        "-c",
        "core.hooksPath=NUL",
        "-c",
        "protocol.ext.allow=never",
    ]);
    c
}

fn run(mut c: Command, what: &str) -> Result<()> {
    let st = c.status().with_context(|| format!("spawn {what}"))?;
    if !st.success() {
        bail!("{what} failed: {st}");
    }
    Ok(())
}

fn exe(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

fn npm() -> Command {
    if cfg!(windows) {
        let mut c = Command::new("cmd");
        c.args(["/C", "npm"]);
        c
    } else {
        Command::new("npm")
    }
}

/// Clone (or re-checkout) one repo at its pinned rev.
fn fetch(dirs: &Dirs, r: &Repo) -> Result<bool> {
    let dst = dirs.corpus(&r.name);
    let src = match (&r.url, &r.local) {
        (Some(u), _) => u.clone(),
        (None, Some(l)) => {
            let p = expand(l);
            if !p.is_dir() {
                eprintln!("skip {}: local source {} not found", r.name, p.display());
                return Ok(false);
            }
            p.to_string_lossy().into_owned()
        }
        (None, None) => bail!("repo {} has neither url nor local", r.name),
    };
    if !dst.join(".git").is_dir() {
        std::fs::create_dir_all(dst.parent().unwrap_or(&dst))?;
        let mut c = git();
        c.args(["clone", "-q", "--no-hardlinks", &src]).arg(&dst);
        run(c, &format!("git clone {}", r.name))?;
    }
    let head = git()
        .arg("-C")
        .arg(&dst)
        .args(["rev-parse", "HEAD"])
        .output()?;
    if String::from_utf8_lossy(&head.stdout).trim() != r.rev {
        let mut c = git();
        c.arg("-C").arg(&dst).args(["fetch", "-q", "origin"]);
        let _ = c.status();
        let mut c = git();
        c.arg("-C")
            .arg(&dst)
            .args(["checkout", "-q", "--detach", &r.rev]);
        run(c, &format!("checkout {} {}", r.name, r.rev))?;
    }
    Ok(true)
}

fn install_tools(dirs: &Dirs, c: &Corpus, langs: &[&str]) -> Result<()> {
    let tools = dirs.tools();
    if langs.contains(&"go") && !tools.join("go").join(exe("scip-go")).is_file() {
        let mut cmd = Command::new("go");
        cmd.env("GOBIN", tools.join("go")).args([
            "install",
            &format!(
                "github.com/scip-code/scip-go/cmd/scip-go@{}",
                c.tools.scip_go
            ),
        ]);
        run(cmd, "go install scip-go")?;
    }
    let node = tools.join("node");
    let need_node = langs.iter().any(|l| matches!(*l, "typescript" | "python"));
    if need_node
        && !node
            .join("node_modules/@sourcegraph/scip-python/index.js")
            .is_file()
    {
        std::fs::create_dir_all(&node)?;
        if !node.join("package.json").is_file() {
            std::fs::write(node.join("package.json"), "{\"private\":true}\n")?;
        }
        let mut cmd = npm();
        cmd.current_dir(&node).args([
            "install",
            "--no-fund",
            "--no-audit",
            &format!("@sourcegraph/scip-typescript@{}", c.tools.scip_typescript),
            &format!("@sourcegraph/scip-python@{}", c.tools.scip_python),
        ]);
        run(cmd, "npm install scip indexers")?;
    }
    if need_node {
        patch_scip_python(&node)?;
    }
    Ok(())
}

/// scip-python 0.6.x builds `new RegExp(path.sep, "g")`, which throws on Windows
/// (`\` alone is an invalid pattern). Escape it in the local copy only.
fn patch_scip_python(node: &Path) -> Result<()> {
    let p = node.join("node_modules/@sourcegraph/scip-python/dist/scip-python.js");
    let Ok(s) = std::fs::read_to_string(&p) else {
        return Ok(());
    };
    let bad = "a=new RegExp(o.sep,\"g\")";
    if s.contains(bad) {
        let good = "a=new RegExp(o.sep===\"\\\\\"?\"\\\\\\\\\":o.sep,\"g\")";
        std::fs::write(&p, s.replacen(bad, good, 1))?;
        eprintln!("patched scip-python path.sep regex (Windows)");
    }
    Ok(())
}

fn scip_index(dirs: &Dirs, r: &Repo) -> Result<()> {
    let repo = dirs.corpus(&r.name);
    let out = dirs.scip(&r.name);
    std::fs::create_dir_all(out.parent().unwrap_or(&out))?;
    let tools = dirs.tools();
    let node_mod = tools.join("node").join("node_modules").join("@sourcegraph");
    let cmd = match r.lang.as_str() {
        "go" => {
            let mut c = Command::new(tools.join("go").join(exe("scip-go")));
            c.arg("--output").arg(&out);
            c
        }
        "rust" => {
            let mut c = Command::new("rust-analyzer");
            c.env("CARGO_TARGET_DIR", dirs.cache.join("target-ra"))
                .args(["scip", "."])
                .arg("--output")
                .arg(&out);
            c
        }
        "typescript" => {
            if !repo.join("node_modules").is_dir() {
                let lock = repo.join("package-lock.json").is_file();
                let mut c = npm();
                c.current_dir(&repo).args([
                    if lock { "ci" } else { "install" },
                    "--ignore-scripts",
                    "--no-fund",
                    "--no-audit",
                ]);
                run(c, &format!("npm install in {}", r.name))?;
            }
            let mut c = Command::new("node");
            c.arg(node_mod.join("scip-typescript/dist/src/main.js"))
                .arg("index")
                .arg("--output")
                .arg(&out)
                .args(&r.ts_projects);
            c
        }
        "python" => {
            if !r.py_extra_paths.is_empty() {
                let cfg = serde_json::json!({ "extraPaths": r.py_extra_paths });
                std::fs::write(repo.join("pyrightconfig.json"), cfg.to_string())?;
            }
            let mut c = Command::new("node");
            c.arg(node_mod.join("scip-python/index.js"))
                .args(["index", ".", "--project-name", &r.name, "--output"])
                .arg(&out);
            c
        }
        l => bail!("unknown lang {l}"),
    };
    let mut cmd = cmd;
    cmd.current_dir(&repo);
    run(cmd, &format!("scip index {}", r.name))
}

/// Clone every repo at its pin and build its SCIP index (skipped when already built for
/// the pinned rev; `force` rebuilds).
pub fn prepare(dirs: &Dirs, only: &[String], force: bool) -> Result<()> {
    let c = load(dirs)?;
    let repos: Vec<&Repo> = c
        .repos
        .iter()
        .filter(|r| only.is_empty() || only.contains(&r.name))
        .collect();
    let langs: Vec<&str> = repos.iter().map(|r| r.lang.as_str()).collect();
    install_tools(dirs, &c, &langs)?;
    for r in repos {
        if !fetch(dirs, r)? {
            continue;
        }
        let out = dirs.scip(&r.name);
        let stamp = out.with_extension("rev");
        let fresh = out.is_file()
            && std::fs::read_to_string(&stamp).is_ok_and(|s| s.trim() == r.rev)
            && !force;
        if fresh {
            eprintln!("{}: scip index up to date", r.name);
            continue;
        }
        eprintln!("{}: building scip index ({})", r.name, r.lang);
        scip_index(dirs, r)?;
        std::fs::write(&stamp, &r.rev)?;
    }
    Ok(())
}

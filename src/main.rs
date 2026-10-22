//! `codegraph` binary: argument handling, output mode, dispatch.

use std::fmt::Write as _;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use clap::Parser;
use serde::Serialize;

use codegraph::cli::{Cli, Command, ConfigAction, GraphFormatArg};
use codegraph::config::{self, Config};
use codegraph::error::Error;
use codegraph::index::{self, IndexOptions};
use codegraph::mcp::Server;
use codegraph::model::SymKind;
use codegraph::output::{self, Style};
use codegraph::query::{self, Dir, Filter, GraphFormat};
use codegraph::store::Store;
use codegraph::watch::{self, WatchEvent, WatchOptions};
use codegraph::{render, util};

/// Indexing allocates tens of millions of small strings from every core; the Windows
/// system heap serializes them (measured: see docs/SCALE.md).
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// Output context for one invocation.
struct App {
    json: bool,
    quiet: bool,
    verbose: bool,
    style: Style,
    cfg: Config,
}

#[allow(clippy::unused_self)]
impl App {
    fn print(&self, s: &str) -> anyhow::Result<()> {
        let mut out = std::io::stdout().lock();
        out.write_all(s.as_bytes()).context("write stdout")?;
        out.flush().context("flush stdout")
    }

    fn json_ok<T: Serialize>(&self, data: &T) -> anyhow::Result<()> {
        self.print(&format!("{}\n", output::ok_envelope(data)))
    }

    /// JSON envelope, or the rendered human text.
    fn emit<T: Serialize>(&self, data: &T, human: impl FnOnce() -> String) -> anyhow::Result<()> {
        if self.json {
            self.json_ok(data)
        } else {
            self.print(&human())
        }
    }

    fn open_store(&self) -> anyhow::Result<Store> {
        let path = config::db_path();
        Ok(Store::open(&path)?)
    }

    fn filter(&self, store: &Store, repo: Option<&str>) -> anyhow::Result<Filter> {
        Ok(Filter {
            repos: query::repo_filter(store, repo)?,
            kinds: None,
        })
    }
}

fn main() -> ExitCode {
    let args: Vec<std::ffi::OsString> = std::env::args_os().collect();
    let json_requested = args.iter().any(|a| a == "--json");
    let cli = match Cli::try_parse_from(&args) {
        Ok(c) => c,
        Err(e) => {
            use clap::error::ErrorKind;
            if matches!(e.kind(), ErrorKind::DisplayHelp | ErrorKind::DisplayVersion) {
                let _ = e.print();
                return ExitCode::SUCCESS;
            }
            if json_requested {
                let msg = e.to_string();
                let first = msg.lines().next().unwrap_or("invalid arguments");
                let first = first.strip_prefix("error: ").unwrap_or(first);
                println!("{}", output::err_envelope("usage", first));
            } else {
                let _ = e.print();
            }
            return ExitCode::from(2);
        }
    };
    let json = cli.global.json;
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            let (code, exit) = match err.downcast_ref::<Error>() {
                Some(e) => (e.code(), e.exit_code()),
                None => ("internal", 1),
            };
            let msg = format!("{err:#}");
            if json {
                println!("{}", output::err_envelope(code, &msg));
            } else {
                eprintln!("codegraph: {msg}");
            }
            ExitCode::from(exit)
        }
    }
}

fn run(cli: Cli) -> anyhow::Result<()> {
    let (cfg, warnings) = config::load()?;
    if !cli.global.quiet {
        for w in &warnings {
            eprintln!("codegraph: warning: {w}");
        }
    }
    if cfg.threads > 0 {
        // Ignore "already initialized" (tests / repeated calls).
        let _ = rayon::ThreadPoolBuilder::new()
            .num_threads(cfg.threads)
            .build_global();
    }
    let is_tty = std::io::stdout().is_terminal();
    let no_color_env = std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty());
    let color = !cli.global.json
        && output::color_enabled(cfg.color, cli.global.no_color, no_color_env, is_tty);
    let app = App {
        json: cli.global.json,
        quiet: cli.global.quiet,
        verbose: cli.global.verbose,
        style: Style::new(color),
        cfg,
    };

    match cli.command {
        Command::Version => {
            if app.json {
                app.json_ok(&serde_json::json!({"name": "codegraph", "version": codegraph::VERSION, "commit": codegraph::COMMIT}))?;
            } else {
                app.print(&format!(
                    "codegraph {} ({})\n",
                    codegraph::VERSION,
                    codegraph::COMMIT
                ))?;
            }
        }
        Command::Config(c) => {
            let path = config::config_path();
            match c.action {
                ConfigAction::Path => {
                    if app.json {
                        app.json_ok(&serde_json::json!({"path": util::slash(&path), "exists": path.exists()}))?;
                    } else {
                        app.print(&format!("{}\n", path.display()))?;
                    }
                }
                ConfigAction::Show => {
                    let view = app.cfg.view(&path);
                    app.emit(&view, || render::config(&view))?;
                }
            }
        }
        Command::Mcp(a) => {
            let store = app.open_store()?;
            let watcher = if a.watch {
                Some(spawn_mcp_watcher(&app.cfg, store.path())?)
            } else {
                None
            };
            let cwd = std::env::current_dir().context("current directory")?;
            let mut server = Server::new(store, app.cfg.clone(), util::absolute(&cwd));
            let stdin = std::io::stdin();
            let stdout = std::io::stdout();
            let served = server
                .serve(stdin.lock(), stdout.lock())
                .context("mcp stdio");
            if let Some((stop, handle)) = watcher {
                stop.store(true, Ordering::SeqCst);
                let _ = handle.join();
            }
            served?;
        }
        Command::Watch(a) => {
            let mut store = app.open_store()?;
            let roots: Vec<PathBuf> = if a.paths.is_empty() {
                app.cfg.roots.clone()
            } else {
                a.paths.iter().map(|p| util::absolute(p)).collect()
            };
            let stop = Arc::new(AtomicBool::new(false));
            {
                let stop = Arc::clone(&stop);
                // First Ctrl-C: finish the current batch and exit 0. Second: exit now
                // (SQLite rolls back an interrupted write transaction).
                ctrlc::set_handler(move || {
                    if stop.swap(true, Ordering::SeqCst) {
                        std::process::exit(130);
                    }
                })
                .context("install Ctrl-C handler")?;
            }
            let debounce = Duration::from_millis(a.debounce_ms.max(10));
            let opts = WatchOptions {
                debounce,
                max_latency: debounce.max(Duration::from_secs(2)),
                initial_index: !a.no_initial,
                add_repos: true,
            };
            let mut emit = |e: &WatchEvent| {
                let res = if app.json {
                    serde_json::to_string(e)
                        .map_err(anyhow::Error::from)
                        .and_then(|l| {
                            app.print(&format!(
                                "{l}
"
                            ))
                        })
                } else {
                    match render::watch_event(e, app.verbose, app.style) {
                        Some(l) if matches!(e, WatchEvent::Error { .. }) => {
                            eprintln!("{l}");
                            Ok(())
                        }
                        Some(l) if !app.quiet || matches!(e, WatchEvent::Ready { .. }) => app
                            .print(&format!(
                                "{l}
"
                            )),
                        _ => Ok(()),
                    }
                };
                // stdout gone (pipe closed): stop watching.
                if res.is_err() {
                    stop.store(true, Ordering::SeqCst);
                }
            };
            watch::watch(&mut store, &app.cfg, &roots, &opts, &stop, &mut emit)?;
            if !app.json && !app.quiet {
                eprintln!("codegraph watch: stopped");
            }
        }
        Command::Index(a) => {
            let mut store = app.open_store()?;
            let opts = IndexOptions {
                full: a.full,
                budget: None,
            };
            let stats = if a.paths.is_empty() {
                index::index_all(&mut store, &app.cfg, &opts)?
            } else {
                let roots: Vec<_> = a.paths.iter().map(|p| util::absolute(p)).collect();
                index::index_roots(&mut store, &app.cfg, &roots, &opts)?
            };
            if app.json {
                app.json_ok(&stats)?;
            } else if !app.quiet {
                app.print(&render::index(&stats, app.verbose, app.style))?;
            }
        }
        Command::Find(a) => {
            let store = app.open_store()?;
            let mut filter = app.filter(&store, a.repo.repo.as_deref())?;
            if let Some(k) = &a.kind {
                filter.kinds = Some(SymKind::parse_filter(k).ok_or_else(|| {
                    Error::Usage(format!(
                        "unknown kind `{k}` (fn, method, class, struct, interface, trait, enum, type, types, const, var, module, macro)"
                    ))
                })?);
            }
            let hits = query::find(&store, &a.query, &filter, a.limit)?;
            if hits.is_empty() {
                return Err(
                    not_found_hint(&store, &format!("no symbols match `{}`", a.query))?.into(),
                );
            }
            if app.quiet && !app.json {
                let mut s = String::new();
                for h in &hits {
                    let _ = writeln!(s, "{}", h.sym.loc());
                }
                app.print(&s)?;
            } else {
                app.emit(&serde_json::json!({"query": a.query, "hits": hits}), || {
                    render::find(&hits, app.style)
                })?;
            }
        }
        Command::Def(a) => {
            let store = app.open_store()?;
            let filter = app.filter(&store, a.repo.repo.as_deref())?;
            let defs = query::def(&store, &a.symbol, &filter, a.max_lines, a.max_defs)
                .map_err(|e| hint_err(&store, e))?;
            if app.quiet && !app.json {
                let s = lines(defs.iter().map(|d| d.sym.loc()));
                app.print(&s)?;
            } else {
                app.emit(&serde_json::json!({"defs": defs}), || {
                    render::defs(&defs, app.style)
                })?;
            }
        }
        Command::Refs(a) => {
            let store = app.open_store()?;
            let filter = app.filter(&store, a.repo.repo.as_deref())?;
            let r = query::refs(&store, &a.symbol, &filter, a.kind.as_deref(), a.limit)
                .map_err(|e| hint_err(&store, e))?;
            if app.quiet && !app.json {
                let s = lines(r.refs.iter().map(|x| format!("{}:{}", x.file, x.line)));
                app.print(&s)?;
            } else {
                app.emit(&r, || render::refs(&r, app.style))?;
            }
        }
        Command::Callers(a) => {
            let store = app.open_store()?;
            let filter = app.filter(&store, a.repo.repo.as_deref())?;
            let t = query::call_tree(&store, &a.symbol, &filter, Dir::Callers, a.depth, true)
                .map_err(|e| hint_err(&store, e))?;
            app.emit(&t, || render::tree(&t, app.style))?;
        }
        Command::Callees(a) => {
            let store = app.open_store()?;
            let filter = app.filter(&store, a.repo.repo.as_deref())?;
            let t = query::call_tree(
                &store,
                &a.symbol,
                &filter,
                Dir::Callees,
                a.depth,
                !a.no_external,
            )
            .map_err(|e| hint_err(&store, e))?;
            app.emit(&t, || render::tree(&t, app.style))?;
        }
        Command::Outline(a) => {
            let store = app.open_store()?;
            let o = query::outline(
                &store,
                &a.file,
                &Filter::default(),
                Some(app.cfg.max_file_bytes()),
            )?;
            let sigs = a.signatures || app.verbose;
            app.emit(&o, || render::outline(&o, sigs, app.style))?;
        }
        Command::Path(a) => {
            let store = app.open_store()?;
            let filter = app.filter(&store, a.repo.repo.as_deref())?;
            let p = query::path(&store, &a.from, &a.to, &filter, a.max_depth)
                .map_err(|e| hint_err(&store, e))?;
            app.emit(&p, || render::path(&p, app.style))?;
        }
        Command::Impact(a) => {
            let store = app.open_store()?;
            let filter = app.filter(&store, a.repo.repo.as_deref())?;
            let i = query::impact(&store, &a.target, &filter, a.depth, a.limit)
                .map_err(|e| hint_err(&store, e))?;
            if app.quiet && !app.json {
                let s = lines(i.files.iter().map(|f| f.file.clone()));
                app.print(&s)?;
            } else {
                app.emit(&i, || render::impact(&i, app.style))?;
            }
        }
        Command::Hotspots(a) => {
            let store = app.open_store()?;
            let filter = app.filter(&store, a.repo.repo.as_deref())?;
            let h = query::hotspots(&store, &filter, a.limit)?;
            app.emit(&h, || render::hotspots(&h, app.style))?;
        }
        Command::Dot(a) => {
            let store = app.open_store()?;
            let filter = app.filter(&store, a.repo.repo.as_deref())?;
            let format = match a.format {
                GraphFormatArg::Dot => GraphFormat::Dot,
                GraphFormatArg::Mermaid => GraphFormat::Mermaid,
            };
            let g = query::graph(&store, &a.symbol, &filter, a.depth, format)
                .map_err(|e| hint_err(&store, e))?;
            app.emit(&serde_json::json!({"format": format!("{:?}", a.format).to_lowercase(), "graph": g}), || g.clone())?;
        }
        Command::Status(a) => {
            let store = app.open_store()?;
            let filter = app.filter(&store, a.repo.as_deref())?;
            let s = query::status(&store, &filter)?;
            app.emit(&s, || render::status(&s, app.style))?;
        }
    }
    Ok(())
}

/// `mcp --watch`: a watcher thread with its own connection to the graph. It keeps repos
/// already in the graph fresh (it never adds repos, like the per-call refresh).
fn spawn_mcp_watcher(
    cfg: &Config,
    db: &Path,
) -> anyhow::Result<(Arc<AtomicBool>, std::thread::JoinHandle<()>)> {
    let mut store = Store::open(db)?;
    let cfg = cfg.clone();
    let stop = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&stop);
    let handle = std::thread::Builder::new()
        .name("codegraph-watch".into())
        .spawn(move || {
            let roots: Vec<PathBuf> = cfg.roots.iter().filter(|r| r.is_dir()).cloned().collect();
            if roots.is_empty() {
                eprintln!("codegraph mcp: watch: no configured root exists; not watching");
                return;
            }
            let opts = WatchOptions {
                initial_index: false,
                add_repos: false,
                ..WatchOptions::default()
            };
            let mut emit = |e: &WatchEvent| {
                if let Some(l) = render::watch_event(e, false, Style::plain()) {
                    eprintln!("codegraph mcp: watch: {l}");
                }
            };
            if let Err(e) = watch::watch(&mut store, &cfg, &roots, &opts, &flag, &mut emit) {
                eprintln!("codegraph mcp: watch stopped: {e}");
            }
        })
        .context("spawn watcher thread")?;
    Ok((stop, handle))
}

/// One item per line.
fn lines(items: impl Iterator<Item = String>) -> String {
    let mut s = String::new();
    for i in items {
        s.push_str(&i);
        s.push('\n');
    }
    s
}

/// Turn "not found" on an empty graph into an actionable hint.
fn not_found_hint(store: &Store, msg: &str) -> anyhow::Result<Error> {
    let empty = store.repos()?.is_empty();
    Ok(Error::NotFound(if empty {
        format!("{msg}; the graph is empty — run `codegraph index` first")
    } else {
        msg.to_string()
    }))
}

fn hint_err(store: &Store, e: Error) -> anyhow::Error {
    match e {
        Error::NotFound(m) => match not_found_hint(store, &m) {
            Ok(x) => x.into(),
            Err(e) => e,
        },
        other => other.into(),
    }
}

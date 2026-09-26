//! `watch`: file changes under a watched root reach the graph without an explicit
//! `index`, while other connections read and write the same database.
//!
//! Timing-sensitive by nature: every wait polls with a generous deadline instead of
//! sleeping a fixed time, and nothing asserts on durations.
#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]

mod common;

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use codegraph::index::{self, IndexOptions};
use codegraph::store::Store;
use codegraph::watch::{self, WatchEvent, WatchOptions};
use common::test_config;
use serde_json::Value;

const DEADLINE: Duration = Duration::from_secs(10);

struct Watching {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<codegraph::error::Result<()>>>,
    events: mpsc::Receiver<WatchEvent>,
}

impl Watching {
    fn start(root: &Path, db: &Path) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, events) = mpsc::channel();
        let (root, db, flag) = (root.to_path_buf(), db.to_path_buf(), Arc::clone(&stop));
        let handle = std::thread::spawn(move || {
            let mut store = Store::open(&db)?;
            let opts = WatchOptions {
                debounce: Duration::from_millis(100),
                ..WatchOptions::default()
            };
            let mut emit = |e: &WatchEvent| {
                let _ = tx.send(e.clone());
            };
            watch::watch(&mut store, &test_config(), &[root], &opts, &flag, &mut emit)
        });
        let w = Self {
            stop,
            handle: Some(handle),
            events,
        };
        w.wait_event(|e| matches!(e, WatchEvent::Ready { .. }), "ready");
        w
    }

    fn wait_event(&self, pred: impl Fn(&WatchEvent) -> bool, what: &str) -> WatchEvent {
        let end = Instant::now() + Duration::from_secs(30);
        while Instant::now() < end {
            if let Ok(e) = self.events.recv_timeout(Duration::from_millis(200)) {
                if let WatchEvent::Error { message, .. } = &e {
                    panic!("watch error: {message}");
                }
                if pred(&e) {
                    return e;
                }
            }
        }
        panic!("no {what} event within 30 s");
    }

    fn stop(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let h = self.handle.take().unwrap();
        let end = Instant::now() + Duration::from_secs(10);
        while !h.is_finished() {
            assert!(Instant::now() < end, "watcher did not stop within 10 s");
            std::thread::sleep(Duration::from_millis(20));
        }
        h.join().unwrap().unwrap();
    }
}

impl Drop for Watching {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

fn count(db: &Path, sql: &str, arg: &str) -> i64 {
    let store = Store::open(db).unwrap();
    store
        .conn()
        .query_row(sql, [arg], |r| r.get(0))
        .unwrap_or(-1)
}

fn symbols_named(db: &Path, name: &str) -> i64 {
    count(db, "SELECT count(*) FROM symbols WHERE name = ?1", name)
}

/// Poll until `f` holds (up to [`DEADLINE`]).
fn eventually(what: &str, f: impl Fn() -> bool) {
    let end = Instant::now() + DEADLINE;
    loop {
        if f() {
            return;
        }
        assert!(Instant::now() < end, "timed out waiting for: {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn repo(dir: &Path) -> PathBuf {
    std::fs::create_dir_all(dir.join(".git")).unwrap();
    dir.to_path_buf()
}

#[test]
fn edit_create_and_delete_are_picked_up() {
    let tmp = tempfile::tempdir().unwrap();
    let root = repo(&tmp.path().join("proj"));
    std::fs::write(root.join("a.go"), "package a\n\nfunc Old() {}\n").unwrap();
    let dbdir = tempfile::tempdir().unwrap();
    let db = dbdir.path().join("graph.db");

    let w = Watching::start(&root, &db);
    // The initial index ran before `ready`.
    assert_eq!(symbols_named(&db, "Old"), 1);

    // Edit.
    std::fs::write(
        root.join("a.go"),
        "package a\n\nfunc Old() {}\n\nfunc Added() { Old() }\n",
    )
    .unwrap();
    eventually("edited symbol appears", || symbols_named(&db, "Added") == 1);

    // Create.
    std::fs::create_dir_all(root.join("sub")).unwrap();
    std::fs::write(root.join("sub/b.go"), "package sub\n\nfunc Fresh() {}\n").unwrap();
    eventually("new file appears", || symbols_named(&db, "Fresh") == 1);

    // Delete.
    std::fs::remove_file(root.join("a.go")).unwrap();
    eventually("deleted file's symbols disappear", || {
        symbols_named(&db, "Old") == 0 && symbols_named(&db, "Added") == 0
    });
    assert_eq!(symbols_named(&db, "Fresh"), 1);

    // Build output and credential files never reach the graph.
    std::fs::create_dir_all(root.join("target/debug")).unwrap();
    std::fs::write(
        root.join("target/debug/gen.go"),
        "package gen\nfunc Gen() {}\n",
    )
    .unwrap();
    std::fs::write(root.join(".env.go"), "package a\nfunc Secret() {}\n").unwrap();
    // A real change after them proves their batch has been processed.
    std::fs::write(root.join("sub/c.go"), "package sub\n\nfunc Later() {}\n").unwrap();
    eventually("later file appears", || symbols_named(&db, "Later") == 1);
    assert_eq!(symbols_named(&db, "Gen"), 0);
    assert_eq!(symbols_named(&db, "Secret"), 0);

    w.stop();
}

#[test]
fn new_repo_under_root_is_added() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("dev");
    let one = repo(&root.join("one"));
    std::fs::write(one.join("m.py"), "def first():\n    pass\n").unwrap();
    let dbdir = tempfile::tempdir().unwrap();
    let db = dbdir.path().join("graph.db");

    let w = Watching::start(&root, &db);
    assert_eq!(symbols_named(&db, "first"), 1);

    let two = root.join("two");
    std::fs::create_dir_all(&two).unwrap();
    std::fs::write(two.join("n.py"), "def second():\n    pass\n").unwrap();
    std::fs::create_dir_all(two.join(".git")).unwrap();
    eventually("new repo is indexed", || {
        symbols_named(&db, "second") == 1
            && count(&db, "SELECT count(*) FROM repos WHERE name = ?1", "two") == 1
    });
    w.stop();
}

#[test]
fn concurrent_writer_waits_instead_of_failing() {
    let src = common::copy_fixture("go");
    let dbdir = tempfile::tempdir().unwrap();
    let db = dbdir.path().join("graph.db");
    let mut store = Store::open(&db).unwrap();

    // Another connection (another process in real life) holds the write lock for a while.
    let (locked_tx, locked_rx) = mpsc::channel();
    let db2 = db.clone();
    let holder = std::thread::spawn(move || {
        let other = Store::open(&db2).unwrap();
        other.conn().execute_batch("BEGIN IMMEDIATE").unwrap();
        other
            .conn()
            .execute(
                "INSERT OR REPLACE INTO meta(key, value) VALUES ('probe', '1')",
                [],
            )
            .unwrap();
        locked_tx.send(()).unwrap();
        std::thread::sleep(Duration::from_millis(700));
        other.conn().execute_batch("COMMIT").unwrap();
    });
    locked_rx.recv().unwrap();
    let st = index::index_roots(
        &mut store,
        &test_config(),
        &[src.path().to_path_buf()],
        &IndexOptions::default(),
    )
    .expect("the busy timeout makes the second writer wait");
    assert_eq!(st.parsed, 3);
    holder.join().unwrap();
}

#[test]
fn cli_watch_streams_ndjson() {
    let tmp = tempfile::tempdir().unwrap();
    let root = repo(&tmp.path().join("proj"));
    std::fs::write(root.join("lib.rs"), "pub fn alpha() {}\n").unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_codegraph"))
        .args(["watch", "--json", "--debounce-ms", "100"])
        .arg(&root)
        .env("KIT_CONFIG_DIR", cfg.path())
        .env("KIT_DATA_DIR", data.path())
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let next = |want: &str| -> Value {
        let end = Instant::now() + Duration::from_secs(30);
        while Instant::now() < end {
            if let Ok(line) = rx.recv_timeout(Duration::from_millis(200)) {
                let v: Value = serde_json::from_str(&line)
                    .unwrap_or_else(|e| panic!("not NDJSON ({e}): {line}"));
                assert!(v["type"].is_string(), "{line}");
                if v["type"] == want {
                    return v;
                }
            }
        }
        panic!("no `{want}` line within 30 s");
    };

    let ready = next("ready");
    assert_eq!(ready["repos"], 1);
    assert_eq!(ready["initial"]["parsed"], 1);

    std::fs::write(
        root.join("lib.rs"),
        "pub fn alpha() {}\npub fn beta() { alpha() }\n",
    )
    .unwrap();
    let re = next("reindex");
    assert_eq!(re["repo"], "proj");
    assert_eq!(re["stats"]["parsed"], 1);
    assert!(re["files"]
        .as_array()
        .unwrap()
        .iter()
        .any(|f| f == "lib.rs"));
    assert!(re["ms"].is_u64());

    let _ = child.kill();
    let _ = child.wait();
    let data_db = data.path().join("codegraph").join("graph.db");
    assert_eq!(symbols_named(&data_db, "beta"), 1);
}

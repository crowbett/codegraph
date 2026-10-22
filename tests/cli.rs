//! End-to-end tests of the binary: `--json` envelopes, exit codes, config paths,
//! an MCP stdio session, and codegraph indexing its own repo.
#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]

mod common;

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command, Stdio};

use serde_json::{json, Value};

struct Env {
    cfg: tempfile::TempDir,
    data: tempfile::TempDir,
}

impl Env {
    fn new() -> Self {
        Self {
            cfg: tempfile::tempdir().unwrap(),
            data: tempfile::tempdir().unwrap(),
        }
    }

    fn cmd(&self) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_codegraph"));
        c.env("KIT_CONFIG_DIR", self.cfg.path())
            .env("KIT_DATA_DIR", self.data.path())
            .env("NO_COLOR", "1");
        c
    }

    /// Run with `--json`; returns (exit code, parsed stdout).
    fn json(&self, args: &[&str]) -> (i32, Value) {
        let out = self.cmd().args(args).arg("--json").output().unwrap();
        let stdout = String::from_utf8(out.stdout).unwrap();
        let v: Value = serde_json::from_str(stdout.trim())
            .unwrap_or_else(|e| panic!("{args:?}: not one JSON document ({e}): {stdout}"));
        (out.status.code().unwrap_or(-1), v)
    }

    fn text(&self, args: &[&str]) -> (i32, String) {
        let out = self.cmd().args(args).output().unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8(out.stdout).unwrap(),
        )
    }
}

fn fixture(name: &str) -> String {
    common::fixture(name).to_string_lossy().to_string()
}

#[test]
fn json_envelopes_and_exit_codes() {
    let env = Env::new();
    let (code, v) = env.json(&["index", &fixture("go")]);
    assert_eq!(code, 0);
    assert_eq!(v["ok"], true);
    assert_eq!(v["data"]["files"], 3);

    let (code, v) = env.json(&["find", "Helper"]);
    assert_eq!(code, 0);
    let hits = v["data"]["hits"].as_array().unwrap();
    let exact: Vec<&str> = hits
        .iter()
        .filter(|h| h["match"] == "exact")
        .map(|h| h["qname"].as_str().unwrap())
        .collect();
    assert_eq!(exact, ["a.Helper", "b.Helper"]);
    // Case-insensitive matches rank after exact ones.
    assert_eq!(hits[2]["qname"], "main.helper");

    let (code, v) = env.json(&["def", "a.Store.Get"]);
    assert_eq!(code, 0);
    let src = v["data"]["defs"][0]["source"].as_str().unwrap();
    assert!(
        src.contains("func (s *Store) Get(k string) string"),
        "{src}"
    );

    let (code, v) = env.json(&["callers", "Helper", "--depth", "1"]);
    assert_eq!(code, 0);
    assert_eq!(v["data"]["roots"].as_array().unwrap().len(), 2);

    let (code, v) = env.json(&["refs", "a.Store"]);
    assert_eq!(code, 0);
    assert!(v["data"]["total"].as_u64().unwrap() >= 3);

    let (code, v) = env.json(&["find", "zzzqqq"]);
    assert_eq!(code, 3);
    assert_eq!(v["ok"], false);
    assert_eq!(v["error"]["code"], "not_found");

    let (code, v) = env.json(&["find", "x", "--kind", "bogus"]);
    assert_eq!(code, 2);
    assert_eq!(v["error"]["code"], "usage");

    let (code, v) = env.json(&["nonsense"]);
    assert_eq!(code, 2);
    assert_eq!(v["error"]["code"], "usage");

    let (code, v) = env.json(&["status"]);
    assert_eq!(code, 0);
    assert_eq!(v["data"]["repos"], 1);

    let (code, v) = env.json(&["hotspots"]);
    assert_eq!(code, 0);
    assert!(v["data"]["most_referenced"].is_array());
}

#[test]
fn human_output_and_meta_commands() {
    let env = Env::new();
    let (code, out) = env.text(&["version"]);
    assert_eq!(code, 0);
    assert!(
        out.starts_with(concat!("codegraph ", env!("CARGO_PKG_VERSION"), " (")),
        "{out}"
    );

    let (code, v) = env.json(&["config", "path"]);
    assert_eq!(code, 0);
    let p = v["data"]["path"].as_str().unwrap();
    assert!(p.ends_with("codegraph.toml"), "{p}");

    std::fs::write(
        env.cfg.path().join("codegraph.toml"),
        "max_file_kb = 10\nbogus = 1\n",
    )
    .unwrap();
    let out = env.cmd().args(["config", "show"]).output().unwrap();
    assert!(out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("unknown config key `bogus`"), "{stderr}");
    assert!(String::from_utf8_lossy(&out.stdout).contains("max_file_kb       10"));

    let (code, _) = env.text(&["index", &fixture("graph")]);
    assert_eq!(code, 0);
    let (code, out) = env.text(&["callers", "c", "--depth", "3"]);
    assert_eq!(code, 0);
    assert!(out.contains("[cycle]"), "{out}");
    assert!(!out.contains('\x1b'), "no ANSI when not a TTY");
    let (code, out) = env.text(&["outline", &format!("{}/main.go", fixture("graph"))]);
    assert_eq!(code, 0);
    assert!(out.contains("fn orphan"), "{out}");
    let (code, out) = env.text(&["dot", "c", "--format", "mermaid"]);
    assert_eq!(code, 0);
    assert!(out.starts_with("flowchart LR"));
    let (code, out) = env.text(&["path", "main", "d"]);
    assert_eq!(code, 0);
    assert!(out.contains("3 hop(s)"), "{out}");
    let (code, out) = env.text(&["impact", "d", "-q"]);
    assert_eq!(code, 0);
    assert!(out.trim().ends_with("main.go"), "{out}");
}

fn send(stdin: &mut impl Write, v: &Value) {
    writeln!(stdin, "{v}").unwrap();
    stdin.flush().unwrap();
}

fn recv(lines: &mut impl Iterator<Item = std::io::Result<String>>) -> Value {
    let line = lines.next().expect("server closed").unwrap();
    serde_json::from_str(&line).unwrap()
}

#[test]
fn mcp_stdio_session_auto_refreshes_indexed_repo() {
    let env = Env::new();
    let repo = common::copy_fixture("go");
    std::fs::create_dir_all(repo.path().join(".git")).unwrap();
    // The repo is a configured root and indexed by the CLI; MCP never adds repos.
    let root = repo.path().to_string_lossy().replace('\\', "/");
    std::fs::write(
        env.cfg.path().join("codegraph.toml"),
        format!("roots = ['{root}']\n"),
    )
    .unwrap();
    let (code, _) = env.json(&["index"]);
    assert_eq!(code, 0);
    // An edit after indexing must be picked up by the MCP auto-refresh.
    let b = repo.path().join("b/b.go");
    let mut text = std::fs::read_to_string(&b).unwrap();
    text.push_str("\n// FreshlyAdded appears after indexing.\nfunc FreshlyAdded() {}\n");
    std::fs::write(&b, text).unwrap();

    let mut child = env
        .cmd()
        .arg("mcp")
        .current_dir(repo.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();

    send(
        &mut stdin,
        &json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"0"}}}),
    );
    let r = recv(&mut lines);
    assert_eq!(r["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(r["result"]["serverInfo"]["name"], "codegraph");
    send(
        &mut stdin,
        &json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
    );

    send(
        &mut stdin,
        &json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
    );
    let r = recv(&mut lines);
    let tools = r["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 8);
    assert!(tools
        .iter()
        .all(|t| t["name"].as_str().unwrap().starts_with("codegraph_")));

    send(
        &mut stdin,
        &json!({"jsonrpc":"2.0","id":30,"method":"tools/call","params":{"name":"codegraph_find","arguments":{"query":"FreshlyAdded"}}}),
    );
    let r = recv(&mut lines);
    assert_eq!(
        r["result"]["structuredContent"]["hits"][0]["qname"], "b.FreshlyAdded",
        "{r}"
    );

    send(
        &mut stdin,
        &json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"codegraph_find","arguments":{"query":"NewStore"}}}),
    );
    let r = recv(&mut lines);
    assert_eq!(r["result"]["isError"], false, "{r}");
    assert_eq!(
        r["result"]["structuredContent"]["hits"][0]["qname"],
        "a.NewStore"
    );
    let text = r["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("a/a.go:10"), "{text}");

    send(
        &mut stdin,
        &json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"codegraph_callers","arguments":{"symbol":"a.Store.Get"}}}),
    );
    let r = recv(&mut lines);
    let callers: Vec<&str> = r["result"]["structuredContent"]["roots"][0]["children"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["qname"].as_str().unwrap())
        .collect();
    assert_eq!(callers, ["b.Use", "main.main"]);

    send(
        &mut stdin,
        &json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"codegraph_outline","arguments":{"file":"a/a.go"}}}),
    );
    let r = recv(&mut lines);
    let text = r["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("method Store.Get"), "{text}");
    assert!(
        text.contains("func (s *Store) Get(k string) string"),
        "{text}"
    );

    send(
        &mut stdin,
        &json!({"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"codegraph_def","arguments":{"symbol":"nope_nothing"}}}),
    );
    let r = recv(&mut lines);
    assert_eq!(r["result"]["isError"], true);

    drop(stdin);
    let status = child.wait().unwrap();
    assert!(status.success());
}

/// A client that closes the server's stdout mid-session (its own exit, a crash) is a
/// normal shutdown: the server exits 0 instead of failing on the broken pipe.
#[test]
fn mcp_exits_zero_when_client_closes_stdout() {
    let env = Env::new();
    let mut child = env
        .cmd()
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let mut lines = BufReader::new(&mut stdout).lines();
    send(&mut stdin, &json!({"jsonrpc":"2.0","id":1,"method":"ping"}));
    assert_eq!(recv(&mut lines)["result"], json!({}));
    // Close our end of the server's stdout, then make it write a response.
    drop(lines);
    drop(stdout);
    send(&mut stdin, &json!({"jsonrpc":"2.0","id":2,"method":"ping"}));
    // Writing a second request may itself fail once the server has exited; that is fine.
    let _ = writeln!(stdin, "{}", json!({"jsonrpc":"2.0","id":3,"method":"ping"}));
    drop(stdin);
    let out = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "stderr: {stderr}");
    assert!(stderr.contains("client closed the pipe"), "{stderr}");
}

#[test]
fn self_index_finds_main() {
    let env = Env::new();
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let (code, v) = env.json(&["index", &root.to_string_lossy()]);
    assert_eq!(code, 0, "{v}");
    let (code, v) = env.json(&["find", "main", "--kind", "fn", "--limit", "50"]);
    assert_eq!(code, 0);
    let hits = v["data"]["hits"].as_array().unwrap();
    assert!(
        hits.iter()
            .any(|h| h["file"].as_str().unwrap().ends_with("src/main.rs") && h["name"] == "main"),
        "{v}"
    );
    // The resolver links main -> run inside the binary.
    let main_rs = std::fs::read_to_string(root.join("src/main.rs")).unwrap();
    let line = main_rs
        .lines()
        .position(|l| l.starts_with("fn main()"))
        .unwrap()
        + 1;
    let spec = format!("src/main.rs:{line}");
    let (code, v) = env.json(&["callees", &spec, "--no-external"]);
    assert_eq!(code, 0, "{v}");
    let names: Vec<&str> = v["data"]["roots"][0]["children"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|c| c["name"].as_str())
        .collect();
    assert!(names.contains(&"run"), "{names:?}");
}

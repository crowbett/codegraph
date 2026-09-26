//! `codegraph mcp`: MCP server over stdio (JSON-RPC 2.0, newline-delimited).
//!
//! Read-only tools only. Before answering, the repo being asked about (explicit
//! `repo`, the repo containing a `file`/`target` path, or the repo containing the
//! server's working directory) is re-indexed incrementally within a time budget,
//! so answers track the working tree. Logs go to stderr; stdout is the protocol.

use std::collections::HashMap;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{json, Value};

use crate::config::Config;
use crate::error::Error;
use crate::index::{self, IndexOptions};
use crate::model::SymKind;
use crate::output::Style;
use crate::query::{self, Dir, Filter};
use crate::store::Store;
use crate::{render, util};

/// MCP protocol revision implemented.
pub const PROTOCOL_VERSION: &str = "2025-06-18";

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;

/// Minimum gap between two refreshes of the same repo.
const REFRESH_EVERY: Duration = Duration::from_secs(3);

const INSTRUCTIONS: &str = "codegraph is a precomputed symbol + call graph (tree-sitter, heuristic resolution) of the user's repos. \
Prefer it over grep for structural questions: where is X defined (codegraph_def), who calls X (codegraph_callers), \
what does X call (codegraph_callees), what breaks if I change this file/function (codegraph_impact), \
what is in this file (codegraph_outline). Symbols accept a simple name, a qualified name (pkg.Type.Method, mod::Type::method), \
or file:line. Edges carry confidence: exact, likely (~), name_only (?) — verify name_only edges by reading the code. \
The repo you ask about is re-indexed automatically before each answer.";

/// Tool definitions for `tools/list`.
pub fn tool_list() -> Value {
    let ro = json!({"readOnlyHint": true, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false});
    let repo = json!({"type": "string", "description": "Restrict to one repo: name as shown by codegraph_status (e.g. \"go/pulse\"), its last segment (\"pulse\"), or its root path. Defaults to all repos."});
    let symbol = json!({"type": "string", "description": "Symbol: simple name (\"Evaluate\"), qualified name (\"rules.Engine.Evaluate\", \"store::Store::open\"), or location \"path/to/file.go:42\" (the innermost symbol enclosing that line)."});
    json!([
        {
            "name": "codegraph_find",
            "title": "Find symbols",
            "description": "Search function/type/const definitions by name across all indexed repos. Ranks exact name > qualified-name suffix > prefix > substring > doc-comment text > fuzzy subsequence. Returns kind, qualified name, file:line and signature. Use this first when you only know part of a name; then codegraph_def to read it.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "Name, qualified name, or fragment (e.g. \"Evaluate\", \"Engine.Eval\", \"hndlreq\")."},
                    "kind": {"type": "string", "description": "Optional kind filter: fn (functions+methods), method, class, struct, interface, trait, enum, type, types (all type-like), const, module, macro."},
                    "repo": repo,
                    "limit": {"type": "integer", "minimum": 1, "maximum": 200, "default": 20}
                },
                "required": ["query"],
                "additionalProperties": false
            },
            "annotations": ro
        },
        {
            "name": "codegraph_def",
            "title": "Go to definition",
            "description": "Definition of a symbol with its source code (plus the doc comment above it), line-numbered, and its exact file:line range. Cheaper and more precise than reading the whole file. Ambiguous names return up to `max_defs` definitions.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "symbol": symbol,
                    "repo": repo,
                    "max_lines": {"type": "integer", "minimum": 1, "maximum": 1000, "default": 80, "description": "Max source lines per definition."},
                    "max_defs": {"type": "integer", "minimum": 1, "maximum": 10, "default": 3}
                },
                "required": ["symbol"],
                "additionalProperties": false
            },
            "annotations": ro
        },
        {
            "name": "codegraph_refs",
            "title": "Find references",
            "description": "Every place a symbol is used: calls, type references, impls/implements, inheritance — with file:line, the enclosing function, the source line, and resolution confidence. Use before renaming or changing a signature.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "symbol": symbol,
                    "kind": {"type": "string", "enum": ["call", "ref", "impl", "inherit"], "description": "Only this reference kind."},
                    "repo": repo,
                    "limit": {"type": "integer", "minimum": 1, "maximum": 1000, "default": 60}
                },
                "required": ["symbol"],
                "additionalProperties": false
            },
            "annotations": ro
        },
        {
            "name": "codegraph_callers",
            "title": "Callers (call hierarchy up)",
            "description": "Who calls this function/method, as a tree `depth` levels up (callers of callers...). Each node shows the caller's qualified name and the call site file:line; cycles and repeats are marked. Answers \"how is this reached?\".",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "symbol": symbol,
                    "depth": {"type": "integer", "minimum": 1, "maximum": 6, "default": 2},
                    "repo": repo
                },
                "required": ["symbol"],
                "additionalProperties": false
            },
            "annotations": ro
        },
        {
            "name": "codegraph_callees",
            "title": "Callees (call hierarchy down)",
            "description": "What this function/method calls, as a tree `depth` levels down. In-repo callees show their definition file:line; calls into the stdlib/dependencies are listed as external names. Answers \"what does this do / touch?\".",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "symbol": symbol,
                    "depth": {"type": "integer", "minimum": 1, "maximum": 6, "default": 1},
                    "external": {"type": "boolean", "default": true, "description": "Include calls that resolve outside the graph."},
                    "repo": repo
                },
                "required": ["symbol"],
                "additionalProperties": false
            },
            "annotations": ro
        },
        {
            "name": "codegraph_outline",
            "title": "File outline",
            "description": "Nested outline of one source file: types, functions, methods, consts with line ranges and signatures. Use instead of reading a large file to decide which part to read. Works on unindexed files too (parsed on the fly).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "file": {"type": "string", "description": "Absolute path, path relative to the working directory, or a repo-relative suffix like \"internal/rules/engine.go\"."},
                    "signatures": {"type": "boolean", "default": true}
                },
                "required": ["file"],
                "additionalProperties": false
            },
            "annotations": ro
        },
        {
            "name": "codegraph_impact",
            "title": "Change impact",
            "description": "What transitively depends on a file or symbol — \"what breaks if I change X\". Walks reverse call/type/impl/inherit edges `depth` levels and lists dependent symbols by distance plus the affected files (and, for a file, the files importing it). Use before refactors to find code and tests to update.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "target": {"type": "string", "description": "A file path (every symbol in it) or a symbol (name, qualified name, file:line)."},
                    "depth": {"type": "integer", "minimum": 1, "maximum": 8, "default": 3},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 1000, "default": 100},
                    "repo": repo
                },
                "required": ["target"],
                "additionalProperties": false
            },
            "annotations": ro
        },
        {
            "name": "codegraph_status",
            "title": "Graph status",
            "description": "Which repos are indexed, their size (files/symbols/edges), share of edges resolved and by what confidence, and when each was last indexed.",
            "inputSchema": {
                "type": "object",
                "properties": {"repo": repo},
                "additionalProperties": false
            },
            "annotations": ro
        }
    ])
}

enum CallError {
    Rpc(i64, String),
    Tool(Error),
}

impl From<Error> for CallError {
    fn from(e: Error) -> Self {
        Self::Tool(e)
    }
}

/// Hard cap on one tool response (text + structuredContent), in bytes.
pub const MAX_RESPONSE_BYTES: usize = 256 * 1024;
/// When structuredContent is present, the text is only a summary of this size.
const SUMMARY_TEXT_BYTES: usize = 16 * 1024;

/// Cut `s` to at most `max` bytes on a char boundary, with a marker.
fn cap_text(s: &str, max: usize, why: &str) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n… truncated ({} of {} bytes shown; {why})",
        &s[..end],
        end,
        s.len()
    )
}

/// Tool result with bounded size: long lines clipped, the text a summary when the
/// structured data carries the full answer, and oversized structured data replaced
/// by a truncation notice.
#[allow(clippy::needless_pass_by_value)]
fn structured<T: Serialize>(data: &T, text: String) -> Result<Value, CallError> {
    let v =
        serde_json::to_value(data).map_err(|e| CallError::Tool(Error::Internal(e.to_string())))?;
    let text: String = text
        .lines()
        .map(query::clip_line)
        .collect::<Vec<_>>()
        .join("\n");
    let json_len = serde_json::to_string(&v).map_or(usize::MAX, |s| s.len());
    let (v, text) = if json_len + SUMMARY_TEXT_BYTES > MAX_RESPONSE_BYTES {
        let notice = json!({
            "truncated": true,
            "bytes": json_len,
            "hint": "result too large; narrow the query (limit, depth, max_lines, repo)"
        });
        let t = cap_text(
            &text,
            MAX_RESPONSE_BYTES - SUMMARY_TEXT_BYTES,
            "narrow the query: limit, depth, max_lines, repo",
        );
        (notice, t)
    } else {
        let t = cap_text(
            &text,
            SUMMARY_TEXT_BYTES,
            "full result in structuredContent",
        );
        (v, t)
    };
    Ok(json!({
        "content": [{"type": "text", "text": text}],
        "structuredContent": v,
        "isError": false
    }))
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
}

fn req_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, CallError> {
    str_arg(args, key).ok_or_else(|| CallError::Rpc(INVALID_PARAMS, format!("`{key}` is required")))
}

fn uint_arg(args: &Value, key: &str, default: u64, max: u64) -> u64 {
    args.get(key)
        .and_then(Value::as_u64)
        .unwrap_or(default)
        .clamp(1, max)
}

fn bool_arg(args: &Value, key: &str, default: bool) -> bool {
    args.get(key).and_then(Value::as_bool).unwrap_or(default)
}

/// Stateful MCP server.
pub struct Server {
    store: Store,
    cfg: Config,
    cwd: PathBuf,
    /// Canonical configured roots: every path argument must resolve under one.
    roots: Vec<PathBuf>,
    last_refresh: HashMap<PathBuf, Instant>,
}

impl Server {
    /// New server over an open store.
    pub fn new(store: Store, cfg: Config, cwd: PathBuf) -> Self {
        let roots = util::canonical_roots(&cfg.roots);
        Self {
            store,
            cfg,
            cwd,
            roots,
            last_refresh: HashMap::new(),
        }
    }

    fn filter(&self, args: &Value) -> Result<Filter, CallError> {
        Ok(Filter {
            repos: query::repo_filter(&self.store, str_arg(args, "repo"))?,
            kinds: None,
        })
    }

    /// Repos to refresh for a call: explicit repo, repo of a path argument, or the cwd repo.
    fn refresh_targets(&self, args: &Value) -> Vec<(PathBuf, String)> {
        let mut out = Vec::new();
        if let Some(r) = str_arg(args, "repo") {
            if let Ok(Some(ids)) = query::repo_filter(&self.store, Some(r)) {
                if let Ok(repos) = self.store.repos() {
                    for repo in repos.into_iter().filter(|x| ids.contains(&x.id)) {
                        out.push((repo.root, repo.name));
                    }
                }
            }
            return out;
        }
        // Only repos already in the graph are refreshed: MCP never adds repos, and
        // path arguments must resolve under a configured root.
        let anchor = str_arg(args, "file")
            .or_else(|| str_arg(args, "target"))
            .and_then(|p| util::confine(p, &self.roots).ok())
            .unwrap_or_else(|| self.cwd.clone());
        if let Ok(Some((_, name, root))) = query::repo_containing(&self.store, &anchor) {
            out.push((root, name));
        }
        out
    }

    fn refresh(&mut self, args: &Value) {
        if self.cfg.refresh_budget_ms == 0 {
            return;
        }
        let budget = Duration::from_millis(self.cfg.refresh_budget_ms);
        for (root, name) in self.refresh_targets(args) {
            if self
                .last_refresh
                .get(&root)
                .is_some_and(|t| t.elapsed() < REFRESH_EVERY)
            {
                continue;
            }
            let opts = IndexOptions {
                full: false,
                budget: Some(budget),
            };
            match index::refresh_repo(&mut self.store, &self.cfg, &root, &name, &opts) {
                Ok(st) => {
                    if st.parsed > 0 || st.removed > 0 {
                        eprintln!(
                            "codegraph mcp: refreshed {name}: {} parsed, {} removed, {} pending ({} ms)",
                            st.parsed, st.removed, st.pending, st.elapsed_ms
                        );
                    }
                    if st.pending == 0 {
                        self.last_refresh.insert(root, Instant::now());
                    }
                }
                Err(e) => eprintln!("codegraph mcp: refresh {name} failed: {e}"),
            }
        }
    }

    fn call_tool(&mut self, name: &str, args: &Value) -> Result<Value, CallError> {
        if name != "codegraph_status" {
            self.refresh(args);
        }
        let plain = Style::plain();
        match name {
            "codegraph_find" => {
                let q = req_str(args, "query")?;
                let mut filter = self.filter(args)?;
                if let Some(k) = str_arg(args, "kind") {
                    filter.kinds = Some(SymKind::parse_filter(k).ok_or_else(|| {
                        CallError::Rpc(INVALID_PARAMS, format!("unknown kind `{k}`"))
                    })?);
                }
                let limit = uint_arg(args, "limit", 20, 200) as usize;
                let hits = query::find(&self.store, q, &filter, limit)?;
                let text = if hits.is_empty() {
                    format!("no symbols match `{q}`")
                } else {
                    render::find(&hits, plain)
                };
                structured(&json!({"query": q, "hits": hits}), text)
            }
            "codegraph_def" => {
                let sym = req_str(args, "symbol")?;
                let filter = self.filter(args)?;
                let max_lines = uint_arg(args, "max_lines", 80, 1000) as u32;
                let max_defs = uint_arg(args, "max_defs", 3, 10) as usize;
                let defs = query::def(&self.store, sym, &filter, max_lines, max_defs)?;
                let text = render::defs(&defs, plain);
                structured(&json!({"defs": defs}), text)
            }
            "codegraph_refs" => {
                let sym = req_str(args, "symbol")?;
                let filter = self.filter(args)?;
                let limit = uint_arg(args, "limit", 60, 1000) as usize;
                let r = query::refs(&self.store, sym, &filter, str_arg(args, "kind"), limit)?;
                let text = render::refs(&r, plain);
                structured(&r, text)
            }
            "codegraph_callers" | "codegraph_callees" => {
                let sym = req_str(args, "symbol")?;
                let filter = self.filter(args)?;
                let (dir, default_depth) = if name == "codegraph_callers" {
                    (Dir::Callers, 2)
                } else {
                    (Dir::Callees, 1)
                };
                let depth = uint_arg(args, "depth", default_depth, 6) as u32;
                let external = bool_arg(args, "external", true);
                let t = query::call_tree(&self.store, sym, &filter, dir, depth, external)?;
                let text = render::tree(&t, plain);
                structured(&t, text)
            }
            "codegraph_outline" => {
                let file = req_str(args, "file")?;
                let sigs = bool_arg(args, "signatures", true);
                if util::is_remote_path(file) {
                    return Err(CallError::Tool(Error::Usage(format!(
                        "`{file}`: UNC and device paths are not allowed"
                    ))));
                }
                // Unindexed files are parsed on the fly only under a configured root.
                let (spec, cap) = match util::confine(file, &self.roots) {
                    Ok(p) => (
                        p.to_string_lossy().into_owned(),
                        Some(self.cfg.max_file_kb * 1024),
                    ),
                    Err(_) => (file.to_string(), None),
                };
                let o = query::outline(&self.store, &spec, &Filter::default(), cap)?;
                let text = render::outline(&o, sigs, plain);
                structured(&o, text)
            }
            "codegraph_impact" => {
                let target = req_str(args, "target")?;
                let filter = self.filter(args)?;
                let depth = uint_arg(args, "depth", 3, 8) as u32;
                let limit = uint_arg(args, "limit", 100, 1000) as usize;
                let i = query::impact(&self.store, target, &filter, depth, limit)?;
                let text = render::impact(&i, plain);
                structured(&i, text)
            }
            "codegraph_status" => {
                let filter = self.filter(args)?;
                let s = query::status(&self.store, &filter)?;
                let text = render::status(&s, plain);
                structured(&s, text)
            }
            other => Err(CallError::Rpc(
                INVALID_PARAMS,
                format!("unknown tool: {other}"),
            )),
        }
    }

    /// Handle one decoded message. Returns the response, or None for notifications.
    pub fn handle(&mut self, msg: &Value) -> Option<Value> {
        let Some(obj) = msg.as_object() else {
            return Some(rpc_error(
                &Value::Null,
                INVALID_REQUEST,
                "expected a JSON object",
            ));
        };
        let method = obj.get("method").and_then(Value::as_str);
        let Some(id) = obj.get("id") else {
            return None; // notification
        };
        let Some(method) = method else {
            if obj.contains_key("result") || obj.contains_key("error") {
                return None; // a response from the client
            }
            return Some(rpc_error(id, INVALID_REQUEST, "missing method"));
        };
        let params = obj.get("params").cloned().unwrap_or(Value::Null);
        let resp = match method {
            "initialize" => rpc_result(
                id,
                json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": {"tools": {"listChanged": false}},
                    "serverInfo": {"name": "codegraph", "title": "codegraph", "version": env!("CARGO_PKG_VERSION")},
                    "instructions": INSTRUCTIONS
                }),
            ),
            "ping" => rpc_result(id, json!({})),
            "tools/list" => rpc_result(id, json!({"tools": tool_list()})),
            "tools/call" => {
                let name = params
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let args = params
                    .get("arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                match self.call_tool(&name, &args) {
                    Ok(v) => rpc_result(id, v),
                    Err(CallError::Rpc(code, m)) => rpc_error(id, code, &m),
                    Err(CallError::Tool(e)) => rpc_result(
                        id,
                        json!({
                            "content": [{"type": "text", "text": format!("{}: {e}", e.code())}],
                            "isError": true
                        }),
                    ),
                }
            }
            m => rpc_error(id, METHOD_NOT_FOUND, &format!("method not found: {m}")),
        };
        Some(resp)
    }

    /// Serve until EOF on `reader`.
    pub fn serve(&mut self, reader: impl BufRead, mut writer: impl Write) -> io::Result<()> {
        eprintln!(
            "codegraph mcp: serving on stdio (protocol {PROTOCOL_VERSION}, db {})",
            self.store.path().display()
        );
        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let resp = match serde_json::from_str::<Value>(&line) {
                Ok(Value::Array(_)) => Some(rpc_error(
                    &Value::Null,
                    INVALID_REQUEST,
                    "batching is not supported",
                )),
                Ok(msg) => self.handle(&msg),
                Err(e) => Some(rpc_error(
                    &Value::Null,
                    PARSE_ERROR,
                    &format!("parse error: {e}"),
                )),
            };
            if let Some(r) = resp {
                writeln!(writer, "{r}")?;
                writer.flush()?;
            }
        }
        eprintln!("codegraph mcp: stdin closed, exiting");
        Ok(())
    }
}

fn rpc_error(id: &Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

#[allow(clippy::needless_pass_by_value)]
fn rpc_result(id: &Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server() -> (tempfile::TempDir, Server) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("g.db")).unwrap();
        let cfg = Config {
            refresh_budget_ms: 0,
            ..Config::default()
        };
        let cwd = dir.path().to_path_buf();
        (dir, Server::new(store, cfg, cwd))
    }

    #[test]
    fn initialize_and_list() {
        let (_d, mut s) = server();
        let r = s
            .handle(&json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":PROTOCOL_VERSION,"capabilities":{},"clientInfo":{"name":"t","version":"0"}}}))
            .unwrap();
        assert_eq!(r["result"]["protocolVersion"], PROTOCOL_VERSION);
        assert!(s
            .handle(&json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
            .is_none());
        let r = s
            .handle(&json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}))
            .unwrap();
        let names: Vec<&str> = r["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            [
                "codegraph_find",
                "codegraph_def",
                "codegraph_refs",
                "codegraph_callers",
                "codegraph_callees",
                "codegraph_outline",
                "codegraph_impact",
                "codegraph_status"
            ]
        );
        for t in r["result"]["tools"].as_array().unwrap() {
            assert_eq!(t["annotations"]["readOnlyHint"], true);
            assert_eq!(t["inputSchema"]["type"], "object");
        }
    }

    #[test]
    fn errors() {
        let (_d, mut s) = server();
        let r = s
            .handle(&json!({"jsonrpc":"2.0","id":3,"method":"nope"}))
            .unwrap();
        assert_eq!(r["error"]["code"], METHOD_NOT_FOUND);
        let r = s
            .handle(&json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"codegraph_def","arguments":{}}}))
            .unwrap();
        assert_eq!(r["error"]["code"], INVALID_PARAMS);
        let r = s
            .handle(&json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"codegraph_def","arguments":{"symbol":"nothing_here"}}}))
            .unwrap();
        assert_eq!(r["result"]["isError"], true);
        let r = s
            .handle(&json!({"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"codegraph_status","arguments":{}}}))
            .unwrap();
        assert_eq!(r["result"]["structuredContent"]["repos"], 0);
    }

    #[test]
    fn responses_are_capped() {
        // Huge structured data: replaced by a notice, text bounded.
        let big: Vec<String> = (0..2000).map(|_| "x".repeat(300)).collect();
        let text = big.join("\n");
        let r = structured(&big, text).ok().unwrap();
        assert_eq!(r["structuredContent"]["truncated"], true);
        assert!(r.to_string().len() <= MAX_RESPONSE_BYTES + 1024);
        // Normal data: kept; the text is a bounded summary; long lines clipped.
        let small = json!({"a": 1});
        let long_line = "y".repeat(5000);
        let r = structured(&small, format!("{long_line}\n{}", "z\n".repeat(20_000)))
            .ok()
            .unwrap();
        assert_eq!(r["structuredContent"]["a"], 1);
        let t = r["content"][0]["text"].as_str().unwrap();
        assert!(t.len() <= SUMMARY_TEXT_BYTES + 200, "{}", t.len());
        assert!(t.contains("…[+4600 chars]"));
        assert!(t.contains("full result in structuredContent"));
    }

    #[test]
    fn path_arguments_are_confined_and_repos_never_added() {
        let (dir, mut s) = server();
        // A git repo in the cwd that was never indexed must stay unindexed.
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        std::fs::write(dir.path().join("x.go"), "package x\nfunc X() {}\n").unwrap();
        s.cfg.refresh_budget_ms = 1000;
        let call = |s: &mut Server, id: i64, name: &str, args: Value| {
            s.handle(&json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":name,"arguments":args}}))
                .unwrap()
        };
        let r = call(&mut s, 1, "codegraph_find", json!({"query": "X"}));
        assert_eq!(r["result"]["structuredContent"]["hits"], json!([]), "{r}");
        assert!(
            s.store.repos().unwrap().is_empty(),
            "MCP must not add repos"
        );
        // UNC paths are refused before touching the filesystem.
        for p in [
            r"\\attacker\share\x.go",
            r"\\?\UNC\attacker\share\x.go",
            "//attacker/s/x.go",
        ] {
            let r = call(&mut s, 2, "codegraph_outline", json!({"file": p}));
            assert_eq!(r["result"]["isError"], true);
            let t = r["result"]["content"][0]["text"].as_str().unwrap();
            assert!(t.contains("UNC"), "{t}");
            let r = call(&mut s, 3, "codegraph_impact", json!({"target": p}));
            assert_eq!(r["result"]["isError"], true);
        }
        // A readable file outside the configured roots is not parsed on the fly.
        let r = call(
            &mut s,
            4,
            "codegraph_outline",
            json!({"file": dir.path().join("x.go").to_string_lossy()}),
        );
        assert_eq!(r["result"]["isError"], true, "{r}");
    }

    #[test]
    fn outline_on_the_fly_only_under_roots_and_within_cap() {
        let (dir, s) = server();
        let root = dir.path().join("root");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("ok.go"), "package x\nfunc Ok() {}\n").unwrap();
        std::fs::write(root.join("big.go"), "// x\n".repeat(400)).unwrap();
        std::fs::write(root.join("secrets.go"), "package x\n").unwrap();
        let cfg = Config {
            roots: vec![root.clone()],
            max_file_kb: 1,
            ..s.cfg.clone()
        };
        let mut s = Server::new(s.store, cfg, dir.path().to_path_buf());
        let mut out = |f: &str| {
            s.handle(&json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"codegraph_outline","arguments":{"file": root.join(f).to_string_lossy()}}}))
                .unwrap()
        };
        let r = out("ok.go");
        assert_eq!(r["result"]["isError"], false, "{r}");
        assert_eq!(r["result"]["structuredContent"]["symbols"][0]["name"], "Ok");
        assert_eq!(out("big.go")["result"]["isError"], true);
        assert_eq!(out("secrets.go")["result"]["isError"], true);
    }

    #[test]
    fn serve_loop_handles_garbage() {
        let (_d, mut s) = server();
        let input = b"not json\n\n{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n";
        let mut out = Vec::new();
        s.serve(&input[..], &mut out).unwrap();
        let lines: Vec<Value> = String::from_utf8(out)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["error"]["code"], PARSE_ERROR);
        assert_eq!(lines[1]["result"], json!({}));
    }
}

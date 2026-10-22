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

/// Floor for the write-lock wait during a refresh (see `Server::refresh`).
const MIN_BUSY_WAIT: Duration = Duration::from_millis(250);

/// Longest accepted JSON-RPC request line (bytes, excluding the newline). Longer
/// lines get a parse error and are skipped without being buffered.
pub const MAX_LINE_BYTES: usize = 4 * 1024 * 1024;

/// One line read by [`read_line_bounded`].
#[derive(Debug, PartialEq, Eq)]
enum Line {
    /// A complete line (newline stripped) is in the buffer.
    Ok,
    /// The line exceeded the cap; it was consumed through its newline (or EOF).
    TooLong,
    /// End of input with nothing read.
    Eof,
}

/// Read one `\n`-terminated line into `buf` (cleared first), holding at most `max`
/// bytes: an overlong line is drained to its newline without buffering it.
fn read_line_bounded(r: &mut impl BufRead, buf: &mut Vec<u8>, max: usize) -> io::Result<Line> {
    buf.clear();
    let mut too_long = false;
    let mut any = false;
    loop {
        let chunk = match r.fill_buf() {
            Ok(c) => c,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        if chunk.is_empty() {
            return Ok(match (any, too_long) {
                (false, _) => Line::Eof,
                (true, true) => Line::TooLong,
                (true, false) => Line::Ok,
            });
        }
        any = true;
        let (part, done) = match chunk.iter().position(|&b| b == b'\n') {
            Some(i) => (&chunk[..i], Some(i + 1)),
            None => (chunk, None),
        };
        if !too_long {
            if buf.len() + part.len() > max {
                too_long = true;
                buf.clear();
            } else {
                buf.extend_from_slice(part);
            }
        }
        let used = done.unwrap_or(chunk.len());
        r.consume(used);
        if done.is_some() {
            if buf.last() == Some(&b'\r') {
                buf.pop();
            }
            return Ok(if too_long { Line::TooLong } else { Line::Ok });
        }
    }
}

const INSTRUCTIONS: &str = "codegraph is a precomputed symbol + call graph (tree-sitter, heuristic resolution) of the user's repos. \
Prefer it over grep for structural questions: where is X defined (codegraph_def), who calls X (codegraph_callers), \
what does X call (codegraph_callees), what breaks if I change this file/function (codegraph_impact), \
what is in this file (codegraph_outline). Symbols accept a simple name, a qualified name (pkg.Type.Method, mod::Type::method), \
or file:line. Edges carry confidence: exact, likely (~), name_only (?) — verify name_only edges by reading the code. \
The repo you ask about is re-indexed automatically before each answer.";

/// Names of the tools in [`tool_list`], in listing order.
pub const TOOL_NAMES: [&str; 8] = [
    "codegraph_find",
    "codegraph_def",
    "codegraph_refs",
    "codegraph_callers",
    "codegraph_callees",
    "codegraph_outline",
    "codegraph_impact",
    "codegraph_status",
];

/// Tool definitions for `tools/list`.
pub fn tool_list() -> Value {
    let ro = json!({"readOnlyHint": true, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false});
    let repo = json!({"type": "string", "description": "Repo name (\"go/pulse\" or \"pulse\") or root path. Default: all."});
    let symbol = json!({"type": "string", "description": "Name, qualified name (Engine.Eval, store::Store::open) or file:line."});
    json!([
        {
            "name": "codegraph_find",
            "title": "Find symbols",
            "description": "Find definitions by name or fragment (exact > suffix > prefix > substring > doc text > fuzzy): kind, qualified name, file:line, signature.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "Name or fragment."},
                    "kind": {"type": "string", "description": "fn, method, class, struct, interface, trait, enum, type, types, const, module, macro."},
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
            "description": "Source of a definition (with its doc comment), line-numbered. Cheaper than reading the file.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "symbol": symbol,
                    "repo": repo,
                    "max_lines": {"type": "integer", "minimum": 1, "maximum": 1000, "default": 80, "description": "Lines per definition."},
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
            "description": "Every use of a symbol (calls, type refs, impls, inheritance) with file:line, enclosing function and source line.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "symbol": symbol,
                    "kind": {"type": "string", "enum": ["call", "ref", "impl", "inherit"], "description": "Only this kind."},
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
            "description": "Call tree up: who calls this, `depth` levels, with call sites. Includes calls through interfaces/traits it implements.",
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
            "description": "Call tree down: what this calls, `depth` levels; stdlib/dependency calls listed by name.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "symbol": symbol,
                    "depth": {"type": "integer", "minimum": 1, "maximum": 6, "default": 1},
                    "external": {"type": "boolean", "default": true, "description": "Include external calls."},
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
            "description": "Nested outline of a file (symbols, line ranges, signatures); use before reading a large file.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "file": {"type": "string", "description": "Path, or repo-relative suffix (internal/rules/engine.go)."},
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
            "description": "What breaks if X changes: symbols depending on a file or symbol, by distance, plus affected files.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "target": {"type": "string", "description": "File path or symbol."},
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
            "description": "Indexed repos: size, resolution rates, last indexed.",
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
    /// Keeps the last big repo's resolution state between auto-refreshes.
    resolve_cache: crate::resolve::ResolveCache,
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
            resolve_cache: crate::resolve::ResolveCache::new(),
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
        // One budget for the whole call, shared by every matched repo (a `repo`
        // filter can match many), not a fresh budget per repo.
        let deadline = Instant::now() + Duration::from_millis(self.cfg.refresh_budget_ms);
        for (root, name) in self.refresh_targets(args) {
            if self
                .last_refresh
                .get(&root)
                .is_some_and(|t| t.elapsed() < REFRESH_EVERY)
            {
                continue;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                eprintln!("codegraph mcp: refresh budget spent, skipping {name}");
                continue;
            }
            let opts = IndexOptions {
                full: false,
                budget: Some(left),
            };
            // The budget also bounds the wait for the write lock: while a CLI `index
            // --full` holds it, a call must not stall for the full default busy timeout.
            // Never below 250 ms, though: with a near-zero timeout the write commits and
            // the resolve right after it fails SQLITE_BUSY, leaving the repo unresolved.
            let wait = left.max(MIN_BUSY_WAIT);
            if let Err(e) = self.store.conn().busy_timeout(wait) {
                eprintln!("codegraph mcp: busy_timeout({wait:?}): {e}");
            }
            let result = index::refresh_repo_cached(
                &mut self.store,
                &self.cfg,
                &root,
                &name,
                &opts,
                &mut self.resolve_cache,
            );
            // Back to the default for everything outside a refresh (queries, the next
            // call's lock wait).
            if let Err(e) = self.store.conn().busy_timeout(Store::BUSY_TIMEOUT) {
                eprintln!("codegraph mcp: could not restore the busy timeout: {e}");
            }
            match result {
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
                Err(e) => {
                    eprintln!("codegraph mcp: refresh {name} failed: {e}");
                    // Busy or broken, do not retry (and stall) on every call for the
                    // next few seconds.
                    self.last_refresh.insert(root, Instant::now());
                }
            }
        }
    }

    fn call_tool(&mut self, name: &str, args: &Value) -> Result<Value, CallError> {
        // Reject unknown tools before the auto-refresh: a bad name must not cost an
        // index pass.
        if !TOOL_NAMES.contains(&name) {
            return Err(CallError::Rpc(
                INVALID_PARAMS,
                format!("unknown tool: {name}"),
            ));
        }
        if name != "codegraph_status" {
            self.refresh(args);
        }
        let plain = Style::plain();
        match name {
            "codegraph_find" => {
                let q = req_str(args, "query")?;
                if q.len() > query::MAX_QUERY_BYTES {
                    return Err(CallError::Rpc(
                        INVALID_PARAMS,
                        format!(
                            "`query` is {} bytes; at most {} are accepted",
                            q.len(),
                            query::MAX_QUERY_BYTES
                        ),
                    ));
                }
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
                // Unindexed files are parsed on the fly only under a configured root
                // (and, inside `outline`, only inside an indexed repo and when the
                // walker would include them).
                let (spec, cap) = match util::confine(file, &self.roots) {
                    Ok(p) => (p.to_string_lossy().into_owned(), Some(&self.cfg)),
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

    /// Serve until the client goes away: EOF on `reader`, or a closed pipe on either
    /// side (the client exited mid-session). Both are a normal shutdown and return
    /// `Ok`, logged as one line on stderr; any other I/O error is returned.
    pub fn serve(&mut self, reader: impl BufRead, writer: impl Write) -> io::Result<()> {
        eprintln!(
            "codegraph mcp: serving on stdio (protocol {PROTOCOL_VERSION}, db {})",
            util::slash(self.store.path())
        );
        match self.serve_loop(reader, writer) {
            Ok(()) => {
                eprintln!("codegraph mcp: stdin closed, exiting");
                Ok(())
            }
            Err(e) if is_closed_pipe(&e) => {
                eprintln!("codegraph mcp: client closed the pipe, exiting");
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    fn serve_loop(&mut self, mut reader: impl BufRead, mut writer: impl Write) -> io::Result<()> {
        let mut line = Vec::new();
        loop {
            match read_line_bounded(&mut reader, &mut line, MAX_LINE_BYTES)? {
                Line::Eof => break,
                Line::TooLong => {
                    let r = rpc_error(
                        &Value::Null,
                        PARSE_ERROR,
                        &format!("parse error: request line exceeds {MAX_LINE_BYTES} bytes"),
                    );
                    writeln!(writer, "{r}")?;
                    writer.flush()?;
                    continue;
                }
                Line::Ok => {}
            }
            if line.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            let resp = match serde_json::from_slice::<Value>(&line) {
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
        Ok(())
    }
}

/// The peer closed its end of a pipe: `EPIPE` / `ERROR_BROKEN_PIPE` / `ERROR_NO_DATA` on
/// write, a reset or aborted connection when stdio is a socket.
fn is_closed_pipe(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::BrokenPipe
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
    )
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
        // An over-long find query is invalid params, not a tool error.
        let long = "q".repeat(query::MAX_QUERY_BYTES + 1);
        let r = s
            .handle(&json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"codegraph_find","arguments":{"query": long}}}))
            .unwrap();
        assert_eq!(r["error"]["code"], INVALID_PARAMS, "{r}");
    }

    #[test]
    fn unknown_tool_is_rejected_before_refresh() {
        let (dir, mut s) = server();
        // A real, indexed repo in the cwd, so a refresh would have something to do.
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        std::fs::write(dir.path().join("x.go"), "package x\nfunc X() {}\n").unwrap();
        index::index_roots(
            &mut s.store,
            &s.cfg,
            &[dir.path().to_path_buf()],
            &IndexOptions::default(),
        )
        .unwrap();
        s.cfg.refresh_budget_ms = 1000;
        let bad = b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"codegraph_nope\",\"arguments\":{}}}\n";
        let good = b"{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"codegraph_find\",\"arguments\":{\"query\":\"X\"}}}\n";
        let mut out = Vec::new();
        // Serve the unknown tool on its own: it must not refresh anything.
        s.serve(&bad[..], &mut out).unwrap();
        let r: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(r["id"], 1);
        assert_eq!(r["error"]["code"], INVALID_PARAMS, "{r}");
        assert!(r["error"]["message"]
            .as_str()
            .unwrap()
            .contains("unknown tool: codegraph_nope"));
        assert!(
            s.last_refresh.is_empty(),
            "unknown tool must not trigger a refresh"
        );
        // A known tool does refresh (so the assertion above is meaningful).
        out.clear();
        s.serve(&good[..], &mut out).unwrap();
        let r: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(r["id"], 2);
        assert_eq!(
            r["result"]["structuredContent"]["hits"][0]["name"], "X",
            "{r}"
        );
        assert_eq!(s.last_refresh.len(), 1);
        // The validated names are exactly the listed tools.
        let listed: Vec<String> = tool_list()
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(listed, TOOL_NAMES);
    }

    /// While another connection holds the write lock (a CLI `index --full`), a tool call
    /// waits at most its refresh budget, not the connection's default busy timeout, and
    /// the failed refresh is not retried on every call.
    #[test]
    fn refresh_waits_for_the_lock_within_its_budget() {
        let (dir, mut s) = server();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        let x = dir.path().join("x.go");
        std::fs::write(&x, "package x\nfunc X() {}\n").unwrap();
        index::index_roots(
            &mut s.store,
            &s.cfg,
            &[dir.path().to_path_buf()],
            &IndexOptions::default(),
        )
        .unwrap();
        // A change that a refresh would have to write.
        std::fs::write(&x, "package x\nfunc X() {}\nfunc Y() {}\n").unwrap();
        s.cfg.refresh_budget_ms = 300;
        // Another connection holding the write lock.
        let mut other = Store::open(s.store.path()).unwrap();
        let lock = other
            .conn_mut()
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .unwrap();
        let t0 = Instant::now();
        let r = s
            .handle(&json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"codegraph_find","arguments":{"query":"X"}}}))
            .unwrap();
        let waited = t0.elapsed();
        assert_eq!(r["result"]["isError"], false, "{r}");
        assert!(waited < Duration::from_secs(3), "stalled {waited:?}");
        assert_eq!(
            s.last_refresh.len(),
            1,
            "a failed refresh is not retried at once"
        );
        // The answer came from the graph as it was.
        assert_eq!(
            r["result"]["structuredContent"]["hits"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        drop(lock);
        // The default timeout is back on the connection: a later refresh succeeds.
        s.last_refresh.clear();
        let r = s
            .handle(&json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"codegraph_find","arguments":{"query":"Y"}}}))
            .unwrap();
        assert_eq!(
            r["result"]["structuredContent"]["hits"][0]["name"], "Y",
            "{r}"
        );
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
    fn outline_on_the_fly_only_inside_indexed_repos_as_the_walker_sees_them() {
        let (dir, s) = server();
        let root = dir.path().to_path_buf();
        let repo = root.join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::write(repo.join("indexed.go"), "package x\nfunc Indexed() {}\n").unwrap();
        std::fs::write(repo.join(".gitignore"), "ignored.go\n").unwrap();
        let cfg = Config {
            roots: vec![root.clone()],
            max_file_kb: 1,
            ..s.cfg.clone()
        };
        let mut s = Server::new(s.store, cfg, root.clone());
        index::index_roots(
            &mut s.store,
            &s.cfg,
            std::slice::from_ref(&root),
            &IndexOptions::default(),
        )
        .unwrap();
        // Files that appeared after the index.
        let write = |rel: &str, text: &str| {
            let p = root.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        };
        write("repo/new.go", "package x\nfunc New() {}\n");
        // The receivers that used to panic the parser: outlined, never a crash (the
        // extraction is wrapped in catch_unwind like the indexer's).
        let hostile = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/hostile/a.go"),
        )
        .unwrap();
        write("repo/hostile.go", &hostile);
        write("repo/ignored.go", "package x\nfunc Ignored() {}\n");
        write("repo/.hidden/h.go", "package x\nfunc Hidden() {}\n");
        write("repo/node_modules/n.go", "package x\nfunc Skipped() {}\n");
        write("repo/big.go", &"// x\n".repeat(400));
        write("repo/secrets.go", "package x\n");
        write("loose/x.go", "package x\nfunc Loose() {}\n");
        let mut out = |f: &str| {
            s.handle(&json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"codegraph_outline","arguments":{"file": root.join(f).to_string_lossy()}}}))
                .unwrap()
        };
        // From the graph, and parsed on the fly: a plain new file in an indexed repo.
        let r = out("repo/indexed.go");
        assert_eq!(r["result"]["isError"], false, "{r}");
        assert_eq!(r["result"]["structuredContent"]["indexed"], true);
        let r = out("repo/new.go");
        assert_eq!(r["result"]["isError"], false, "{r}");
        assert_eq!(r["result"]["structuredContent"]["indexed"], false);
        assert_eq!(
            r["result"]["structuredContent"]["symbols"][0]["name"],
            "New"
        );
        let r = out("repo/hostile.go");
        assert_eq!(r["result"]["isError"], false, "{r}");
        let names: Vec<&str> = r["result"]["structuredContent"]["symbols"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["name"].as_str().unwrap())
            .collect();
        // Methods are shown as `<receiver>.<name>`; the receiver text is the hostile part.
        assert!(
            names.iter().any(|n| n.rsplit('.').next() == Some("F")),
            "{names:?}"
        );
        // Everything an index pass would not read is refused, under a root or not.
        for f in [
            "repo/ignored.go",
            "repo/.hidden/h.go",
            "repo/node_modules/n.go",
            "repo/big.go",
            "repo/secrets.go",
            "loose/x.go",
        ] {
            let r = out(f);
            assert_eq!(r["result"]["isError"], true, "{f}: {r}");
            let t = r["result"]["content"][0]["text"].as_str().unwrap();
            assert!(!t.contains("Ignored") && !t.contains("Hidden"), "{f}: {t}");
        }
    }

    #[test]
    fn bounded_line_reader() {
        let read_all = |input: &[u8], max: usize| {
            let mut r = io::BufReader::with_capacity(4, input);
            let mut buf = Vec::new();
            let mut out = Vec::new();
            loop {
                let l = read_line_bounded(&mut r, &mut buf, max).unwrap();
                if l == Line::Eof {
                    break;
                }
                out.push((l, String::from_utf8(buf.clone()).unwrap()));
            }
            out
        };
        assert_eq!(
            read_all(b"abc\r\n0123456789\nxy\nlast", 5),
            [
                (Line::Ok, "abc".to_string()),
                (Line::TooLong, String::new()),
                (Line::Ok, "xy".to_string()),
                (Line::Ok, "last".to_string()),
            ]
        );
        assert_eq!(
            read_all(b"12345\n123456", 5),
            [
                (Line::Ok, "12345".to_string()),
                (Line::TooLong, String::new())
            ]
        );
        assert_eq!(read_all(b"", 5), [] as [(Line, String); 0]);
    }

    #[test]
    fn serve_loop_caps_line_length() {
        let (_d, mut s) = server();
        // An overlong request (valid JSON, but huge) followed by a normal one.
        let mut input = format!(
            "{{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\",\"pad\":\"{}\"}}\n",
            "x".repeat(MAX_LINE_BYTES)
        )
        .into_bytes();
        input.extend_from_slice(b"{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"ping\"}\n");
        // Invalid UTF-8 is a parse error, not a fatal I/O error.
        input.extend_from_slice(b"\xff\xfe\n{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"ping\"}\n");
        let mut out = Vec::new();
        s.serve(&input[..], &mut out).unwrap();
        let lines: Vec<Value> = String::from_utf8(out)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 4, "{lines:?}");
        assert_eq!(lines[0]["error"]["code"], PARSE_ERROR);
        assert!(lines[0]["error"]["message"]
            .as_str()
            .unwrap()
            .contains("exceeds"));
        assert_eq!(lines[1]["id"], 2);
        assert_eq!(lines[1]["result"], json!({}));
        assert_eq!(lines[2]["error"]["code"], PARSE_ERROR);
        assert_eq!(lines[3]["id"], 3);
    }

    /// A writer whose every write fails with the given kind (a closed stdout pipe).
    struct FailWriter(io::ErrorKind);

    impl Write for FailWriter {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::new(self.0, "write failed"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// A reader whose every read fails with the given kind (a broken stdin pipe).
    struct FailReader(io::ErrorKind);

    impl io::Read for FailReader {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::new(self.0, "read failed"))
        }
    }

    const PING: &[u8] = b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n";

    #[test]
    fn closed_pipe_is_a_normal_shutdown() {
        let (_d, mut s) = server();
        // Client closed stdout: the response cannot be written.
        s.serve(PING, FailWriter(io::ErrorKind::BrokenPipe))
            .unwrap();
        s.serve(PING, FailWriter(io::ErrorKind::ConnectionReset))
            .unwrap();
        // Client's end of stdin broke mid-session.
        s.serve(
            io::BufReader::new(FailReader(io::ErrorKind::BrokenPipe)),
            Vec::new(),
        )
        .unwrap();
        // Plain EOF on stdin.
        s.serve(&b""[..], Vec::new()).unwrap();
    }

    #[test]
    fn real_io_errors_still_fail() {
        let (_d, mut s) = server();
        let e = s
            .serve(PING, FailWriter(io::ErrorKind::PermissionDenied))
            .unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::PermissionDenied);
        let e = s
            .serve(
                io::BufReader::new(FailReader(io::ErrorKind::Other)),
                Vec::new(),
            )
            .unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::Other);
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

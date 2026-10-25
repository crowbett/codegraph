//! The browser demo's logic (`crates/codegraph-web` wraps it for wasm32): a bundled
//! sample crate indexed into an in-memory graph by [`index::index_sources`], answering
//! the page's calls through [`query`], the same code the CLI and MCP server run.

use std::path::Path;

use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::index;
use crate::model::SymKind;
use crate::query::{self, Dir, Filter, OutlineNode};
use crate::store::Store;

/// Where the sample sits in the graph (it never touches the filesystem).
pub const ROOT: &str = "/sample";

/// The bundled sample: (repo-relative path, source).
pub const SAMPLE: [(&str, &str); 4] = [
    (
        "src/main.rs",
        include_str!("../crates/codegraph-web/sample/src/main.rs"),
    ),
    (
        "src/parse.rs",
        include_str!("../crates/codegraph-web/sample/src/parse.rs"),
    ),
    (
        "src/store.rs",
        include_str!("../crates/codegraph-web/sample/src/store.rs"),
    ),
    (
        "src/log.rs",
        include_str!("../crates/codegraph-web/sample/src/log.rs"),
    ),
];

/// The sample's graph.
#[derive(Debug)]
pub struct Demo {
    store: Store,
}

#[derive(Deserialize)]
struct CallsParams {
    /// `path:line` of the symbol (as listed by `sample`).
    symbol: String,
}

impl Demo {
    /// Index [`SAMPLE`] into a fresh in-memory graph.
    pub fn new() -> Result<Self> {
        let mut store = Store::open(Path::new(":memory:"))?;
        let files: Vec<(&str, &[u8])> = SAMPLE.iter().map(|(p, s)| (*p, s.as_bytes())).collect();
        index::index_sources(&mut store, "sample", Path::new(ROOT), &files)?;
        Ok(Self { store })
    }

    /// Answer one call: `sample` (the files, their functions and methods, and the
    /// resolver's edge counts), or `callers` / `callees` of `{"symbol": "path:line"}` one
    /// level deep, unresolved calls included. Errors come back as `{"error": ...}`.
    pub fn handle(&self, method: &str, params: &[u8]) -> Value {
        self.call(method, params)
            .unwrap_or_else(|e| json!({ "error": e.to_string() }))
    }

    fn call(&self, method: &str, params: &[u8]) -> Result<Value> {
        let dir = match method {
            "sample" => return self.sample(),
            "callers" => Dir::Callers,
            "callees" => Dir::Callees,
            _ => return Err(Error::Usage(format!("unknown method {method}"))),
        };
        let p: CallsParams =
            serde_json::from_slice(params).map_err(|e| Error::Usage(format!("params: {e}")))?;
        let tree = query::call_tree(&self.store, &p.symbol, &Filter::default(), dir, 1, true)?;
        serde_json::to_value(tree).map_err(|e| Error::Internal(e.to_string()))
    }

    fn sample(&self) -> Result<Value> {
        let mut files = Vec::new();
        for (path, text) in SAMPLE {
            let outline = query::outline(&self.store, path, &Filter::default(), None)?;
            let mut symbols = Vec::new();
            callables(&outline.symbols, path, None, &mut symbols);
            files.push(json!({ "path": path, "text": text, "symbols": symbols }));
        }
        let repo = self
            .store
            .repos()?
            .into_iter()
            .next()
            .ok_or_else(|| Error::Internal("sample not indexed".into()))?;
        let [symbols, edges, resolved, exact, likely, name_only] = repo.counts;
        Ok(json!({
            "root": ROOT,
            "files": files,
            "counts": {
                "symbols": symbols,
                "edges": edges,
                "resolved": resolved,
                "exact": exact,
                "likely": likely,
                "name_only": name_only,
            },
        }))
    }
}

/// Functions and methods of an outline, depth first, with the type or trait each sits in
/// and the spec that names it.
fn callables(nodes: &[OutlineNode], path: &str, owner: Option<&str>, out: &mut Vec<Value>) {
    for n in nodes {
        if SymKind::parse(&n.kind).is_some_and(SymKind::is_callable) {
            out.push(json!({
                "name": n.name,
                "owner": owner,
                "kind": n.kind,
                "line": n.line,
                "end_line": n.end_line,
                "spec": format!("{path}:{}", n.line),
            }));
        }
        callables(&n.children, path, Some(&n.name), out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::index::{index_roots, IndexOptions};
    use crate::query::{CallNode, CallTree};

    /// A call-tree level, independent of row ids and roots: (target, site, sites,
    /// confidence, as value, via).
    type Edge = (String, String, u32, Option<String>, bool, Option<String>);

    fn edges(t: &CallTree, root: &str) -> (String, Vec<Edge>) {
        assert_eq!(t.roots.len(), 1, "spec names one symbol");
        let r = &t.roots[0];
        let strip = |s: &str| {
            s.replace('\\', "/")
                .strip_prefix(&format!("{root}/"))
                .map_or_else(|| s.to_string(), str::to_string)
        };
        let target = |n: &CallNode| match (&n.sym, &n.external) {
            (Some(s), _) => s.qname.clone(),
            (None, Some(x)) => x.clone(),
            (None, None) => String::new(),
        };
        let qname = r.sym.as_ref().map(|s| s.qname.clone()).unwrap_or_default();
        let kids = r
            .children
            .iter()
            .map(|c| {
                (
                    target(c),
                    strip(c.site.as_deref().unwrap_or_default()),
                    c.sites,
                    c.confidence.clone(),
                    c.as_value,
                    c.via.clone(),
                )
            })
            .collect();
        (qname, kids)
    }

    fn specs(demo: &Demo) -> Vec<(String, String)> {
        let v = demo.handle("sample", b"{}");
        let mut out = Vec::new();
        for f in v["files"].as_array().unwrap() {
            for s in f["symbols"].as_array().unwrap() {
                out.push((
                    s["spec"].as_str().unwrap().to_string(),
                    s["name"].as_str().unwrap().to_string(),
                ));
            }
        }
        out
    }

    /// The in-memory path must answer exactly what `codegraph index` of the same files
    /// on disk answers, for every function and method in both directions.
    #[test]
    fn matches_a_disk_index_of_the_same_files() {
        let demo = Demo::new().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("sample");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        for (path, text) in SAMPLE {
            let p = root.join(path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        }
        let mut disk = Store::open(&tmp.path().join("graph.db")).unwrap();
        index_roots(
            &mut disk,
            &Config::default(),
            std::slice::from_ref(&root),
            &IndexOptions::default(),
        )
        .unwrap();
        let disk_root = disk.repos().unwrap()[0]
            .root
            .to_string_lossy()
            .replace('\\', "/");
        assert_eq!(
            disk.repos().unwrap()[0].counts,
            demo.store.repos().unwrap()[0].counts
        );
        let specs = specs(&demo);
        assert!(specs.len() >= 15, "{specs:?}");
        let (mut resolved, mut likely) = (0, 0);
        for (spec, name) in &specs {
            for dir in [Dir::Callers, Dir::Callees] {
                let f = Filter::default();
                let mem = query::call_tree(&demo.store, spec, &f, dir, 1, true).unwrap();
                let on_disk = query::call_tree(&disk, spec, &f, dir, 1, true).unwrap();
                let (q, mem) = edges(&mem, ROOT);
                assert!(
                    q.ends_with(name.rsplit("::").next().unwrap()),
                    "{spec}: {q}"
                );
                assert_eq!(
                    (q.clone(), mem.clone()),
                    edges(&on_disk, &disk_root),
                    "{spec} {dir:?}"
                );
                resolved += mem.iter().filter(|e| e.3.is_some()).count();
                likely += mem
                    .iter()
                    .filter(|e| e.3.as_deref() == Some("likely"))
                    .count();
            }
        }
        assert!(
            resolved > 20 && likely > 0,
            "{resolved} resolved, {likely} likely"
        );
    }

    /// Every call the page can make, answered natively. `crates/codegraph-web/check.mjs`
    /// asserts the wasm32 build answers the same; `UPDATE_ANSWERS=1` rewrites the file.
    #[test]
    fn answers_file_is_current() {
        let demo = Demo::new().unwrap();
        let sample = demo.handle("sample", b"{}");
        let mut calls = serde_json::Map::new();
        for (spec, _) in specs(&demo) {
            let p = json!({ "symbol": spec }).to_string();
            let both = json!({
                "callers": demo.handle("callers", p.as_bytes()),
                "callees": demo.handle("callees", p.as_bytes()),
            });
            calls.insert(spec, both);
        }
        let got = serde_json::to_string_pretty(&json!({
            "sample": sample,
            "calls": calls,
        }))
        .unwrap()
            + "\n";
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("crates/codegraph-web/sample/answers.json");
        if std::env::var_os("UPDATE_ANSWERS").is_some() {
            std::fs::write(&path, &got).unwrap();
        }
        let want = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            want.replace("\r\n", "\n") == got,
            "{} is stale: run UPDATE_ANSWERS=1 cargo test webdemo",
            path.display()
        );
    }

    #[test]
    fn answers_and_errors() {
        let demo = Demo::new().unwrap();
        let v = demo.handle("sample", b"{}");
        assert_eq!(v["files"].as_array().unwrap().len(), SAMPLE.len());
        assert!(v["counts"]["exact"].as_i64().unwrap() > 0);
        let v = demo.handle("callers", br#"{"symbol":"src/store.rs:32"}"#);
        let callers: Vec<&str> = v["roots"][0]["children"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["qname"].as_str().unwrap())
            .collect();
        assert_eq!(callers, ["store::Store::set", "store::Store::delete"]);
        assert!(demo.handle("nope", b"{}")["error"].is_string());
        assert!(demo.handle("callers", b"{")["error"].is_string());
        assert!(demo.handle("callees", br#"{"symbol":"src/none.rs:1"}"#)["error"].is_string());
    }

    #[test]
    fn indexing_a_root_twice_is_refused() {
        let mut demo = Demo::new().unwrap();
        let before = demo.store.repos().unwrap()[0].counts;
        let files: Vec<(&str, &[u8])> = SAMPLE.iter().map(|(p, s)| (*p, s.as_bytes())).collect();
        let err = index::index_sources(&mut demo.store, "sample", Path::new(ROOT), &files);
        assert!(matches!(err, Err(Error::Usage(_))), "{err:?}");
        assert_eq!(demo.store.repos().unwrap()[0].counts, before);
    }
}

//! Graph queries on a small known graph: callers/callees depth + cycles, path,
//! impact, hotspots, outline, dot, find.
#![allow(clippy::unwrap_used, missing_docs)]

mod common;

use codegraph::query::{self, CallNode, Dir, Filter, GraphFormat};
use common::index_fixture;

fn names(nodes: &[CallNode]) -> Vec<String> {
    nodes
        .iter()
        .map(|n| {
            let mut s = n
                .sym
                .as_ref()
                .map_or_else(|| "?".into(), |s| s.name.clone());
            if n.cycle {
                s.push_str("[cycle]");
            }
            if n.seen {
                s.push_str("[seen]");
            }
            s
        })
        .collect()
}

#[test]
fn callers_depth_and_cycles() {
    let ix = index_fixture("graph");
    let f = Filter::default();
    let t = query::call_tree(&ix.store, "c", &f, Dir::Callers, 1, true).unwrap();
    assert_eq!(names(&t.roots[0].children), ["a", "b"]);
    assert!(t.roots[0].children.iter().all(|c| c.children.is_empty()));

    let t = query::call_tree(&ix.store, "c", &f, Dir::Callers, 3, true).unwrap();
    let root = &t.roots[0];
    assert_eq!(names(&root.children[0].children), ["main"]);
    // b <- a (already expanded above) and b <- d <- c (cycle back to the root).
    assert_eq!(names(&root.children[1].children), ["a[seen]", "d"]);
    assert_eq!(names(&root.children[1].children[1].children), ["c[cycle]"]);
}

#[test]
fn callees_depth() {
    let ix = index_fixture("graph");
    let f = Filter::default();
    let t = query::call_tree(&ix.store, "a", &f, Dir::Callees, 1, true).unwrap();
    assert_eq!(names(&t.roots[0].children), ["b", "c"]);
    let t = query::call_tree(&ix.store, "b", &f, Dir::Callees, 4, true).unwrap();
    // b -> c -> d -> b (cycle)
    let c = &t.roots[0].children[0];
    let d = &c.children[0];
    assert_eq!(names(&d.children), ["b[cycle]"]);
}

#[test]
fn shortest_path() {
    let ix = index_fixture("graph");
    let f = Filter::default();
    let p = query::path(&ix.store, "main", "d", &f, 10).unwrap();
    let hops: Vec<&str> = p.hops.iter().map(|h| h.sym.name.as_str()).collect();
    assert_eq!(hops, ["main", "a", "c", "d"]);
    assert_eq!(p.length, 3);
    assert!(p.hops[0]
        .calls_at
        .as_deref()
        .unwrap()
        .ends_with("main.go:3"));
    let err = query::path(&ix.store, "d", "main", &f, 10).unwrap_err();
    assert_eq!(err.code(), "not_found");
}

#[test]
fn impact_by_depth() {
    let ix = index_fixture("graph");
    let f = Filter::default();
    let i = query::impact(&ix.store, "d", &f, 3, 100).unwrap();
    let got: Vec<(u32, &str)> = i
        .dependents
        .iter()
        .map(|d| (d.depth, d.sym.name.as_str()))
        .collect();
    assert_eq!(got, [(1, "c"), (2, "a"), (2, "b"), (3, "main")]);
    let shallow = query::impact(&ix.store, "d", &f, 1, 100).unwrap();
    assert_eq!(shallow.total, 1);
    // A file target seeds every symbol in it.
    let file = common::fixture("graph").join("main.go");
    let whole = query::impact(&ix.store, &file.to_string_lossy(), &f, 2, 100).unwrap();
    assert_eq!(whole.seeds, 6);
}

#[test]
fn hotspots_outline_dot_find() {
    let ix = index_fixture("graph");
    let f = Filter::default();
    let h = query::hotspots(&ix.store, &f, 5).unwrap();
    let top: Vec<(&str, i64)> = h
        .most_referenced
        .iter()
        .take(2)
        .map(|r| (r.sym.name.as_str(), r.count))
        .collect();
    assert_eq!(top, [("b", 2), ("c", 2)]);
    let orphans: Vec<&str> = h.orphans.iter().map(|r| r.sym.name.as_str()).collect();
    assert_eq!(orphans, ["orphan"]);

    let o = query::outline(&ix.store, "main.go", &f, None).unwrap();
    let names: Vec<&str> = o.symbols.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["main", "a", "b", "c", "d", "orphan"]);
    assert_eq!((o.symbols[1].line, o.symbols[1].end_line), (5, 8));

    let dot = query::graph(&ix.store, "c", &f, 1, GraphFormat::Dot).unwrap();
    assert!(dot.starts_with("digraph codegraph {"));
    assert_eq!(dot.matches(" -> ").count(), 3, "{dot}");
    let mm = query::graph(&ix.store, "c", &f, 1, GraphFormat::Mermaid).unwrap();
    assert!(mm.starts_with("flowchart LR") && mm.contains("-->"));

    let hits = query::find(&ix.store, "orphn", &f, 5).unwrap();
    assert_eq!(hits[0].sym.name, "orphan");
    assert_eq!(hits[0].how, "fuzzy");
    let hits = query::find(&ix.store, "main.d", &f, 5).unwrap();
    assert_eq!(hits[0].sym.qname, "main.d");
    // Doc text is searchable.
    let hits = query::find(&ix.store, "closes the cycle", &f, 5).unwrap();
    assert_eq!(hits[0].sym.name, "d");
}

#[test]
fn file_line_targets() {
    let ix = index_fixture("graph");
    let f = Filter::default();
    let t = query::resolve_target(&ix.store, "main.go:6", &f).unwrap();
    assert_eq!(t[0].name, "a");
    let d = query::def(&ix.store, "d", &f, 50, 1).unwrap();
    assert_eq!(d[0].source_line, 14, "doc comment is included");
    assert!(d[0].source.contains("func d() { b() }"));
}

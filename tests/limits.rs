//! SEC-1: hostile inputs (thousand-call method chains, deep nesting) must extract in
//! bounded time, and ordinary files must not be affected by the limits.
#![allow(clippy::unwrap_used, missing_docs)]

use std::time::{Duration, Instant};

use codegraph::extract::extract;
use codegraph::lang::Lang;

/// Generous for debug builds on a loaded CI runner. Before SEC-1 a 16k-link chain took
/// 11-73 s in release and a 64k-link one never finished.
const BOUND: Duration = Duration::from_secs(30);

const LANGS: [Lang; 5] = [
    Lang::Go,
    Lang::Rust,
    Lang::TypeScript,
    Lang::JavaScript,
    Lang::Python,
];

fn chain(links: usize) -> String {
    let mut s = String::from("x");
    for _ in 0..links {
        s.push_str(".b()");
    }
    s
}

fn chained(lang: Lang, links: usize) -> String {
    let c = chain(links);
    match lang {
        Lang::Go => format!("package p\n\nfunc f(x T) {{\n\t{c}\n}}\n"),
        Lang::Rust => format!("fn f(x: T) {{\n    {c};\n    assert!({c});\n}}\n"),
        Lang::TypeScript | Lang::JavaScript => format!("function f(x) {{\n  {c};\n}}\n"),
        Lang::Python => format!("def f(x):\n    {c}\n"),
        _ => unreachable!(),
    }
}

fn timed(lang: Lang, src: &str) -> codegraph::extract::FileFacts {
    let t = Instant::now();
    let facts = extract(lang, src.as_bytes()).unwrap();
    let took = t.elapsed();
    assert!(took < BOUND, "{lang:?}: {took:?}");
    facts
}

#[test]
/// Under the depth cap (two tree levels per link) but past the match limit.
fn long_chains_are_bounded_and_still_extracted() {
    for lang in LANGS {
        let facts = timed(lang, &chained(lang, 1500));
        assert!(facts.limited, "{lang:?}: expected the limits to kick in");
        // Calls are still extracted. Past the match limit tree-sitter drops the oldest
        // in-progress matches, which can include the enclosing function's definition.
        assert!(facts.refs.iter().any(|r| r.name == "b"), "{lang:?}");
    }
}

#[test]
fn over_deep_trees_are_cancelled_not_hung() {
    // Past a depth of 65 535 tree-sitter's query cursor stops making progress: the query
    // is cancelled at its time budget and whatever it matched so far is kept.
    for lang in LANGS {
        let facts = timed(lang, &chained(lang, 40_000));
        assert!(facts.limited, "{lang:?}");
        assert!(facts.lines > 0, "{lang:?}");
    }
}

#[test]
fn ordinary_chains_are_not_limited() {
    for lang in LANGS {
        let facts = timed(lang, &chained(lang, 20));
        assert!(!facts.limited, "{lang:?}");
        let calls = facts.refs.iter().filter(|r| r.name == "b").count();
        let want = if lang == Lang::Rust { 40 } else { 20 };
        assert_eq!(calls, want, "{lang:?}");
    }
    let facts = extract(Lang::Rust, chained(Lang::Rust, 20).as_bytes()).unwrap();
    assert!(facts.refs.iter().any(|r| r.name == "b" && r.hint.is_some()));
}

#[test]
fn nested_generic_receivers_are_cheap() {
    // A Go receiver's type parameters are its type's own arguments; the nested `A`s of a
    // hostile `*A[A[A[..]]]` are types, and looking at them must not recurse or rescan.
    let depth = 10_000;
    let recv = format!("{}int{}", "A[".repeat(depth), "]".repeat(depth));
    let src = format!("package p\n\nfunc (s *{recv}) M(v T) T {{\n\treturn v\n}}\n");
    let facts = timed(Lang::Go, &src);
    assert!(facts.symbols.iter().any(|s| s.name == "M"));
}

/// Go receivers with `]` before `[` (`func (s /*]*/ T /*[*/) F()`, `func (s ]T[) F()`)
/// used to slice out of bounds in the receiver type-parameter scan and panic the parser
/// thread: with `panic = "abort"` that killed `codegraph index` and the MCP server on
/// every refresh; in debug builds the file silently vanished from the graph.
#[test]
fn broken_go_receivers_do_not_panic() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/hostile");
    for f in ["a.go", "b.go"] {
        let src = std::fs::read(dir.join(f)).unwrap();
        let facts = extract(Lang::Go, &src).unwrap();
        assert!(facts.symbols.iter().any(|s| s.name == "F"), "{f}");
    }
    // Through the indexer too: both files land in the graph.
    let db = tempfile::tempdir().unwrap();
    let mut store = codegraph::store::Store::open(&db.path().join("g.db")).unwrap();
    let cfg = codegraph::config::Config {
        roots: Vec::new(),
        ..codegraph::config::Config::default()
    };
    let st = codegraph::index::index_roots(
        &mut store,
        &cfg,
        &[dir],
        &codegraph::index::IndexOptions::default(),
    )
    .unwrap();
    assert_eq!((st.files, st.parsed), (2, 2), "{st:?}");
}

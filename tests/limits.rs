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

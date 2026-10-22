//! Human-readable rendering. The same (uncolored) text is the MCP `content` summary,
//! so it is kept compact: one line per item, repo-relative paths under a repo header,
//! `file:line` everywhere.

use std::fmt::Write as _;

use crate::config::ConfigView;
use crate::extract::clip;
use crate::index::IndexStats;
use crate::output::{code, Cell, Style, Table};
use crate::query::{
    CallNode, CallPath, CallTree, Def, Hit, Hotspots, Impact, Outline, OutlineNode, Ranked,
    References, Status, Sym,
};
use crate::watch::WatchEvent;

/// Replace C0/C1 control characters (ESC, CR, BEL, U+0080..U+009F, ...) and bidi
/// embedding/override/isolate controls (U+202A..U+202E, U+2066..U+2069, "Trojan
/// Source") with U+FFFD so repo-controlled text (source snippets, file and repo
/// names, paths) cannot drive a terminal or visually reorder output. Tabs are kept.
pub fn clean(s: &str) -> String {
    s.chars()
        .map(|c| {
            if (c.is_control() && c != '\t')
                || matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
            {
                '\u{FFFD}'
            } else {
                c
            }
        })
        .collect()
}

fn short_kind(k: &str) -> &str {
    match k {
        "function" => "fn",
        "method" => "method",
        "interface" => "iface",
        other => other,
    }
}

fn kind_color(k: &str) -> &'static str {
    match k {
        "function" | "method" | "macro" => code::GREEN,
        "class" | "struct" | "interface" | "trait" | "enum" | "type" => code::CYAN,
        "module" => code::MAGENTA,
        _ => code::YELLOW,
    }
}

fn conf_mark(c: Option<&str>) -> &'static str {
    match c {
        Some("likely") => " ~",
        Some("name_only") => " ?",
        _ => "",
    }
}

/// Strip `root/` from an absolute `root/path:line` string.
/// Strip `root/` from an absolute `root/path:line` string (sanitized for display).
fn rel_to(root: &str, abs: &str) -> String {
    let r = root.trim_end_matches('/');
    clean(match abs.strip_prefix(r) {
        Some(rest) if rest.starts_with('/') => &rest[1..],
        _ => abs,
    })
}

fn repo_header(s: Style, repo: &str, root: &str) -> String {
    format!(
        "{} {}\n",
        s.bold(&format!("# {}", clean(repo))),
        s.dim(&clean(root))
    )
}

/// `find` results.
pub fn find(hits: &[Hit], s: Style) -> String {
    let mut out = String::new();
    let mut cur_repo = "";
    let mut table = Table::new(&[]);
    for h in hits {
        if h.sym.repo != cur_repo {
            out.push_str(&table.render(s, 2));
            table = Table::new(&[]);
            out.push_str(&repo_header(s, &h.sym.repo, &h.sym.root));
            cur_repo = &h.sym.repo;
        }
        table.row(vec![
            Cell::styled(short_kind(&h.sym.kind), kind_color(&h.sym.kind)),
            Cell::styled(
                if matches!(h.how, "exact" | "qualified" | "prefix") {
                    clean(&h.sym.qname)
                } else {
                    format!("{} ({})", clean(&h.sym.qname), h.how)
                },
                code::BOLD,
            ),
            Cell::plain(clean(&h.sym.rel_loc())),
            Cell::styled(clean(&clip(&h.sym.signature, 90)), code::DIM),
        ]);
    }
    out.push_str(&table.render(s, 2));
    out
}

/// `def` results with numbered source.
pub fn defs(defs: &[Def], s: Style) -> String {
    let mut out = String::new();
    for (i, d) in defs.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let _ = writeln!(
            out,
            "{} {} {}",
            s.bold(&clean(&d.sym.qname)),
            s.paint(kind_color(&d.sym.kind), &d.sym.kind),
            s.dim(&format!(
                "{}:{}-{}",
                clean(&d.sym.file),
                d.sym.line,
                d.sym.end_line
            ))
        );
        if let Some(note) = &d.note {
            let _ = writeln!(out, "{}", s.yellow(&format!("  ({note})")));
        }
        let width = (d.source_line as usize + d.source.lines().count())
            .to_string()
            .len();
        for (n, line) in d.source.lines().enumerate() {
            let ln = d.source_line as usize + n;
            let _ = writeln!(out, "{} {}", s.dim(&format!("{ln:>width$}|")), clean(line));
        }
        if d.truncated_lines > 0 {
            let _ = writeln!(
                out,
                "{}",
                s.dim(&format!(
                    "… {} more lines (raise max_lines)",
                    d.truncated_lines
                ))
            );
        }
    }
    out
}

/// `refs` results grouped by file.
pub fn refs(r: &References, s: Style) -> String {
    let mut out = String::new();
    let names: Vec<String> = r
        .targets
        .iter()
        .map(|t| format!("{} ({})", clean(&t.qname), clean(&t.rel_loc())))
        .collect();
    let _ = writeln!(
        out,
        "{} reference(s) to {}{}",
        r.total,
        names.join(", "),
        if r.refs.len() < r.total {
            format!(" — showing {}", r.refs.len())
        } else {
            String::new()
        }
    );
    let mut cur_file = String::new();
    let mut cur_root = String::new();
    for x in &r.refs {
        if x.root != cur_root {
            cur_root.clone_from(&x.root);
            out.push_str(&repo_header(s, &x.repo, &x.root));
            cur_file.clear();
        }
        if x.rel != cur_file {
            cur_file.clone_from(&x.rel);
            let _ = writeln!(out, "  {}", s.cyan(&clean(&x.rel)));
        }
        let from = x
            .from
            .as_deref()
            .map_or(String::new(), |f| format!(" in {}", clean(f)));
        let text = match &x.note {
            Some(n) => s.yellow(&format!("({n})")),
            None => clean(&x.text),
        };
        let _ = writeln!(
            out,
            "    {:>5} {}{}{}  {}",
            format!(":{}", x.line),
            x.kind,
            conf_mark(Some(&x.confidence)),
            s.dim(&from),
            text
        );
    }
    out
}

fn node_label(n: &CallNode, callers: bool, root: &str, s: Style) -> String {
    let mut line = match (&n.sym, &n.external) {
        (Some(sym), _) => {
            let loc = if callers {
                n.site
                    .as_deref()
                    .map_or_else(|| sym.rel_loc(), |x| rel_to(root, x))
            } else {
                rel_to(root, &sym.loc())
            };
            format!("{}  {}", s.bold(&clean(&sym.qname)), s.dim(&clean(&loc)))
        }
        (None, Some(x)) => {
            let at = n
                .site
                .as_deref()
                .map_or(String::new(), |x| format!("  {}", rel_to(root, x)));
            format!("{}{}", s.yellow(&clean(x)), s.dim(&at))
        }
        (None, None) => "?".into(),
    };
    line.push_str(conf_mark(n.confidence.as_deref()));
    if n.sites > 1 {
        let _ = write!(line, " {}", s.dim(&format!("×{}", n.sites)));
    }
    if let Some(v) = &n.via {
        line.push_str(&format!(" {}", s.dim(&format!("[via {}]", clean(v)))));
    }
    if n.as_value {
        line.push_str(&format!(" {}", s.dim("[as value]")));
    }
    if n.cycle {
        line.push_str(&format!(" {}", s.red("[cycle]")));
    }
    if n.seen {
        line.push_str(&format!(" {}", s.dim("[seen above]")));
    }
    line
}

fn tree_rec(n: &CallNode, prefix: &str, callers: bool, root: &str, s: Style, out: &mut String) {
    let count = n.children.len() + usize::from(n.more > 0);
    for (i, c) in n.children.iter().enumerate() {
        let last = i + 1 == count;
        let _ = writeln!(
            out,
            "{prefix}{}{}",
            if last { "└─ " } else { "├─ " },
            node_label(c, callers, root, s)
        );
        let next = format!("{prefix}{}", if last { "   " } else { "│  " });
        tree_rec(c, &next, callers, root, s, out);
    }
    if n.more > 0 {
        let _ = writeln!(out, "{prefix}└─ {}", s.dim(&format!("… {} more", n.more)));
    }
}

/// `callers` / `callees` tree.
pub fn tree(t: &CallTree, s: Style) -> String {
    let callers = t.direction == "callers";
    let mut out = String::new();
    for r in &t.roots {
        let Some(sym) = &r.sym else { continue };
        let _ = writeln!(
            out,
            "{} {}  {}",
            s.bold(&clean(&sym.qname)),
            s.dim(&format!("({})", sym.kind)),
            s.dim(&clean(&sym.loc()))
        );
        if r.children.is_empty() && r.more == 0 {
            let _ = writeln!(out, "   {}", s.dim(&format!("(no {} found)", t.direction)));
        }
        tree_rec(r, "", callers, &sym.root, s, &mut out);
    }
    let mut foot = format!(
        "{} node(s), depth {}; paths relative to each root's repo; ~ likely, ? name-only",
        t.nodes, t.depth
    );
    if t.truncated {
        foot.push_str("; truncated");
    }
    let _ = writeln!(out, "{}", s.dim(&foot));
    out
}

fn outline_rec(nodes: &[OutlineNode], depth: usize, sigs: bool, s: Style, out: &mut String) {
    for n in nodes {
        let lines = format!("{}-{}", n.line, n.end_line);
        let indent = "  ".repeat(depth);
        let sig = if sigs && !n.signature.is_empty() && n.signature != n.name {
            format!("  {}", s.dim(&clean(&clip(&n.signature, 100))))
        } else {
            String::new()
        };
        let _ = writeln!(
            out,
            "{:>9}  {indent}{} {}{sig}",
            lines,
            s.paint(kind_color(&n.kind), short_kind(&n.kind)),
            s.bold(&clean(&n.name))
        );
        outline_rec(&n.children, depth + 1, sigs, s, out);
    }
}

/// `outline` of a file.
pub fn outline(o: &Outline, sigs: bool, s: Style) -> String {
    let mut out = format!(
        "{} {}\n",
        s.bold(&clean(&o.file)),
        s.dim(&format!(
            "({}, {} lines{})",
            o.lang,
            o.lines,
            if o.indexed { "" } else { ", not indexed" }
        ))
    );
    if o.symbols.is_empty() {
        out.push_str("  (no symbols)\n");
    }
    outline_rec(&o.symbols, 0, sigs, s, &mut out);
    out
}

/// `path` result.
pub fn path(p: &CallPath, s: Style) -> String {
    let mut out = format!("call path, {} hop(s):\n", p.length);
    for (i, h) in p.hops.iter().enumerate() {
        let at = h.calls_at.as_deref().map_or(String::new(), |x| {
            format!("  calls next at {}", rel_to(&h.sym.root, x))
        });
        let _ = writeln!(
            out,
            "{:>3}. {}  {}{}",
            i + 1,
            s.bold(&clean(&h.sym.qname)),
            s.dim(&clean(&h.sym.rel_loc())),
            s.dim(&at)
        );
    }
    out
}

/// `impact` result.
pub fn impact(i: &Impact, s: Style) -> String {
    let mut out = format!(
        "impact of {}: {} dependent symbol(s) in {} file(s) within depth {} ({} seed symbol(s)){}\n",
        s.bold(&clean(&i.target)),
        i.total,
        i.files.len(),
        i.depth,
        i.seeds,
        if i.truncated { "; list truncated" } else { "" }
    );
    if !i.importers.is_empty() {
        let _ = writeln!(out, "imported by {} file(s):", i.importers.len());
        for f in i.importers.iter().take(50) {
            let _ = writeln!(out, "  {}", clean(f));
        }
    }
    let mut cur = 0;
    for d in &i.dependents {
        if d.depth != cur {
            cur = d.depth;
            let _ = writeln!(
                out,
                "{}",
                s.bold(&format!(
                    "depth {cur}{}",
                    if cur == 1 { " (direct)" } else { "" }
                ))
            );
        }
        let _ = writeln!(
            out,
            "  {} {}  {}  {}",
            s.paint(kind_color(&d.sym.kind), short_kind(&d.sym.kind)),
            s.bold(&clean(&d.sym.qname)),
            s.dim(&rel_to(&d.sym.root, &d.site)),
            s.dim(&format!("via {}", clean(&d.via)))
        );
    }
    if !i.files.is_empty() {
        let _ = writeln!(out, "{}", s.bold("files"));
        for f in i.files.iter().take(40) {
            let _ = writeln!(
                out,
                "  {:>4}  d{}  {}",
                f.count,
                f.min_depth,
                clean(&f.file)
            );
        }
    }
    out
}

fn ranked_table(title: &str, rows: &[Ranked], metric: &str, s: Style) -> String {
    let mut out = format!("{}\n", s.bold(title));
    if rows.is_empty() {
        out.push_str("  (none)\n");
        return out;
    }
    let mut t = Table::new(&[]);
    for r in rows {
        let extra = r.files.map_or(String::new(), |f| format!(" in {f} files"));
        t.row(vec![
            Cell::plain(format!("{} {metric}{extra}", r.count)),
            Cell::styled(short_kind(&r.sym.kind), kind_color(&r.sym.kind)),
            Cell::styled(clean(&r.sym.qname), code::BOLD),
            Cell::styled(
                format!("{}  {}", clean(&r.sym.repo), clean(&r.sym.rel_loc())),
                code::DIM,
            ),
        ]);
    }
    out.push_str(&t.render(s, 2));
    out
}

/// `hotspots` result.
pub fn hotspots(h: &Hotspots, s: Style) -> String {
    let mut out = ranked_table("most referenced", &h.most_referenced, "refs", s);
    out.push_str(&ranked_table("largest fan-out", &h.fan_out, "callees", s));
    out.push_str(&ranked_table(
        &format!(
            "orphans: unreferenced non-exported functions ({} total; candidates only — callbacks, reflection and entry points are invisible to the graph)",
            h.orphan_total
        ),
        &h.orphans,
        "lines",
        s,
    ));
    out
}

fn human_count(n: i64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

fn pct(a: i64, b: i64) -> String {
    if b == 0 {
        "-".into()
    } else {
        format!("{:.0}%", a as f64 * 100.0 / b as f64)
    }
}

/// `status` result.
pub fn status(st: &Status, s: Style) -> String {
    let mut out = String::new();
    let conf = |k: &str| st.by_confidence.get(k).copied().unwrap_or(0);
    let _ = writeln!(
        out,
        "{} repos · {} files · {} symbols · {} edges ({} resolved: {} exact, {} likely, {} name-only)",
        st.repos,
        human_count(st.files),
        human_count(st.symbols),
        human_count(st.edges),
        pct(st.resolved, st.edges),
        human_count(conf("exact")),
        human_count(conf("likely")),
        human_count(conf("name_only")),
    );
    let langs: Vec<String> = st
        .languages
        .iter()
        .map(|(l, n)| format!("{l} {n}"))
        .collect();
    let _ = writeln!(out, "languages: {}", langs.join(", "));
    let _ = writeln!(
        out,
        "db: {} ({:.1} MB)",
        clean(&st.db),
        st.db_bytes as f64 / 1_048_576.0
    );
    if !st.repo_list.is_empty() {
        let mut t = Table::new(&["REPO", "FILES", "SYMBOLS", "EDGES", "RESOLVED", "INDEXED"]);
        for r in &st.repo_list {
            t.row(vec![
                Cell::styled(clean(&r.name), code::BOLD),
                Cell::plain(human_count(r.files)),
                Cell::plain(human_count(r.symbols)),
                Cell::plain(human_count(r.edges)),
                Cell::plain(pct(r.resolved, r.edges)),
                Cell::styled(r.indexed_at.clone(), code::DIM),
            ]);
        }
        out.push_str(&t.render(s, 0));
    }
    out
}

/// `index` result.
pub fn index(st: &IndexStats, verbose: bool, s: Style) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "indexed {} repos · {} files ({} parsed, {} unchanged, {} removed{}) · {} symbols · {} edges ({} resolved) · {:.2}s",
        st.repos,
        human_count(st.files as i64),
        human_count(st.parsed as i64),
        human_count(st.unchanged as i64),
        st.removed,
        if st.skipped_large > 0 {
            format!(", {} too large", st.skipped_large)
        } else {
            String::new()
        },
        human_count(st.symbols),
        human_count(st.edges),
        pct(st.resolved_edges, st.edges),
        st.elapsed_ms as f64 / 1000.0
    );
    if st.pending > 0 {
        let _ = writeln!(
            out,
            "{}",
            s.yellow(&format!("{} file(s) pending (time budget hit)", st.pending))
        );
    }
    if st.failed > 0 {
        let _ = writeln!(
            out,
            "{}",
            s.yellow(&format!(
                "{} file(s) could not be read (locked or permission denied); kept as last indexed",
                st.failed
            ))
        );
    }
    if st.parse_errors > 0 {
        let _ = writeln!(
            out,
            "{}",
            s.dim(&format!(
                "{} file(s) had syntax errors (indexed best-effort)",
                st.parse_errors
            ))
        );
    }
    if verbose {
        let _ = writeln!(
            out,
            "  phases: parse {} ms, write {} ms, resolve {} ms",
            st.parse_ms, st.write_ms, st.resolve_ms
        );
        for r in &st.changed_repos {
            let res = r.resolve.map_or(String::new(), |x| {
                format!(
                    "  edges {} resolved {} (exact {}, likely {}, name-only {})",
                    x.edges, x.resolved, x.exact, x.likely, x.name_only
                )
            });
            let _ = writeln!(
                out,
                "  {}  {} files, {} parsed, {} removed{}",
                s.bold(&clean(&r.name)),
                r.files,
                r.parsed,
                r.removed,
                s.dim(&res)
            );
        }
    }
    out
}

/// `config show`.
pub fn config(v: &ConfigView) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "path              {}{}",
        clean(&v.path),
        if v.exists {
            ""
        } else {
            " (not found; defaults)"
        }
    );
    let _ = writeln!(out, "db                {}", clean(&v.db));
    let _ = writeln!(out, "roots             {}", clean(&v.roots.join(", ")));
    let _ = writeln!(out, "skip_dirs         {}", clean(&v.skip_dirs.join(", ")));
    let _ = writeln!(out, "max_file_kb       {}", v.max_file_kb);
    let langs: Vec<&str> = v.languages.iter().map(|l| l.as_str()).collect();
    let _ = writeln!(out, "languages         {}", langs.join(", "));
    let _ = writeln!(out, "color             {:?}", v.color);
    let _ = writeln!(out, "refresh_budget_ms {}", v.refresh_budget_ms);
    let _ = writeln!(out, "threads           {}", v.threads);
    out
}

/// One-line symbol label (`qname  file:line`).
pub fn sym_line(sym: &Sym) -> String {
    format!("{}  {}", clean(&sym.qname), clean(&sym.loc()))
}

fn clock() -> String {
    let t = crate::util::format_unix(crate::util::now_unix());
    t.get(11..19).unwrap_or(&t).to_string()
}

fn listed(files: &[String], changed: usize) -> String {
    let mut s: Vec<&str> = files.iter().take(3).map(String::as_str).collect();
    let more = changed.saturating_sub(s.len());
    let extra = format!("+{more}");
    if more > 0 {
        s.push(&extra);
    }
    s.join(", ")
}

/// One `watch` event as a human line. `None` for events not worth a line (a re-index
/// that changed nothing, unless `verbose`).
pub fn watch_event(e: &WatchEvent, verbose: bool, s: Style) -> Option<String> {
    let line = match e {
        WatchEvent::Ready {
            roots,
            repos,
            initial,
        } => {
            let mut out = String::new();
            if let Some(st) = initial {
                out.push_str(&index(st, false, s));
            }
            let _ = write!(
                out,
                "{} watching {} ({} repos) — Ctrl-C to stop",
                s.dim(&clock()),
                roots.join(", "),
                repos
            );
            out
        }
        WatchEvent::Reindex {
            repo,
            files,
            changed,
            ms,
            stats,
            ..
        } => {
            if stats.parsed == 0 && stats.removed == 0 && !verbose {
                return None;
            }
            format!(
                "{} {} {}: {} parsed, {} removed · {} symbols · {} ms ({})",
                s.dim(&clock()),
                s.green("reindex"),
                s.bold(repo),
                stats.parsed,
                stats.removed,
                human_count(stats.symbols),
                ms,
                clean(&listed(files, *changed))
            )
        }
        WatchEvent::Rescan {
            root,
            reason,
            ms,
            stats,
        } => format!(
            "{} {} {root}: {} parsed, {} removed · {} repos · {} ms ({})",
            s.dim(&clock()),
            s.cyan("rescan"),
            stats.parsed,
            stats.removed,
            stats.repos,
            ms,
            clean(reason)
        ),
        WatchEvent::Error {
            repo,
            code,
            message,
        } => format!(
            "{} {} {}{code}: {}",
            s.dim(&clock()),
            s.red("error"),
            repo.as_deref()
                .map(|r| format!("{r}: "))
                .unwrap_or_default(),
            clean(message)
        ),
    };
    Some(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_chars_are_neutralized() {
        let evil = "let x = 1; \u{1b}[2J\u{1b}]0;pwned\u{7}\r\u{9b}31m\tok";
        let c = clean(evil);
        assert!(!c.chars().any(|ch| ch.is_control() && ch != '\t'), "{c:?}");
        assert!(c.contains("\tok") && c.starts_with("let x = 1; "));
        let d = crate::query::Def {
            sym: crate::query::test_sym(),
            source_line: 1,
            source: evil.to_string(),
            truncated_lines: 0,
            note: None,
        };
        let out = defs(&[d], Style::plain());
        assert!(!out.contains('\u{1b}') && !out.contains('\u{9b}') && !out.contains('\r'));
    }

    #[test]
    fn bidi_controls_are_neutralized() {
        for ch in [
            '\u{202A}', '\u{202B}', '\u{202C}', '\u{202D}', '\u{202E}', '\u{2066}', '\u{2067}',
            '\u{2068}', '\u{2069}',
        ] {
            let c = clean(&format!("a{ch}b"));
            assert_eq!(c, "a\u{FFFD}b", "{ch:?}");
        }
        // Neighbours of the ranges and ordinary non-ASCII text are untouched.
        assert_eq!(
            clean("\u{2029}\u{202F}\u{2065}\u{206A}é名"),
            "\u{2029}\u{202F}\u{2065}\u{206A}é名"
        );
    }

    fn assert_clean(out: &str) {
        for bad in ['\u{1b}', '\u{9b}', '\u{7}', '\r', '\u{202E}', '\u{2066}'] {
            assert!(!out.contains(bad), "{bad:?} leaked into {out:?}");
        }
    }

    #[test]
    fn paths_and_names_are_sanitized() {
        let evil = "a\u{1b}]0;pwned\u{7}\u{202E}b\r\u{9b}2J\u{2066}";
        let mut sym = crate::query::test_sym();
        sym.qname = format!("m.{evil}");
        sym.repo = format!("repo{evil}");
        sym.root = format!("C:/{evil}");
        sym.rel = format!("{evil}.go");
        sym.file = format!("{}/{}", sym.root, sym.rel);
        let s = Style::plain();

        let hit = Hit {
            sym: sym.clone(),
            how: "exact",
        };
        assert_clean(&find(&[hit], s));
        let d = Def {
            sym: sym.clone(),
            source_line: 1,
            source: String::new(),
            truncated_lines: 0,
            note: Some(crate::query::STALE_NOTE.into()),
        };
        let out = defs(&[d], s);
        assert_clean(&out);
        assert!(out.contains("re-index"), "{out}");
        let r = References {
            targets: vec![sym.clone()],
            refs: vec![crate::query::RefSite {
                file: sym.file.clone(),
                line: 1,
                kind: "call".into(),
                confidence: "exact".into(),
                from: Some(evil.into()),
                text: String::new(),
                note: Some(crate::query::STALE_NOTE.into()),
                rel: sym.rel.clone(),
                root: sym.root.clone(),
                repo: evil.into(),
            }],
            total: 1,
        };
        let out = refs(&r, s);
        assert_clean(&out);
        assert!(out.contains("re-index"), "{out}");
        assert_clean(&sym_line(&sym));
        assert_clean(&rel_to("C:/x", &format!("C:/x/{evil}")));
        assert_clean(&repo_header(s, evil, evil));
    }

    #[test]
    fn helpers() {
        assert_eq!(rel_to("C:/a/b", "C:/a/b/x/y.go:3"), "x/y.go:3");
        assert_eq!(rel_to("C:/a/b", "C:/z/y.go:3"), "C:/z/y.go:3");
        assert_eq!(human_count(1_234_567), "1,234,567");
        assert_eq!(pct(1, 4), "25%");
        assert_eq!(pct(1, 0), "-");
        assert_eq!(conf_mark(Some("likely")), " ~");
    }
}

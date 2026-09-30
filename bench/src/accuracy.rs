//! Accuracy benchmark: codegraph's graph vs SCIP ground truth, site by site.
//!
//! Ground truth (per repo, from the SCIP index built by `prepare`):
//! - **definitions**: non-local SCIP definitions of functions/methods (`name().`) and
//!   types (`Name#`) in files codegraph indexes;
//! - **reference sites**: SCIP reference occurrences that point at one of those in-repo
//!   definitions; a site is a **call site** when its target is callable and the source
//!   text after it is a call (`(`, `::<..>(`, `<..>(`, `[..](`);
//! - **implementations**: SCIP `is_implementation` relationships between two in-repo types.
//!
//! Scoring:
//! - recall: a GT site is a hit when codegraph has an edge at the same `file:line:col`
//!   with the same name, resolved to the symbol whose span holds the SCIP definition;
//!   `dispatch` hits resolve to an implementation of (or the interface for) the GT target.
//! - precision: every *resolved* codegraph edge whose site SCIP knows is judged; it is
//!   false when SCIP says the site points somewhere else (another symbol, a local, or
//!   something outside the repo). Dispatch-related targets count as correct.
//! - calibration: precision per confidence tier (`exact`, `likely`, `name_only`).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::corpus::{self, Dirs, Repo};
use crate::scipdata::{class_of, is_local, Class, Occ, Scip};

/// Recall counters for one kind of GT site.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Recall {
    pub gt: u64,
    pub hit: u64,
    pub dispatch: u64,
    pub wrong: u64,
    pub unresolved: u64,
    pub no_edge: u64,
    pub no_def: u64,
}

impl Recall {
    fn add(&mut self, o: &Recall) {
        self.gt += o.gt;
        self.hit += o.hit;
        self.dispatch += o.dispatch;
        self.wrong += o.wrong;
        self.unresolved += o.unresolved;
        self.no_edge += o.no_edge;
        self.no_def += o.no_def;
    }
    fn rate(&self) -> f64 {
        ratio(self.hit + self.dispatch, self.gt)
    }
}

/// Precision counters for resolved codegraph edges.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Precision {
    pub resolved: u64,
    pub judged: u64,
    pub correct: u64,
    pub dispatch: u64,
    pub false_edges: u64,
}

impl Precision {
    fn add(&mut self, o: &Precision) {
        self.resolved += o.resolved;
        self.judged += o.judged;
        self.correct += o.correct;
        self.dispatch += o.dispatch;
        self.false_edges += o.false_edges;
    }
    fn rate(&self) -> f64 {
        ratio(self.correct + self.dispatch, self.judged)
    }
    fn false_rate(&self) -> f64 {
        ratio(self.false_edges, self.judged)
    }
}

/// Everything measured for one repo (or summed over a language).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Counts {
    pub files: u64,
    pub defs_gt: u64,
    pub defs_found: u64,
    pub calls: Recall,
    pub refs: Recall,
    pub call_edges: Precision,
    pub all_edges: Precision,
    pub conf: BTreeMap<String, Precision>,
    pub impl_gt: u64,
    pub impl_hit: u64,
    pub impl_edges: Precision,
    pub index_ms: u64,
}

impl Counts {
    fn add(&mut self, o: &Counts) {
        self.files += o.files;
        self.defs_gt += o.defs_gt;
        self.defs_found += o.defs_found;
        self.calls.add(&o.calls);
        self.refs.add(&o.refs);
        self.call_edges.add(&o.call_edges);
        self.all_edges.add(&o.all_edges);
        for (k, v) in &o.conf {
            self.conf.entry(k.clone()).or_default().add(v);
        }
        self.impl_gt += o.impl_gt;
        self.impl_hit += o.impl_hit;
        self.impl_edges.add(&o.impl_edges);
        self.index_ms += o.index_ms;
    }
}

/// The JSON results document.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Results {
    pub codegraph: String,
    pub repos: BTreeMap<String, RepoResult>,
    pub langs: BTreeMap<String, Counts>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RepoResult {
    pub lang: String,
    pub rev: String,
    pub counts: Counts,
}

fn ratio(a: u64, b: u64) -> f64 {
    if b == 0 {
        f64::NAN
    } else {
        a as f64 / b as f64
    }
}

// ---- codegraph side ----------------------------------------------------------

struct CgSym {
    file: i64,
    name: String,
    kind: String,
    start: u32,
    end: u32,
}

struct CgEdge {
    file: i64,
    name: String,
    kind: String,
    line: u32,
    col: u32,
    to: Option<i64>,
    conf: Option<String>,
    from: Option<i64>,
}

struct Cg {
    /// file id -> (path, lang)
    files: HashMap<i64, (String, String)>,
    by_path: HashMap<String, i64>,
    syms: HashMap<i64, CgSym>,
    /// (file, name) -> symbol ids
    sym_by_file_name: HashMap<(i64, String), Vec<i64>>,
    edges: Vec<CgEdge>,
    /// (file, line) -> edge indexes
    edge_at: HashMap<(i64, u32), Vec<usize>>,
}

fn load_cg(db: &Path) -> Result<Cg> {
    let conn = rusqlite::Connection::open(db)?;
    let mut files = HashMap::new();
    let mut by_path = HashMap::new();
    {
        let mut st = conn.prepare("SELECT id, path, lang FROM files")?;
        let rows = st.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        for row in rows {
            let (id, path, lang) = row?;
            by_path.insert(path.clone(), id);
            files.insert(id, (path, lang));
        }
    }
    let mut syms = HashMap::new();
    let mut sym_by_file_name: HashMap<(i64, String), Vec<i64>> = HashMap::new();
    {
        let mut st =
            conn.prepare("SELECT id, file_id, name, kind, start_line, end_line FROM symbols")?;
        let rows = st.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, u32>(4)?,
                r.get::<_, u32>(5)?,
            ))
        })?;
        for row in rows {
            let (id, file, name, kind, start, end) = row?;
            sym_by_file_name
                .entry((file, name.clone()))
                .or_default()
                .push(id);
            syms.insert(
                id,
                CgSym {
                    file,
                    name,
                    kind,
                    start,
                    end,
                },
            );
        }
    }
    let mut edges = Vec::new();
    {
        let mut st = conn.prepare(
            "SELECT file_id, name, kind, line, col, to_symbol, confidence, from_symbol
             FROM edges WHERE kind != 'import'",
        )?;
        let rows = st.query_map([], |r| {
            Ok(CgEdge {
                file: r.get(0)?,
                name: r.get(1)?,
                kind: r.get(2)?,
                line: r.get(3)?,
                col: r.get(4)?,
                to: r.get(5)?,
                conf: r.get(6)?,
                from: r.get(7)?,
            })
        })?;
        for row in rows {
            edges.push(row?);
        }
    }
    let mut edge_at: HashMap<(i64, u32), Vec<usize>> = HashMap::new();
    for (i, e) in edges.iter().enumerate() {
        edge_at.entry((e.file, e.line)).or_default().push(i);
    }
    Ok(Cg {
        files,
        by_path,
        syms,
        sym_by_file_name,
        edges,
        edge_at,
    })
}

fn lang_family(lang: &str) -> Option<&'static str> {
    match lang {
        "go" => Some("go"),
        "rust" => Some("rust"),
        "typescript" | "tsx" | "javascript" => Some("ts"),
        "python" => Some("python"),
        _ => None,
    }
}

/// A TS/JS shorthand property (`{ make_, extend_ }`, one per line or inline) resolved to the
/// same-file definition it names. `{ make_ }` is `{ make_: make_ }`, but scip-typescript
/// records only the contextual type's member there (`Annotation.make_`) when the object has
/// one, never the binding the value comes from; both are valid readings of the site.
fn shorthand_local(cg: &Cg, fid: i64, line: &str, o: &Occ, to: i64, name: &str) -> bool {
    if cg.files.get(&fid).and_then(|(_, l)| lang_family(l)) != Some("ts") || is_local(&o.symbol) {
        return false;
    }
    let Some((bs, be)) = byte_range(line, o.start, o.end) else {
        return false;
    };
    let (before, after) = (line[..bs].trim_end(), line[be..].trim_start());
    let open = before.is_empty() || before.ends_with('{') || before.ends_with(',');
    let close = after.is_empty() || after.starts_with(',') || after.starts_with('}');
    open && close
        && cg
            .syms
            .get(&to)
            .is_some_and(|s| s.file == fid && s.name == name)
}

// ---- source text -------------------------------------------------------------

struct Sources {
    root: PathBuf,
    cache: HashMap<String, Vec<String>>,
}

impl Sources {
    fn line(&mut self, path: &str, line0: u32) -> Option<&str> {
        if !self.cache.contains_key(path) {
            let text = std::fs::read(self.root.join(path))
                .map(|b| String::from_utf8_lossy(&b).into_owned())
                .unwrap_or_default();
            let lines = text
                .split('\n')
                .map(|l| l.strip_suffix('\r').unwrap_or(l).to_string())
                .collect();
            self.cache.insert(path.to_string(), lines);
        }
        self.cache
            .get(path)
            .and_then(|v| v.get(line0 as usize))
            .map(String::as_str)
    }
}

/// Byte range of an occurrence on its line. SCIP indexers disagree on the column unit
/// (UTF-8 bytes vs UTF-16 units); on non-ASCII lines try both and keep the one that
/// lands on an identifier.
fn byte_range(line: &str, start: u32, end: u32) -> Option<(usize, usize)> {
    if end == u32::MAX || end < start {
        return None;
    }
    let (s, e) = (start as usize, end as usize);
    if line.is_ascii() {
        return (e <= line.len()).then_some((s, e));
    }
    let as_bytes = (e <= line.len() && line.is_char_boundary(s) && line.is_char_boundary(e))
        .then_some((s, e))
        .filter(|&(s, e)| is_ident(&line[s..e]));
    if as_bytes.is_some() {
        return as_bytes;
    }
    let u16_to_byte = |u: usize| -> Option<usize> {
        let mut units = 0;
        for (b, ch) in line.char_indices() {
            if units == u {
                return Some(b);
            }
            units += ch.len_utf16();
        }
        (units == u).then_some(line.len())
    };
    let (bs, be) = (u16_to_byte(s)?, u16_to_byte(e)?);
    Some((bs, be))
}

fn is_ident(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '$')
}

fn occ_text<'a>(line: &'a str, o: &Occ) -> Option<&'a str> {
    let (s, e) = byte_range(line, o.start, o.end)?;
    let t = line.get(s..e)?;
    // rust-analyzer ranges for raw identifiers include `r#`.
    let t = t.strip_prefix("r#").unwrap_or(t);
    is_ident(t).then_some(t)
}

/// Whether the text after an identifier makes it a call.
fn is_call(rest: &str) -> bool {
    let r = rest.trim_start();
    if r.starts_with('(') || r.starts_with("?.(") {
        return true;
    }
    let r = r.strip_prefix("::").unwrap_or(r);
    let Some(open) = r.chars().next() else {
        return false;
    };
    let close = match open {
        '<' => '>',
        '[' => ']',
        _ => return false,
    };
    let mut depth = 0i32;
    for (i, c) in r.char_indices() {
        if c == open {
            depth += 1;
        } else if c == close {
            depth -= 1;
            if depth == 0 {
                return r[i + 1..].trim_start().starts_with('(');
            }
        } else if matches!(c, ';' | '{' | '}' | '=') && open == '<' {
            return false;
        }
    }
    false
}

// ---- per repo ----------------------------------------------------------------

struct Report {
    misses: Vec<String>,
    /// Missed non-call references (types, values, callables used as values).
    ref_misses: Vec<String>,
    falses: Vec<String>,
    defs_missing: Vec<String>,
}

fn index_repo(dirs: &Dirs, r: &Repo) -> Result<(PathBuf, u64)> {
    let db_dir = dirs.cache.join("cgdb");
    std::fs::create_dir_all(&db_dir)?;
    let db = db_dir.join(format!("{}.db", r.name));
    for ext in ["", "-wal", "-shm"] {
        let p = PathBuf::from(format!("{}{ext}", db.display()));
        if p.exists() {
            std::fs::remove_file(&p).with_context(|| format!("remove {}", p.display()))?;
        }
    }
    let root = dirs.corpus(&r.name);
    let cfg = codegraph::config::Config {
        roots: vec![root.clone()],
        ..codegraph::config::Config::default()
    };
    let mut store = codegraph::store::Store::open(&db)?;
    let t = Instant::now();
    codegraph::index::index_roots(
        &mut store,
        &cfg,
        &[root],
        &codegraph::index::IndexOptions {
            full: true,
            budget: None,
        },
    )?;
    let ms = t.elapsed().as_millis() as u64;
    drop(store);
    Ok((db, ms))
}

fn score_repo(dirs: &Dirs, r: &Repo, report: bool) -> Result<(BTreeMap<String, Counts>, Report)> {
    let (db, index_ms) = index_repo(dirs, r)?;
    let cg = load_cg(&db)?;
    let scip = Scip::load(&dirs.scip(&r.name))?;
    let mut src = Sources {
        root: dirs.corpus(&r.name),
        cache: HashMap::new(),
    };
    // Separate cache for the platform-twin check (used while `src` lines are borrowed).
    let mut twin_src = Sources {
        root: dirs.corpus(&r.name),
        cache: HashMap::new(),
    };
    let mut by_lang: BTreeMap<String, Counts> = BTreeMap::new();
    let mut rep = Report {
        misses: Vec::new(),
        ref_misses: Vec::new(),
        falses: Vec::new(),
        defs_missing: Vec::new(),
    };
    let fam_of =
        |fid: i64| -> Option<&'static str> { cg.files.get(&fid).and_then(|(_, l)| lang_family(l)) };

    // SCIP definition -> codegraph symbol (same file, same name, span holds the line).
    let mut def_text: HashMap<&str, String> = HashMap::new();
    let mut def_to_cg: HashMap<&str, i64> = HashMap::new();
    for (sym, d) in &scip.defs {
        let Some(&fid) = cg.by_path.get(&d.path) else {
            continue;
        };
        let Some(line) = src.line(&d.path, d.line) else {
            continue;
        };
        let occ = Occ {
            line: d.line,
            start: d.start,
            end: scip
                .on_line(&d.path, d.line)
                .iter()
                .find(|o| o.is_def && o.symbol == *sym)
                .map_or(u32::MAX, |o| o.end),
            symbol: sym.clone(),
            is_def: true,
        };
        let Some(name) = occ_text(line, &occ) else {
            continue;
        };
        let name = name.to_string();
        let l1 = d.line + 1;
        let best = cg
            .sym_by_file_name
            .get(&(fid, name.clone()))
            .into_iter()
            .flatten()
            .filter(|&&id| {
                let s = &cg.syms[&id];
                s.start <= l1 && l1 <= s.end
            })
            .min_by_key(|&&id| cg.syms[&id].end - cg.syms[&id].start);
        if let Some(&id) = best {
            def_to_cg.insert(sym.as_str(), id);
        }
        def_text.insert(sym.as_str(), name);
    }
    let mut cg_to_scip: HashMap<i64, Vec<&str>> = HashMap::new();
    for (&s, &id) in &def_to_cg {
        cg_to_scip.entry(id).or_default().push(s);
    }
    let is_callable_cg = |id: i64| matches!(cg.syms[&id].kind.as_str(), "function" | "method");
    let target_class = |sym: &str| -> Class {
        match class_of(sym) {
            Class::Other => match def_to_cg.get(sym) {
                Some(&id) if is_callable_cg(id) => Class::Callable,
                _ => Class::Other,
            },
            c => c,
        }
    };

    // Definitions coverage.
    for (sym, d) in &scip.defs {
        let Some(&fid) = cg.by_path.get(&d.path) else {
            continue;
        };
        let Some(fam) = fam_of(fid) else { continue };
        if class_of(sym) == Class::Other || !def_text.contains_key(sym.as_str()) {
            continue;
        }
        let c = by_lang.entry(fam.to_string()).or_default();
        c.defs_gt += 1;
        if def_to_cg.contains_key(sym.as_str()) {
            c.defs_found += 1;
        } else if report {
            rep.defs_missing.push(format!(
                "{}:{} {}  {}",
                d.path,
                d.line + 1,
                def_text[sym.as_str()],
                sym
            ));
        }
    }

    let related = |gt_sym: &str, cg_id: i64| -> bool {
        cg_to_scip
            .get(&cg_id)
            .is_some_and(|v| v.iter().any(|s| scip.dispatch_related(gt_sym, s)))
    };
    let describe = |id: Option<i64>| -> String {
        match id {
            None => "-".into(),
            Some(id) => {
                let s = &cg.syms[&id];
                format!("{}:{} {}", cg.files[&s.file].0, s.start, s.name)
            }
        }
    };

    // GT reference sites -> recall.
    let paths: Vec<String> = scip.occs.keys().cloned().collect();
    for path in &paths {
        let Some(&fid) = cg.by_path.get(path) else {
            continue;
        };
        let Some(fam) = fam_of(fid) else { continue };
        let occs = scip.occs[path].clone();
        for o in occs.iter().filter(|o| !o.is_def) {
            let Some(d) = scip.defs.get(&o.symbol) else {
                continue;
            };
            if !cg.by_path.contains_key(&d.path) {
                continue;
            }
            let class = target_class(&o.symbol);
            if class == Class::Other {
                continue;
            }
            let Some(line) = src.line(path, o.line) else {
                continue;
            };
            let Some(name) = occ_text(line, o) else {
                continue;
            };
            let name = name.to_string();
            let Some((_, be)) = byte_range(line, o.start, o.end) else {
                continue;
            };
            // codegraph has a definition of this name starting on this line: an
            // object-literal method (`run() {}`) that SCIP records as a reference to the
            // interface member it implements, not a use site.
            let cg_def_here = cg
                .sym_by_file_name
                .get(&(fid, name.clone()))
                .is_some_and(|v| v.iter().any(|id| cg.syms[id].start == o.line + 1));
            if cg_def_here {
                continue;
            }
            let call = class == Class::Callable && is_call(&line[be..]);
            let (bs, _) = byte_range(line, o.start, o.end).unwrap_or((0, 0));
            let edge = find_edge(&cg, fid, o.line + 1, bs as u32 + 1, &name);
            let expect = def_to_cg.get(o.symbol.as_str()).copied();
            // Other symbols SCIP records on this exact range: a TS shorthand property
            // (`{ out, err }`) is both the local binding and the contextual type's member.
            // One edge can name either.
            let alt_hit = |t: i64| {
                scip.on_line(path, o.line).iter().any(|x| {
                    !x.is_def
                        && x.start == o.start
                        && x.end == o.end
                        && x.symbol != o.symbol
                        && correct_target(&cg, &scip, &def_text, &x.symbol, t)
                })
            };
            let outcome = match (expect, edge) {
                (None, _) => Outcome::NoDef,
                (_, None) => Outcome::NoEdge,
                (Some(_), Some(e)) => match cg.edges[e].to {
                    None => Outcome::Unresolved,
                    Some(t) if Some(t) == expect => Outcome::Hit,
                    Some(t) if correct_target(&cg, &scip, &def_text, &o.symbol, t) => Outcome::Hit,
                    Some(t)
                        if platform_twin(&cg, &scip, &def_text, &mut twin_src, &o.symbol, t) =>
                    {
                        Outcome::Hit
                    }
                    Some(t) if alt_hit(t) => Outcome::Hit,
                    Some(t) if shorthand_local(&cg, fid, line, o, t, &name) => Outcome::Hit,
                    Some(t) if related(&o.symbol, t) => Outcome::Dispatch,
                    Some(_) => Outcome::Wrong,
                },
            };
            let c = by_lang.entry(fam.to_string()).or_default();
            let mut buckets = vec![&mut c.refs];
            if call {
                buckets.push(&mut c.calls);
            }
            for b in buckets {
                b.gt += 1;
                match outcome {
                    Outcome::Hit => b.hit += 1,
                    Outcome::Dispatch => b.dispatch += 1,
                    Outcome::Wrong => b.wrong += 1,
                    Outcome::Unresolved => b.unresolved += 1,
                    Outcome::NoEdge => b.no_edge += 1,
                    Outcome::NoDef => b.no_def += 1,
                }
            }
            if report && !matches!(outcome, Outcome::Hit | Outcome::Dispatch) {
                let got = edge.map(|e| {
                    let ce = &cg.edges[e];
                    format!("{} {}", describe(ce.to), ce.conf.as_deref().unwrap_or(""))
                });
                let (out, tag) = if call {
                    (&mut rep.misses, String::new())
                } else {
                    (&mut rep.ref_misses, format!("{class:?}\t"))
                };
                out.push(format!(
                    "{tag}{:?}\t{}:{}\t{}\twant {}:{}\tgot {}\t| {}",
                    outcome,
                    path,
                    o.line + 1,
                    name,
                    d.path,
                    d.line + 1,
                    got.unwrap_or_else(|| "no edge".into()),
                    line.trim()
                ));
            }
        }
    }

    // Resolved codegraph edges -> precision + calibration.
    for e in &cg.edges {
        let Some(to) = e.to else { continue };
        if !matches!(e.kind.as_str(), "call" | "ref" | "value") {
            continue;
        }
        let Some(fam) = fam_of(e.file) else { continue };
        let path = cg.files[&e.file].0.clone();
        let c = by_lang.entry(fam.to_string()).or_default();
        let is_call_edge = e.kind == "call";
        c.all_edges.resolved += 1;
        if is_call_edge {
            c.call_edges.resolved += 1;
        }
        let Some(line) = src.line(&path, e.line - 1) else {
            continue;
        };
        let line = line.to_string();
        let occs = scip.on_line(&path, e.line - 1);
        let hit = occs
            .iter()
            .filter(|o| occ_text(&line, o) == Some(e.name.as_str()))
            .min_by_key(|o| {
                let bs = byte_range(&line, o.start, o.end).map_or(0, |r| r.0 as i64);
                (bs - (i64::from(e.col) - 1)).abs()
            });
        let Some(o) = hit else { continue };
        if o.is_def {
            continue;
        }
        // A local with no definition occurrence is SCIP's placeholder for a name it could
        // not resolve: an import of a workspace package whose `types` point at an unbuilt
        // dist/, or a member of a module it did not type-check (mobx's test files are
        // outside its tsconfig, so `mobx.autorun` became `local N`). Real locals always
        // have a definition. No ground truth for the site: leave it unjudged.
        if is_local(&o.symbol) && !scip.defs.contains_key(&o.symbol) {
            continue;
        }
        // Every symbol SCIP records on this exact range is a valid reading of the site
        // (see `alt_hit` above).
        let alt_ok = occs.iter().any(|x| {
            !x.is_def
                && x.start == o.start
                && x.end == o.end
                && x.symbol != o.symbol
                && correct_target(&cg, &scip, &def_text, &x.symbol, to)
        });
        let verdict = if alt_ok || shorthand_local(&cg, e.file, &line, o, to, &e.name) {
            Verdict::Correct
        } else if !scip.defs.contains_key(&o.symbol) {
            Verdict::False
        } else if correct_target(&cg, &scip, &def_text, &o.symbol, to)
            || platform_twin(&cg, &scip, &def_text, &mut twin_src, &o.symbol, to)
        {
            Verdict::Correct
        } else if related(&o.symbol, to) {
            Verdict::Dispatch
        } else {
            Verdict::False
        };
        let conf = e.conf.clone().unwrap_or_default();
        let mut buckets = vec![&mut c.all_edges];
        if is_call_edge {
            buckets.push(&mut c.call_edges);
        }
        let conf_bucket = c.conf.entry(conf.clone()).or_default();
        conf_bucket.resolved += 1;
        buckets.push(conf_bucket);
        for (i, b) in buckets.into_iter().enumerate() {
            b.judged += 1;
            match verdict {
                Verdict::Correct => b.correct += 1,
                Verdict::Dispatch => b.dispatch += 1,
                Verdict::False => b.false_edges += 1,
            }
            let _ = i;
        }
        if report && verdict == Verdict::False {
            let truth = match scip.defs.get(&o.symbol) {
                Some(d) if !is_local(&o.symbol) => format!("{}:{}", d.path, d.line + 1),
                _ if is_local(&o.symbol) => "local".into(),
                _ => format!("external {}", short_sym(&o.symbol)),
            };
            rep.falses.push(format!(
                "{}\t{}\t{}:{}\t{}{}\tgot {}\ttruth {}\t| {}",
                conf,
                e.kind,
                path,
                e.line,
                e.name,
                "",
                describe(Some(to)),
                truth,
                line.trim()
            ));
        }
    }
    // conf buckets double-count `resolved` above for the per-tier view; fix to judged only.
    for c in by_lang.values_mut() {
        for b in c.conf.values_mut() {
            b.resolved = b.judged;
        }
    }

    // Implementations (type -> interface/trait/base).
    let mut gt_pairs: HashSet<(i64, i64)> = HashSet::new();
    let mut gt_fam: HashMap<(i64, i64), &'static str> = HashMap::new();
    for (a, set) in &scip.implements {
        if class_of(a) != Class::Type {
            continue;
        }
        let Some(&ca) = def_to_cg.get(a.as_str()) else {
            continue;
        };
        for b in set {
            if class_of(b) != Class::Type {
                continue;
            }
            let Some(&cb) = def_to_cg.get(b.as_str()) else {
                continue;
            };
            if gt_pairs.insert((ca, cb)) {
                if let Some(f) = fam_of(cg.syms[&ca].file) {
                    gt_fam.insert((ca, cb), f);
                }
            }
        }
    }
    let mut cg_pairs: HashSet<(i64, i64)> = HashSet::new();
    // Indexers without implementation relationships (rust-analyzer) give no impl truth:
    // leave impl unscored instead of calling every codegraph impl edge false.
    let impl_edges = if scip.implements.is_empty() {
        &[][..]
    } else {
        &cg.edges[..]
    };
    for e in impl_edges {
        if !matches!(e.kind.as_str(), "impl" | "inherit") {
            continue;
        }
        if let (Some(f), Some(t)) = (e.from, e.to) {
            cg_pairs.insert((f, t));
        }
    }
    // SCIP lists every supertype, transitively (`Atom -> IObservable` through `IAtom`);
    // codegraph stores direct edges, so a chain of them answers the pair.
    let mut supers: HashMap<i64, Vec<i64>> = HashMap::new();
    for &(a, b) in &cg_pairs {
        supers.entry(a).or_default().push(b);
    }
    let reaches = |from: i64, to: i64| {
        let (mut frontier, mut seen) = (vec![from], HashSet::new());
        for _ in 0..6 {
            let mut next = Vec::new();
            for x in frontier {
                for &y in supers.get(&x).into_iter().flatten() {
                    if y == to {
                        return true;
                    }
                    if seen.insert(y) {
                        next.push(y);
                    }
                }
            }
            frontier = next;
        }
        false
    };
    // scip-typescript records no relationships for an interface's own `extends` or for
    // local symbols (classes in files outside the tsconfig, like mobx's tests): no truth.
    let no_truth = |a: i64| {
        fam_of(cg.syms[&a].file) == Some("ts")
            && (cg.syms[&a].kind == "interface"
                || cg_to_scip
                    .get(&a)
                    .is_some_and(|v| v.iter().all(|s| is_local(s))))
    };
    for p in &gt_pairs {
        let Some(f) = gt_fam.get(p) else { continue };
        let c = by_lang.entry((*f).to_string()).or_default();
        c.impl_gt += 1;
        if reaches(p.0, p.1) {
            c.impl_hit += 1;
        } else if report {
            rep.misses.push(format!(
                "ImplMiss	{} -> {}",
                describe(Some(p.0)),
                describe(Some(p.1))
            ));
        }
    }
    for p in &cg_pairs {
        let Some(f) = fam_of(cg.syms[&p.0].file) else {
            continue;
        };
        let c = by_lang.entry(f.to_string()).or_default();
        c.impl_edges.resolved += 1;
        if cg_to_scip.contains_key(&p.0) && cg_to_scip.contains_key(&p.1) && !no_truth(p.0) {
            c.impl_edges.judged += 1;
            if gt_pairs.contains(p) {
                c.impl_edges.correct += 1;
            } else {
                c.impl_edges.false_edges += 1;
                if report {
                    rep.falses.push(format!(
                        "impl\t{} -> {}",
                        describe(Some(p.0)),
                        describe(Some(p.1))
                    ));
                }
            }
        }
    }

    // Files per language.
    for (fid, (_, l)) in &cg.files {
        let _ = fid;
        if let Some(f) = lang_family(l) {
            by_lang.entry(f.to_string()).or_default().files += 1;
        }
    }
    if let Some(c) = by_lang.values_mut().next() {
        c.index_ms = index_ms;
    }
    Ok((by_lang, rep))
}

fn correct_target(
    cg: &Cg,
    scip: &Scip,
    def_text: &HashMap<&str, String>,
    sym: &str,
    to: i64,
) -> bool {
    let (Some(d), Some(s)) = (scip.defs.get(sym), cg.syms.get(&to)) else {
        return false;
    };
    let name_ok = def_text.get(sym).is_none_or(|n| *n == s.name);
    let path = &cg.files[&s.file].0;
    // Any definition site of the symbol counts (overload stubs vs implementation).
    let at_def = std::iter::once(d)
        .chain(scip.extra_defs.get(sym).into_iter().flatten())
        .any(|d| {
            let l1 = d.line + 1;
            *path == d.path && s.start <= l1 && l1 <= s.end && name_ok
        });
    at_def || python_redef(scip, sym, path, s, name_ok)
}

/// The ground truth was indexed on one machine, so for code defined per platform or
/// build configuration it only knows the active variant. codegraph cannot know which
/// variant a build uses; its target is a *platform twin* of the wanted one, and correct,
/// when both have the same name and:
/// - Rust: both are in the same file and both carry a `#[cfg(..)]` attribute;
/// - Go: both are in the same package directory, in different files that each carry a
///   build constraint (`//go:build`, `// +build`, or a GOOS/GOARCH file-name suffix);
/// - Python: codegraph's definition is a platform alternative, defined inside a
///   module-level `if`/`elif`/`else` block.
fn platform_twin(
    cg: &Cg,
    scip: &Scip,
    def_text: &HashMap<&str, String>,
    src: &mut Sources,
    sym: &str,
    to: i64,
) -> bool {
    let (Some(d), Some(s)) = (scip.defs.get(sym), cg.syms.get(&to)) else {
        return false;
    };
    if def_text.get(sym).is_some_and(|n| *n != s.name) {
        return false;
    }
    let path = cg.files[&s.file].0.clone();
    let line0 = s.start.saturating_sub(1);
    if path.ends_with(".rs") {
        path == d.path && cfg_above(src, &path, line0) && cfg_above(src, &d.path, d.line)
    } else if path.ends_with(".go") {
        let dir = |p: &str| {
            p.rsplit_once('/')
                .map_or(String::new(), |(a, _)| a.to_string())
        };
        path != d.path
            && dir(&path) == dir(&d.path)
            && go_constrained(src, &path)
            && go_constrained(src, &d.path)
    } else if path.ends_with(".py") {
        py_conditional(src, &path, line0)
    } else {
        false
    }
}

/// A `#[cfg(..)]` attribute on the item at `line0` (on it, or on the attribute/comment
/// lines right above it).
fn cfg_above(src: &mut Sources, path: &str, line0: u32) -> bool {
    let mut l = line0;
    for _ in 0..12 {
        let Some(t) = src.line(path, l).map(str::trim) else {
            return false;
        };
        if t.contains("#[cfg(") {
            return true;
        }
        let is_prefix = l == line0 || t.starts_with("#[") || t.starts_with("//") || t.is_empty();
        if !is_prefix || l == 0 {
            return false;
        }
        l -= 1;
    }
    false
}

/// A Go file restricted to some platforms (build constraint or file-name suffix).
fn go_constrained(src: &mut Sources, path: &str) -> bool {
    const SUFFIX: &[&str] = &[
        "_windows", "_linux", "_darwin", "_unix", "_other", "_freebsd", "_openbsd", "_netbsd",
        "_plan9", "_js", "_wasm", "_wasip1", "_amd64", "_arm64", "_386", "_arm",
    ];
    let stem = path
        .rsplit('/')
        .next()
        .unwrap_or(path)
        .trim_end_matches(".go");
    let stem = stem.trim_end_matches("_test");
    if SUFFIX.iter().any(|s| stem.ends_with(s)) {
        return true;
    }
    (0..20).any(|l| {
        src.line(path, l)
            .is_some_and(|t| t.starts_with("//go:build") || t.starts_with("// +build"))
    })
}

/// A Python definition at `line0` inside a module-level `if`/`elif`/`else` block.
fn py_conditional(src: &mut Sources, path: &str, line0: u32) -> bool {
    let indent = |t: &str| t.len() - t.trim_start().len();
    let Some(def) = src.line(path, line0).map(str::to_string) else {
        return false;
    };
    let own = indent(&def);
    if own == 0 {
        return false;
    }
    let mut l = line0;
    while l > 0 {
        l -= 1;
        let Some(t) = src.line(path, l).map(str::to_string) else {
            return false;
        };
        if t.trim().is_empty() || t.trim_start().starts_with('#') {
            continue;
        }
        if indent(&t) < own {
            let t = t.trim_start();
            return indent(&src.line(path, l).map(str::to_string).unwrap_or_default()) == 0
                && (t.starts_with("if ") || t.starts_with("elif ") || t.starts_with("else:"));
        }
    }
    false
}

/// scip-python records only the first `@overload` stub of a function as its definition;
/// the later stubs and the implementation are *references* to that symbol at their own
/// name token. A codegraph symbol whose name token (on its first line, same file, no
/// other definition of that name on the line) is such a reference is the same function.
fn python_redef(scip: &Scip, sym: &str, path: &str, s: &CgSym, name_ok: bool) -> bool {
    if !name_ok || !sym.starts_with("scip-python ") {
        return false;
    }
    if scip.defs.get(sym).is_none_or(|d| d.path != path) {
        return false;
    }
    let n = s.name.chars().count() as u32;
    let occs = scip.on_line(path, s.start.saturating_sub(1));
    let same_len = |o: &&Occ| o.end.saturating_sub(o.start) == n;
    occs.iter().filter(same_len).all(|o| !o.is_def)
        && occs.iter().filter(same_len).any(|o| o.symbol == sym)
}

fn short_sym(s: &str) -> String {
    s.rsplit(' ').next().unwrap_or(s).to_string()
}

fn find_edge(cg: &Cg, fid: i64, line1: u32, col1: u32, name: &str) -> Option<usize> {
    let v = cg.edge_at.get(&(fid, line1))?;
    let same: Vec<usize> = v
        .iter()
        .copied()
        .filter(|&i| cg.edges[i].name == name)
        .collect();
    same.iter()
        .copied()
        .find(|&i| cg.edges[i].col == col1)
        .or_else(|| {
            same.iter()
                .copied()
                .min_by_key(|&i| (i64::from(cg.edges[i].col) - i64::from(col1)).abs())
        })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Hit,
    Dispatch,
    Wrong,
    Unresolved,
    NoEdge,
    NoDef,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    Correct,
    Dispatch,
    False,
}

// ---- driver + rendering --------------------------------------------------------

fn git_head(dir: &Path) -> String {
    std::process::Command::new("git")
        .args(["-c", "core.fsmonitor=", "-c", "core.hooksPath=NUL", "-C"])
        .arg(dir)
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

pub fn run(
    dirs: &Dirs,
    only: &[String],
    out: Option<PathBuf>,
    baseline: Option<PathBuf>,
    write_docs: bool,
    report: bool,
) -> Result<()> {
    let c = corpus::load(dirs)?;
    let mut res = Results {
        codegraph: git_head(&dirs.bench),
        ..Results::default()
    };
    for r in c
        .repos
        .iter()
        .filter(|r| only.is_empty() || only.contains(&r.name))
    {
        if !dirs.scip(&r.name).is_file() || !dirs.corpus(&r.name).is_dir() {
            eprintln!(
                "skip {}: not prepared (run `codegraph-bench prepare`)",
                r.name
            );
            continue;
        }
        let (by_lang, rep) = score_repo(dirs, r, report)?;
        let mut total = Counts::default();
        for (l, cnt) in &by_lang {
            res.langs.entry(l.clone()).or_default().add(cnt);
            total.add(cnt);
        }
        eprintln!(
            "{:<8} calls recall {:>5.1}%  call precision {:>5.1}%  refs recall {:>5.1}%  ({} ms)",
            r.name,
            100.0 * total.calls.rate(),
            100.0 * total.call_edges.rate(),
            100.0 * total.refs.rate(),
            total.index_ms
        );
        if report {
            let dir = dirs.cache.join("report");
            std::fs::create_dir_all(&dir)?;
            std::fs::write(
                dir.join(format!("{}.misses.tsv", r.name)),
                rep.misses.join("\n"),
            )?;
            std::fs::write(
                dir.join(format!("{}.refmisses.tsv", r.name)),
                rep.ref_misses.join("\n"),
            )?;
            std::fs::write(
                dir.join(format!("{}.false.tsv", r.name)),
                rep.falses.join("\n"),
            )?;
            std::fs::write(
                dir.join(format!("{}.defs.tsv", r.name)),
                rep.defs_missing.join("\n"),
            )?;
        }
        res.repos.insert(
            r.name.clone(),
            RepoResult {
                lang: r.lang.clone(),
                rev: r.rev.clone(),
                counts: total,
            },
        );
    }
    let table = render(&res);
    println!("{table}");
    if let Some(b) = baseline {
        let text = std::fs::read_to_string(&b).with_context(|| format!("read {}", b.display()))?;
        let base: Results = serde_json::from_str(&text)?;
        println!("{}", render_delta(&base, &res));
    }
    if let Some(o) = out {
        if let Some(p) = o.parent() {
            std::fs::create_dir_all(p)?;
        }
        std::fs::write(&o, serde_json::to_string_pretty(&res)?)?;
        eprintln!("wrote {}", o.display());
    }
    if write_docs {
        let doc = dirs.bench.join("..").join("docs").join("ACCURACY.md");
        let text = std::fs::read_to_string(&doc).unwrap_or_default();
        let (a, b) = ("<!-- results:start -->", "<!-- results:end -->");
        let new = match (text.find(a), text.find(b)) {
            (Some(i), Some(j)) if i < j => {
                format!("{}{a}\n{table}\n{}", &text[..i], &text[j..])
            }
            _ => format!("{text}\n{a}\n{table}\n{b}\n"),
        };
        std::fs::write(&doc, new)?;
        eprintln!("updated {}", doc.display());
    }
    Ok(())
}

fn pct(x: f64) -> String {
    if x.is_nan() {
        "–".into()
    } else {
        format!("{:.1}%", 100.0 * x)
    }
}

fn lang_label(l: &str) -> &str {
    match l {
        "go" => "Go",
        "rust" => "Rust",
        "ts" => "TypeScript/JS",
        "python" => "Python",
        _ => l,
    }
}

/// Markdown tables: per language, calibration, per repo.
pub fn render(r: &Results) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "codegraph `{}`\n", r.codegraph);
    s.push_str(
        "| language | files | defs found | callers recall | callers precision | false call edges | refs recall | refs precision | impl recall | impl precision |\n",
    );
    s.push_str("|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|\n");
    let mut all = Counts::default();
    for (l, c) in &r.langs {
        all.add(c);
        row(&mut s, lang_label(l), c);
    }
    row(&mut s, "**all**", &all);
    s.push_str("\nConfidence calibration (share of judged edges that are correct):\n\n");
    s.push_str("| language | exact | likely | name_only |\n|---|---:|---:|---:|\n");
    for (l, c) in r
        .langs
        .iter()
        .chain(std::iter::once((&"all".to_string(), &all)))
    {
        let cell = |k: &str| {
            c.conf.get(k).map_or("–".into(), |p| {
                format!(
                    "{} ({}/{})",
                    pct(p.rate()),
                    p.correct + p.dispatch,
                    p.judged
                )
            })
        };
        let _ = writeln!(
            s,
            "| {} | {} | {} | {} |",
            lang_label(l),
            cell("exact"),
            cell("likely"),
            cell("name_only")
        );
    }
    s.push_str("\nPer repo:\n\n| repo | lang | callers recall | callers precision | GT call sites | resolved call edges | refs recall |\n|---|---|---:|---:|---:|---:|---:|\n");
    for (name, rr) in &r.repos {
        let c = &rr.counts;
        let _ = writeln!(
            s,
            "| {name} | {} | {} | {} | {} | {} | {} |",
            rr.lang,
            pct(c.calls.rate()),
            pct(c.call_edges.rate()),
            c.calls.gt,
            c.call_edges.resolved,
            pct(c.refs.rate())
        );
    }
    s
}

fn row(s: &mut String, label: &str, c: &Counts) {
    let _ = writeln!(
        s,
        "| {label} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
        c.files,
        pct(ratio(c.defs_found, c.defs_gt)),
        pct(c.calls.rate()),
        pct(c.call_edges.rate()),
        pct(c.call_edges.false_rate()),
        pct(c.refs.rate()),
        pct(c.all_edges.rate()),
        pct(ratio(c.impl_hit, c.impl_gt)),
        pct(c.impl_edges.rate()),
    );
}

fn render_delta(base: &Results, cur: &Results) -> String {
    let mut s = String::from("\nDelta vs baseline (percentage points):\n\n| language | callers recall | callers precision | refs recall | refs precision | impl recall |\n|---|---:|---:|---:|---:|---:|\n");
    let d = |a: f64, b: f64| {
        if a.is_nan() || b.is_nan() {
            "–".to_string()
        } else {
            format!("{:+.1}", 100.0 * (b - a))
        }
    };
    for (l, c) in &cur.langs {
        let Some(b) = base.langs.get(l) else { continue };
        let _ = writeln!(
            s,
            "| {} | {} | {} | {} | {} | {} |",
            lang_label(l),
            d(b.calls.rate(), c.calls.rate()),
            d(b.call_edges.rate(), c.call_edges.rate()),
            d(b.refs.rate(), c.refs.rate()),
            d(b.all_edges.rate(), c.all_edges.rate()),
            d(ratio(b.impl_hit, b.impl_gt), ratio(c.impl_hit, c.impl_gt)),
        );
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn call_syntax() {
        assert!(is_call("(x)"));
        assert!(is_call(" (x)"));
        assert!(is_call("::<T>(x)"));
        assert!(is_call("<T>(x)"));
        assert!(is_call("[int](x)"));
        assert!(!is_call(", y)"));
        assert!(!is_call(" < b; f(x)"));
        assert!(!is_call(""));
    }

    #[test]
    fn utf16_ranges() {
        let line = "é := foo(x)";
        // `foo` at byte 6, UTF-16 unit 5
        assert_eq!(byte_range(line, 5, 8), Some((6, 9)));
        assert_eq!(byte_range(line, 6, 9), Some((6, 9)));
    }
}

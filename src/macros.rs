//! Rust `macro_rules!` expansion at extraction time.
//!
//! A `macro_rules!` macro defined in a file is expanded where that file invokes it. The
//! invocation's tokens are matched against the rules the way rustc does it (fragments,
//! repetitions with separators, the first arm that matches wins), the arm's transcriber is
//! instantiated, and the resulting text is parsed and extracted like a small file of its
//! own: inside a copy of the enclosing `impl`/`trait` header for items of an impl, as
//! items at module level, inside a throwaway function anywhere else. Everything extracted
//! from that text whose *name token came from the invocation* is mapped back to the
//! invocation's position: `deserialize_number!(deserialize_i32)` in an impl defines the
//! method `deserialize_i32` there, and `stream.next()` in the tokens handed to
//! `test_stream!(data, Value, |stream| { .. })` gets `stream`'s type from the expansion
//! (`let mut $stream = de.into_iter::<$ty>()`). Names written only in the macro body
//! (a literal `fn helper()`) have no position of their own in the invoking file and are
//! dropped. A transcriber invoking a macro expands again, up to [`MAX_DEPTH`] levels.
//!
//! Not covered: macros defined in another file (`#[macro_use]`, `$crate::m!`), procedural
//! macros, hygiene (a macro-body local shadowing a caller's name is not modeled), and
//! textual scope (a macro redefined further down expands with its last definition
//! everywhere in the file).

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use crate::clock::Instant;

use tree_sitter::Node;

use crate::extract::FileFacts;

/// Expansion levels (an expansion invoking a macro is level 2).
pub(crate) const MAX_DEPTH: u8 = 4;
/// Bytes of one transcribed expansion; larger ones are skipped.
const MAX_EXPANSION: usize = 256 * 1024;
/// Invocations expanded per file.
const MAX_INVOCATIONS: usize = 4096;
/// Nesting of a definition or an invocation's arguments; deeper ones are not expanded
/// (matching and transcription recurse along it).
const MAX_NEST: usize = 64;
/// Expansions per file, all levels together.
const MAX_EXPANSIONS: usize = 4096;
/// Wrapper function around expansions in statement/expression position.
const STMT_WRAPPER: &str = "__codegraph_expansion";

#[derive(Clone, Debug)]
struct Tok {
    text: String,
    /// Directly after the previous token of its source (no whitespace between).
    glued: bool,
    /// A line break precedes it in its source (kept in expansions, so a definition's
    /// signature is its own line).
    newline: bool,
    /// Byte offset in the source being expanded, when the token comes from there.
    origin: Option<usize>,
}

#[derive(Clone, Debug)]
enum Tt {
    Leaf(Tok),
    Group {
        open: Tok,
        close: Tok,
        inner: Vec<Tt>,
    },
}

#[derive(Clone, Debug)]
enum Pat {
    Tok(String),
    Frag {
        name: String,
        kind: String,
    },
    Rep {
        inner: Vec<Pat>,
        sep: Option<String>,
        op: String,
    },
    Group {
        open: String,
        inner: Vec<Pat>,
    },
}

#[derive(Clone, Debug)]
enum Tr {
    Tok(Tok),
    Var(String, Tok),
    /// `$( .. ) sep op`: the count comes from the bindings, so `op` is not kept.
    Rep {
        inner: Vec<Tr>,
        sep: Option<Tok>,
    },
    Group {
        open: Tok,
        close: Tok,
        inner: Vec<Tr>,
    },
}

#[derive(Clone, Debug)]
enum Bind {
    One(Vec<Tt>),
    Seq(Vec<Bind>),
}

type Binds = HashMap<String, Bind>;

/// One `(matcher) => { transcriber }` arm.
type Rule = (Vec<Pat>, Vec<Tr>);

/// The `macro_rules!` macros visible to an expansion: name -> rules (matcher, transcriber).
#[derive(Clone, Debug, Default)]
pub(crate) struct Macros {
    rules: HashMap<String, Vec<Rule>>,
}

/// Macros and depth handed to the extraction of an expansion.
pub(crate) struct Ctx<'a> {
    pub(crate) macros: &'a Macros,
    pub(crate) depth: u8,
    /// Expansions left for the whole file, shared by every level (a macro whose
    /// expansion invokes it many times over would otherwise multiply per level).
    budget: &'a Cell<usize>,
}

fn text<'a>(n: Node<'_>, src: &'a [u8]) -> &'a str {
    n.utf8_text(src).unwrap_or("")
}

fn children(n: Node<'_>) -> Vec<Node<'_>> {
    let mut c = n.walk();
    n.children(&mut c).collect()
}

fn is_delim(s: &str) -> bool {
    matches!(s, "(" | ")" | "[" | "]" | "{" | "}")
}

/// Comments come through as tokens of a token tree; an expansion is one text, so a `//`
/// kept in it would comment out the rest.
fn is_comment(n: Node<'_>) -> bool {
    matches!(n.kind(), "line_comment" | "block_comment")
}

/// Whether `src[from..to]` holds a line break.
fn breaks(src: &[u8], from: usize, to: usize) -> bool {
    src.get(from..to).is_some_and(|b| b.contains(&b'\n'))
}

// ---- reading the tree ----------------------------------------------------------------

/// Tokens between the delimiters of a `token_tree`.
fn tts_of(group: Node<'_>, src: &[u8]) -> Vec<Tt> {
    let ch = children(group);
    let inner = if ch.len() >= 2 && is_delim(ch[0].kind()) {
        &ch[1..ch.len() - 1]
    } else {
        &ch[..]
    };
    let mut prev_end = ch.first().map_or(group.start_byte(), Node::end_byte);
    let mut out = Vec::with_capacity(inner.len());
    for &c in inner.iter().filter(|c| !is_comment(**c)) {
        let glued = c.start_byte() == prev_end;
        let newline = breaks(src, prev_end, c.start_byte());
        prev_end = c.end_byte();
        out.push(tt_of(c, glued, newline, src));
    }
    out
}

fn tt_of(n: Node<'_>, glued: bool, newline: bool, src: &[u8]) -> Tt {
    if n.kind() == "token_tree" {
        let ch = children(n);
        if let (Some(first), Some(last)) = (ch.first(), ch.last()) {
            if is_delim(first.kind()) && ch.len() >= 2 {
                let before_close = ch[ch.len() - 2].end_byte();
                return Tt::Group {
                    open: leaf(*first, glued, newline, src),
                    close: leaf(
                        *last,
                        false,
                        breaks(src, before_close, last.start_byte()),
                        src,
                    ),
                    inner: tts_of(n, src),
                };
            }
        }
    }
    Tt::Leaf(leaf(n, glued, newline, src))
}

fn leaf(n: Node<'_>, glued: bool, newline: bool, src: &[u8]) -> Tok {
    Tok {
        text: text(n, src).to_string(),
        glued,
        newline,
        origin: Some(n.start_byte()),
    }
}

/// Matcher of a `token_tree_pattern` / the inside of a `token_repetition_pattern`.
fn pats_of(nodes: &[Node<'_>], src: &[u8]) -> Vec<Pat> {
    let mut out = Vec::new();
    for &c in nodes.iter().filter(|c| !is_comment(**c)) {
        match c.kind() {
            "token_binding_pattern" => {
                let name = c
                    .child_by_field_name("name")
                    .map(|n| text(n, src).trim_start_matches('$').to_string())
                    .unwrap_or_default();
                let kind = c
                    .child_by_field_name("type")
                    .map(|n| text(n, src).to_string())
                    .unwrap_or_default();
                out.push(Pat::Frag { name, kind });
            }
            "token_repetition_pattern" => {
                let (inner, sep, op) = repetition(c, src);
                out.push(Pat::Rep {
                    inner: pats_of(&inner, src),
                    sep,
                    op,
                });
            }
            "token_tree_pattern" => {
                let ch = children(c);
                let open = ch
                    .first()
                    .map(|f| text(*f, src).to_string())
                    .unwrap_or_default();
                let inner = if ch.len() >= 2 {
                    &ch[1..ch.len() - 1]
                } else {
                    &ch[..]
                };
                out.push(Pat::Group {
                    open,
                    inner: pats_of(inner, src),
                });
            }
            _ => out.push(Pat::Tok(text(c, src).to_string())),
        }
    }
    out
}

/// `$( inner ) sep? op` -> (inner nodes, separator, operator). The grammar matches the
/// separator with an unnamed regex, so it is no node: it is the text between `)` and `op`.
fn repetition<'t>(n: Node<'t>, src: &[u8]) -> (Vec<Node<'t>>, Option<String>, String) {
    let ch = children(n);
    let open = ch.iter().position(|c| c.kind() == "(");
    let close = ch.iter().rposition(|c| c.kind() == ")");
    let (Some(o), Some(cl)) = (open, close) else {
        return (Vec::new(), None, "*".into());
    };
    let op = ch[cl + 1..]
        .iter()
        .rfind(|c| matches!(c.kind(), "*" | "+" | "?"));
    let sep_end = op.map_or(n.end_byte(), Node::start_byte);
    let sep = std::str::from_utf8(&src[ch[cl].end_byte()..sep_end])
        .ok()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let op = op.map_or_else(|| "*".into(), |o| text(*o, src).to_string());
    (ch[o + 1..cl].to_vec(), sep, op)
}

/// Transcriber tokens of a `token_tree` / the inside of a `token_repetition`.
fn trs_of(nodes: &[Node<'_>], mut prev_end: usize, src: &[u8]) -> Vec<Tr> {
    let mut out = Vec::new();
    for &c in nodes.iter().filter(|c| !is_comment(**c)) {
        let glued = c.start_byte() == prev_end;
        let newline = breaks(src, prev_end, c.start_byte());
        prev_end = c.end_byte();
        let tok = |n: Node<'_>, glued: bool| Tok {
            text: text(n, src).to_string(),
            glued,
            newline: newline && !glued,
            origin: None,
        };
        match c.kind() {
            "metavariable" => {
                let name = text(c, src).trim_start_matches('$').to_string();
                out.push(Tr::Var(name, tok(c, glued)));
            }
            "token_repetition" => {
                let (inner, sep, _) = repetition(c, src);
                let start = inner.first().map_or(c.start_byte(), Node::start_byte);
                out.push(Tr::Rep {
                    inner: trs_of(&inner, start, src),
                    sep: sep.map(|text| Tok {
                        text,
                        glued: true,
                        newline: false,
                        origin: None,
                    }),
                });
            }
            "token_tree" => {
                let ch = children(c);
                if ch.len() >= 2 && is_delim(ch[0].kind()) {
                    out.push(Tr::Group {
                        open: tok(ch[0], glued),
                        close: tok(
                            ch[ch.len() - 1],
                            ch[ch.len() - 1].start_byte() == ch[ch.len() - 2].end_byte(),
                        ),
                        inner: trs_of(&ch[1..ch.len() - 1], ch[0].end_byte(), src),
                    });
                } else {
                    out.push(Tr::Tok(tok(c, glued)));
                }
            }
            _ => out.push(Tr::Tok(tok(c, glued))),
        }
    }
    out
}

/// Every `macro_rules!` definition under `root`.
fn definitions(root: Node<'_>, src: &[u8], into: &mut Macros) {
    walk(root, &mut |n| {
        if n.kind() != "macro_definition" {
            return true;
        }
        let Some(name) = n.child_by_field_name("name") else {
            return false;
        };
        if !shallow(n, MAX_NEST) {
            return false;
        }
        let mut arms = Vec::new();
        for rule in children(n).into_iter().filter(|c| c.kind() == "macro_rule") {
            let (Some(left), Some(right)) = (
                rule.child_by_field_name("left"),
                rule.child_by_field_name("right"),
            ) else {
                continue;
            };
            let lch = children(left);
            let pats = if lch.len() >= 2 && is_delim(lch[0].kind()) {
                pats_of(&lch[1..lch.len() - 1], src)
            } else {
                pats_of(&lch, src)
            };
            let rch = children(right);
            let trs = if rch.len() >= 2 && is_delim(rch[0].kind()) {
                trs_of(&rch[1..rch.len() - 1], rch[0].end_byte(), src)
            } else {
                trs_of(&rch, right.start_byte(), src)
            };
            arms.push((pats, trs));
        }
        into.rules.insert(text(name, src).to_string(), arms);
        false
    });
}

/// Whether the subtree under `n` is at most `limit` levels deep (iterative).
fn shallow(n: Node<'_>, limit: usize) -> bool {
    let mut cur = n.walk();
    let mut depth = 0usize;
    loop {
        if cur.goto_first_child() {
            depth += 1;
            if depth > limit {
                return false;
            }
            continue;
        }
        loop {
            if depth == 0 {
                return true;
            }
            if cur.goto_next_sibling() {
                break;
            }
            cur.goto_parent();
            depth -= 1;
        }
    }
}

/// Pre-order walk; `f` returns whether to descend into the node.
fn walk<'t>(root: Node<'t>, f: &mut dyn FnMut(Node<'t>) -> bool) {
    let mut cur = root.walk();
    let mut descend = f(root);
    loop {
        if descend && cur.goto_first_child() {
            descend = f(cur.node());
            continue;
        }
        loop {
            if cur.node().id() == root.id() {
                return;
            }
            if cur.goto_next_sibling() {
                descend = f(cur.node());
                break;
            }
            if !cur.goto_parent() {
                return;
            }
        }
    }
}

// ---- matching ------------------------------------------------------------------------

fn match_all(pats: &[Pat], tts: &[Tt]) -> Option<Binds> {
    let (n, b) = match_prefix(pats, tts)?;
    (n == tts.len()).then_some(b)
}

fn leaf_text(t: Option<&Tt>) -> Option<&str> {
    match t {
        Some(Tt::Leaf(k)) => Some(k.text.as_str()),
        _ => None,
    }
}

/// Match `pats` against a prefix of `tts`: (tokens consumed, bindings).
fn match_prefix(pats: &[Pat], tts: &[Tt]) -> Option<(usize, Binds)> {
    let mut binds = Binds::new();
    let mut i = 0;
    for p in pats {
        match p {
            Pat::Tok(t) => {
                if leaf_text(tts.get(i)) != Some(t.as_str()) {
                    return None;
                }
                i += 1;
            }
            Pat::Group { open, inner } => match tts.get(i) {
                Some(Tt::Group {
                    open: o, inner: ti, ..
                }) if o.text == *open => {
                    binds.extend(match_all(inner, ti)?);
                    i += 1;
                }
                _ => return None,
            },
            Pat::Frag { name, kind } => {
                let n = frag_len(kind, &tts[i..])?;
                binds.insert(name.clone(), Bind::One(tts[i..i + n].to_vec()));
                i += n;
            }
            Pat::Rep { inner, sep, op } => {
                let mut iters: Vec<Binds> = Vec::new();
                loop {
                    if op == "?" && !iters.is_empty() {
                        break;
                    }
                    let mut k = i;
                    if !iters.is_empty() {
                        if let Some(s) = sep {
                            if leaf_text(tts.get(k)) != Some(s.as_str()) {
                                break;
                            }
                            k += 1;
                        }
                    }
                    let Some((n, b)) = match_prefix(inner, &tts[k..]) else {
                        break;
                    };
                    if n == 0 {
                        break;
                    }
                    iters.push(b);
                    i = k + n;
                }
                if op == "+" && iters.is_empty() {
                    return None;
                }
                let mut vars = Vec::new();
                pat_vars(inner, &mut vars);
                for v in vars {
                    let seq = iters
                        .iter()
                        .map(|b| b.get(&v).cloned().unwrap_or(Bind::Seq(Vec::new())))
                        .collect();
                    binds.insert(v, Bind::Seq(seq));
                }
            }
        }
    }
    Some((i, binds))
}

fn pat_vars(pats: &[Pat], out: &mut Vec<String>) {
    for p in pats {
        match p {
            Pat::Frag { name, .. } => out.push(name.clone()),
            Pat::Rep { inner, .. } | Pat::Group { inner, .. } => pat_vars(inner, out),
            Pat::Tok(_) => {}
        }
    }
}

fn is_ident(s: &str) -> bool {
    let mut c = s.chars();
    c.next().is_some_and(|f| f.is_alphabetic() || f == '_')
        && s != "_"
        && s.chars().all(|x| x.is_alphanumeric() || x == '_')
}

fn is_literal(s: &str) -> bool {
    // `'a'` is a char, `'a` a lifetime; `r#"` a raw string, `r#type` a raw identifier.
    let char_lit = s.len() >= 3 && s.starts_with('\'') && s.ends_with('\'');
    s.starts_with(|c: char| c.is_ascii_digit() || c == '"')
        || char_lit
        || [
            "b\"", "b'", "r\"", "r#\"", "r##", "br\"", "br#", "c\"", "cr\"", "cr#",
        ]
        .iter()
        .any(|p| s.starts_with(p))
        || matches!(s, "true" | "false")
}

/// Tokens a fragment of `kind` takes from the front of `tts`, rustc-style: an expression,
/// type or pattern runs to the first token of its follow set at the top level.
fn frag_len(kind: &str, tts: &[Tt]) -> Option<usize> {
    let first = tts.first();
    match kind {
        "ident" => leaf_text(first).filter(|t| is_ident(t)).map(|_| 1),
        "lifetime" => leaf_text(first).filter(|t| t.starts_with('\'')).map(|_| 1),
        "literal" => match leaf_text(first) {
            Some(t) if is_literal(t) => Some(1),
            Some("-") if leaf_text(tts.get(1)).is_some_and(is_literal) => Some(2),
            _ => None,
        },
        "tt" => first.map(|_| 1),
        "block" => match first {
            Some(Tt::Group { open, .. }) if open.text == "{" => Some(1),
            _ => None,
        },
        "vis" => match leaf_text(first) {
            Some("pub") => Some(match tts.get(1) {
                Some(Tt::Group { open, .. }) if open.text == "(" => 2,
                _ => 1,
            }),
            _ => Some(0),
        },
        "expr" | "expr_2021" | "stmt" => {
            let stops: &[&str] = if kind == "stmt" {
                &[";"]
            } else {
                &[",", ";", "=>"]
            };
            // A closure's parameter list may hold commas: `|a, b| a + b`.
            let skip = match leaf_text(first) {
                Some("move") if leaf_text(tts.get(1)) == Some("|") => closing_pipe(tts, 2),
                Some("|") => closing_pipe(tts, 1),
                _ => 0,
            };
            let rest = &tts[skip..];
            let len = until(rest, stops, false);
            nonzero(skip + len.min(juxtaposed(&rest[..len])))
        }
        "ty" | "path" => nonzero(until(
            tts,
            &[",", ";", "=>", "=", "|", "{", "as", "where"],
            true,
        )),
        "pat" => nonzero(until(tts, &["=>", ",", "=", "if", "in"], false)),
        "pat_param" => nonzero(until(tts, &["=>", ",", "=", "if", "in", "|"], false)),
        "meta" => nonzero(until(tts, &[","], false)),
        "item" => nonzero(tts.len()),
        _ => None,
    }
}

fn nonzero(n: usize) -> Option<usize> {
    (n > 0).then_some(n)
}

/// Where an expression ends because the next token starts another operand: `$($n:expr)*`
/// over `f32::consts::E f32::consts::PI` takes one path each, as rustc's parser does. Two
/// operands are only ever adjacent through a keyword (`x as u8`, `if c {..} else {..}`,
/// `return v`).
fn juxtaposed(tts: &[Tt]) -> usize {
    const KEYWORDS: &[&str] = &[
        "as", "async", "await", "box", "break", "const", "continue", "dyn", "else", "enum",
        "extern", "fn", "for", "gen", "if", "impl", "in", "let", "loop", "match", "mod", "move",
        "mut", "pub", "ref", "return", "static", "struct", "trait", "try", "type", "union",
        "unsafe", "use", "where", "while", "yield",
    ];
    let operand = |t: &Tt| match t {
        Tt::Leaf(k) => {
            let s = k.text.as_str();
            !KEYWORDS.contains(&s) && (is_ident(s) || is_literal(s) || s.starts_with("r#"))
        }
        Tt::Group { .. } => false,
    };
    let ends_operand = |t: &Tt| matches!(t, Tt::Group { .. }) || operand(t);
    tts.windows(2)
        .position(|w| ends_operand(&w[0]) && operand(&w[1]))
        .map_or(tts.len(), |i| i + 1)
}

/// Index just past the `|` closing a closure parameter list that starts at `from`.
fn closing_pipe(tts: &[Tt], from: usize) -> usize {
    tts[from..]
        .iter()
        .position(|t| leaf_text(Some(t)) == Some("|"))
        .map_or(0, |p| from + p + 1)
}

/// Tokens before the first top-level stop token (angle brackets nest when `angles`).
fn until(tts: &[Tt], stops: &[&str], angles: bool) -> usize {
    let mut depth = 0i32;
    for (i, t) in tts.iter().enumerate() {
        let Some(s) = leaf_text(Some(t)) else {
            continue;
        };
        if angles {
            match s {
                "<" => depth += 1,
                ">" => depth -= 1,
                ">>" => depth -= 2,
                _ => {}
            }
            if depth < 0 {
                return i;
            }
        }
        if depth <= 0 && stops.contains(&s) {
            return i;
        }
    }
    tts.len()
}

// ---- transcription ---------------------------------------------------------------------

#[derive(Default)]
struct Out {
    text: String,
    /// (output offset of a token, its origin), in output order.
    map: Vec<(usize, usize)>,
}

impl Out {
    fn tok(&mut self, t: &Tok, glued: bool) {
        self.put(t, glued, t.newline);
    }

    fn put(&mut self, t: &Tok, glued: bool, newline: bool) {
        if !glued && !self.text.is_empty() {
            self.text.push(if newline { '\n' } else { ' ' });
        }
        if let Some(o) = t.origin {
            self.map.push((self.text.len(), o));
        }
        self.text.push_str(&t.text);
    }

    /// A fragment's tokens; the first is spaced like `lead` (the `$var` it replaces), or
    /// glued when there is none (the inside of a group).
    fn tts(&mut self, tts: &[Tt], lead: Option<&Tok>) {
        for (i, t) in tts.iter().enumerate() {
            let space = |k: &Tok| match (i, lead) {
                (0, Some(l)) => (l.glued, l.newline),
                (0, None) => (true, false),
                _ => (k.glued, k.newline),
            };
            match t {
                Tt::Leaf(k) => {
                    let (g, n) = space(k);
                    self.put(k, g, n);
                }
                Tt::Group { open, close, inner } => {
                    let (g, n) = space(open);
                    self.put(open, g, n);
                    self.tts(inner, None);
                    self.tok(close, close.glued || inner.is_empty());
                }
            }
        }
    }

    /// The origin of the token starting at output offset `at`.
    fn origin(&self, at: usize) -> Option<usize> {
        self.map
            .binary_search_by_key(&at, |&(o, _)| o)
            .ok()
            .map(|i| self.map[i].1)
    }
}

fn lookup<'b>(binds: &'b Binds, name: &str, idx: &[usize]) -> Option<&'b Bind> {
    let mut b = binds.get(name)?;
    for &i in idx {
        match b {
            Bind::Seq(v) => b = v.get(i)?,
            Bind::One(_) => break,
        }
    }
    Some(b)
}

fn tr_vars(trs: &[Tr], out: &mut Vec<String>) {
    for t in trs {
        match t {
            Tr::Var(n, _) => out.push(n.clone()),
            Tr::Rep { inner, .. } | Tr::Group { inner, .. } => tr_vars(inner, out),
            Tr::Tok(_) => {}
        }
    }
}

fn transcribe(trs: &[Tr], binds: &Binds, idx: &mut Vec<usize>, out: &mut Out) -> Option<()> {
    for t in trs {
        if out.text.len() > MAX_EXPANSION {
            return None;
        }
        match t {
            Tr::Tok(k) => out.tok(k, k.glued),
            Tr::Var(name, k) => match lookup(binds, name, idx) {
                Some(Bind::One(tts)) => out.tts(tts, Some(k)),
                Some(Bind::Seq(_)) => return None,
                // `$crate` and unbound names stay as written (`$crate` -> `crate`).
                None if name == "crate" => out.tok(
                    &Tok {
                        text: "crate".into(),
                        glued: k.glued,
                        newline: k.newline,
                        origin: None,
                    },
                    k.glued,
                ),
                None => out.tok(k, k.glued),
            },
            Tr::Group { open, close, inner } => {
                out.tok(open, open.glued);
                transcribe(inner, binds, idx, out)?;
                out.tok(close, close.glued || inner.is_empty());
            }
            Tr::Rep { inner, sep, .. } => {
                let mut vars = Vec::new();
                tr_vars(inner, &mut vars);
                let count = vars
                    .iter()
                    .find_map(|v| match lookup(binds, v, idx) {
                        Some(Bind::Seq(s)) => Some(s.len()),
                        _ => None,
                    })
                    .unwrap_or(0);
                for i in 0..count {
                    if i > 0 {
                        if let Some(s) = sep {
                            out.tok(s, true);
                        }
                    }
                    idx.push(i);
                    let r = transcribe(inner, binds, idx, out);
                    idx.pop();
                    r?;
                }
            }
        }
    }
    Some(())
}

// ---- expansion -------------------------------------------------------------------------

/// Line start offsets of `src`.
fn line_starts(src: &[u8]) -> Vec<usize> {
    std::iter::once(0)
        .chain(
            src.iter()
                .enumerate()
                .filter(|&(_, &b)| b == b'\n')
                .map(|(i, _)| i + 1),
        )
        .collect()
}

/// 1-based (line, column) of byte `at`.
fn line_col(starts: &[usize], at: usize) -> (u32, u32) {
    let line = starts.partition_point(|&s| s <= at).max(1);
    (line as u32, (at - starts[line - 1]) as u32 + 1)
}

/// Where an invocation sits: the text wrapped around its expansion so it parses as the
/// right thing.
fn context(inv: Node<'_>, src: &[u8]) -> (String, String) {
    let mut p = inv.parent();
    if p.is_some_and(|x| x.kind() == "expression_statement") {
        p = p.and_then(|x| x.parent());
    }
    match p {
        Some(list) if list.kind() == "declaration_list" => match list.parent() {
            Some(owner) if matches!(owner.kind(), "impl_item" | "trait_item") => {
                let head = String::from_utf8_lossy(&src[owner.start_byte()..list.start_byte()]);
                (format!("{head}{{\n"), "\n}\n".into())
            }
            _ => (String::new(), String::new()),
        },
        Some(f) if f.kind() == "source_file" => (String::new(), String::new()),
        _ => (format!("fn {STMT_WRAPPER}() {{\n"), "\n}\n".into()),
    }
}

/// Expansion results waiting to be merged into a file's facts (once, at the end).
struct Pending {
    /// Symbols and references extraction found before expanding.
    orig_syms: usize,
    orig_refs: usize,
    /// Symbols expansions defined, by (name position, name) -> index in `facts.symbols`.
    defined: HashMap<(usize, String), usize>,
    /// (line, col, name) of every reference an expansion saw: it replaces the original.
    seen: HashSet<(u32, u32, String)>,
}

/// Expand every invocation in `root` of a `macro_rules!` macro defined in this file (or
/// handed down by an enclosing expansion) and splice what the expansions define and
/// reference into `facts`.
pub(crate) fn expand_file(
    root: Node<'_>,
    src: &[u8],
    start: Instant,
    outer: Option<&Ctx<'_>>,
    facts: &mut FileFacts,
) {
    let depth = outer.map_or(0, |c| c.depth);
    if depth >= MAX_DEPTH {
        return;
    }
    // The enclosing file's macros, plus this source's own when it defines any (only then
    // is the table copied: an expansion rarely defines macros).
    let owned: Macros;
    let table: &Macros = if memchr_str(src, b"macro_rules") {
        let mut t = outer.map(|c| c.macros.clone()).unwrap_or_default();
        definitions(root, src, &mut t);
        owned = t;
        &owned
    } else {
        match outer {
            Some(c) => c.macros,
            None => return,
        }
    };
    if table.rules.is_empty() {
        return;
    }
    let mut invocations: Vec<Inv<'_>> = Vec::new();
    known_invocations(root, src, table, &mut invocations);
    if invocations.is_empty() {
        return;
    }
    let starts = line_starts(src);
    let own_budget = Cell::new(MAX_EXPANSIONS);
    let budget = outer.map_or(&own_budget, |c| c.budget);
    let ctx = Ctx {
        macros: table,
        depth: depth + 1,
        budget,
    };
    let mut pend = Pending {
        orig_syms: facts.symbols.len(),
        orig_refs: facts.refs.len(),
        defined: HashMap::new(),
        seen: HashSet::new(),
    };
    let mut next = 0;
    while let Some(&inv) = invocations.get(next) {
        next += 1;
        if start.elapsed() > crate::extract::HINT_BUDGET || budget.get() == 0 {
            // Some invocations stay unexpanded: the graph depends on timing.
            facts.limited = true;
            break;
        }
        budget.set(budget.get() - 1);
        let (name, args) = (text(inv.name, src), inv.args);
        if !shallow(args, MAX_NEST) {
            continue;
        }
        let tts = tts_of(args, src);
        let Some(out) = table.rules[name].iter().find_map(|(pats, trs)| {
            let binds = match_all(pats, &tts)?;
            let mut out = Out::default();
            transcribe(trs, &binds, &mut Vec::new(), &mut out)?;
            Some(out)
        }) else {
            // Not expandable: invocations among its arguments still are, on their own.
            known_invocations(args, src, table, &mut invocations);
            continue;
        };
        if out.map.is_empty() {
            continue;
        }
        let (head, tail) = context(inv.anchor, src);
        let synthetic = format!("{head}{}{tail}", out.text);
        let Ok(sub) = crate::extract::extract_expansion(synthetic.as_bytes(), start, &ctx) else {
            continue;
        };
        facts.limited |= sub.limited;
        let map = |at: usize| at.checked_sub(head.len()).and_then(|x| out.origin(x));
        splice(
            facts,
            &mut pend,
            sub,
            synthetic.as_bytes(),
            &map,
            inv.anchor,
            &starts,
        );
    }
    finish(facts, &pend);
}

/// One invocation of a known macro: `name!(args)` as a node, or as tokens inside another
/// macro's arguments (`assert_eq!(m!(x), y)`), which tree-sitter does not parse.
#[derive(Clone, Copy)]
struct Inv<'t> {
    name: Node<'t>,
    args: Node<'t>,
    /// Decides the context the expansion is parsed in ([`context`]).
    anchor: Node<'t>,
}

/// Invocations of `table`'s macros under `root`, outside macro definitions and without
/// descending into their arguments: an invocation passed to another reaches the expansion
/// through its tokens (and is queued on its own when the outer one does not expand).
fn known_invocations<'t>(root: Node<'t>, src: &[u8], table: &Macros, out: &mut Vec<Inv<'t>>) {
    let known = |n: Node<'_>| n.kind() == "identifier" && table.rules.contains_key(text(n, src));
    let mut skip: HashSet<usize> = HashSet::new();
    let top = root.id();
    walk(root, &mut |n| {
        if n.id() != top && skip.contains(&n.id()) {
            return false;
        }
        match n.kind() {
            "macro_definition" => false,
            "macro_invocation" if n.id() != top => {
                let name = n.child_by_field_name("macro").filter(|m| known(*m));
                let args = children(n).into_iter().find(|c| c.kind() == "token_tree");
                if let (Some(name), Some(args)) = (name, args) {
                    if out.len() < MAX_INVOCATIONS {
                        out.push(Inv {
                            name,
                            args,
                            anchor: n,
                        });
                    }
                    return false;
                }
                true
            }
            "token_tree" => {
                let ch = children(n);
                for w in ch.windows(3) {
                    if known(w[0])
                        && w[1].kind() == "!"
                        && w[2].kind() == "token_tree"
                        && out.len() < MAX_INVOCATIONS
                    {
                        skip.insert(w[2].id());
                        out.push(Inv {
                            name: w[0],
                            args: w[2],
                            anchor: w[0],
                        });
                    }
                }
                true
            }
            _ => true,
        }
    });
}

fn memchr_str(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

/// Append the symbols and references of one expansion whose names map back to the
/// invoking source. Indexes into `facts.symbols` stay valid until [`finish`] re-sorts.
fn splice(
    facts: &mut FileFacts,
    pend: &mut Pending,
    sub: FileFacts,
    synthetic: &[u8],
    map: &dyn Fn(usize) -> Option<usize>,
    inv: Node<'_>,
    starts: &[usize],
) {
    const SEP: &str = "::";
    let sub_starts = line_starts(synthetic);
    let at = |line: u32, col: u32| -> Option<usize> {
        let s = *sub_starts.get(line.checked_sub(1)? as usize)?;
        Some(s + col.checked_sub(1)? as usize)
    };
    let orig_syms = pend.orig_syms;
    let original =
        |facts: &FileFacts, pos: usize| crate::extract::enclosing(&facts.symbols[..orig_syms], pos);
    let outer_parent = original(facts, inv.start_byte());

    // Symbols: kept when their name token came from the invocation.
    let mut kept: HashMap<usize, usize> = HashMap::new(); // sub index -> facts index
    let mut sub_qname: HashMap<usize, String> = HashMap::new();
    for (i, mut s) in sub.symbols.into_iter().enumerate() {
        let Some(o) = map(s.name_byte) else {
            continue;
        };
        let key = (o, s.name.clone());
        if let Some(&g) = pend.defined.get(&key) {
            kept.insert(i, g);
            continue;
        }
        // The definition is its name token at the invocation. Its byte span is the whole
        // invocation: symbols are ordered by span, and an expansion may name a child before
        // its parent (`m!(inner, outer)` -> `mod outer { fn inner() {} }`); sharing one
        // span keeps them in the expansion's own (parent-first) order.
        let end = o + s.name.len();
        (s.start_line, s.start_col) = line_col(starts, o);
        (s.end_line, s.end_col) = line_col(starts, end);
        (s.start_byte, s.end_byte, s.name_byte) = (inv.start_byte(), inv.end_byte(), o);
        let own_q = std::mem::take(&mut s.local_qname);
        if let Some((p, g)) = s.parent.and_then(|p| kept.get(&p).map(|&g| (p, g))) {
            s.parent = Some(g);
            let rest = sub_qname
                .get(&p)
                .and_then(|pq| own_q.strip_prefix(pq.as_str()))
                .map_or_else(|| format!("{SEP}{}", s.name), str::to_string);
            s.local_qname = format!("{}{rest}", facts.symbols[g].local_qname);
        } else {
            s.parent = outer_parent;
            let own = own_q
                .strip_prefix(&format!("{STMT_WRAPPER}{SEP}"))
                .unwrap_or(&own_q);
            s.local_qname = match outer_parent {
                Some(p) => format!("{}{SEP}{own}", facts.symbols[p].local_qname),
                None => own.to_string(),
            };
        }
        sub_qname.insert(i, own_q);
        facts.symbols.push(s);
        let g = facts.symbols.len() - 1;
        kept.insert(i, g);
        pend.defined.insert(key, g);
    }

    // References: kept when their name token came from the invocation; they replace what
    // plain extraction saw at the same place (the expansion knows more, like types).
    for mut r in sub.refs {
        let Some(o) = at(r.line, r.col).and_then(map) else {
            continue;
        };
        (r.line, r.col) = line_col(starts, o);
        if !pend.seen.insert((r.line, r.col, r.name.clone())) {
            continue;
        }
        r.from = match r.from.and_then(|f| kept.get(&f)) {
            Some(&g) => Some(g),
            None => original(facts, o),
        };
        facts.refs.push(r);
    }
}

/// Drop the original references an expansion replaced, then restore source order.
fn finish(facts: &mut FileFacts, pend: &Pending) {
    if pend.seen.is_empty() && facts.symbols.len() == pend.orig_syms {
        return;
    }
    let mut i = 0;
    facts.refs.retain(|r| {
        let original = i < pend.orig_refs;
        i += 1;
        !(original && pend.seen.contains(&(r.line, r.col, r.name.clone())))
    });
    facts.refs.sort_by_key(|r| (r.line, r.col));
    crate::extract::merge_symbols_sorted(facts);
}
#[cfg(test)]
mod tests {
    use crate::extract::{extract, FileFacts};
    use crate::lang::Lang;
    use crate::model::SymKind;

    fn facts(src: &str) -> FileFacts {
        extract(Lang::Rust, src.as_bytes()).unwrap()
    }

    fn sym<'f>(f: &'f FileFacts, name: &str) -> &'f crate::extract::RawSymbol {
        f.symbols
            .iter()
            .find(|s| s.name == name)
            .unwrap_or_else(|| panic!("no symbol {name}"))
    }

    #[test]
    fn recursive_macro_defines_methods_of_the_impl() {
        let f = facts(
            "struct De;\n\
             macro_rules! num {\n\
                 ($m:ident) => { num!($m, visit); };\n\
                 ($m:ident, $v:ident) => { fn $m(&self) -> u8 { 0 } };\n\
             }\n\
             impl De {\n\
                 num!(deserialize_i8);\n\
             }\n",
        );
        let s = sym(&f, "deserialize_i8");
        assert_eq!(s.kind, SymKind::Method);
        assert_eq!(s.container.as_deref(), Some("De"));
        assert_eq!((s.start_line, s.start_col), (7, 6));
        // Names written only in the macro body have no place in this file.
        assert!(f.symbols.iter().all(|s| s.name != "visit"));
    }

    #[test]
    fn repetitions_with_separators_and_trailing_comma() {
        let f = facts(
            "macro_rules! fns {\n\
                 ($($n:ident),* $(,)?) => { $(pub fn $n() {})* };\n\
             }\n\
             mod m {\n\
                 fns!(alpha, beta,);\n\
             }\n",
        );
        assert_eq!(sym(&f, "alpha").local_qname, "m::alpha");
        assert_eq!(sym(&f, "beta").start_line, 5);
    }

    #[test]
    fn fragments_stop_at_their_follow_set() {
        // `ty` keeps generic arguments together; `expr` keeps a closure's parameters.
        let f = facts(
            "macro_rules! pair {\n\
                 ($t:ty, $e:expr, $n:ident) => { fn $n(x: $t) { let f = $e; } };\n\
             }\n\
             pair!(HashMap<u8, Vec<(u8, u8)>>, |a, b| a + b, both);\n",
        );
        assert_eq!(sym(&f, "both").start_line, 4);
    }

    #[test]
    fn unseparated_expressions_end_where_the_next_operand_starts() {
        // serde_json's `number_partialeq_ok!(0 f32::consts::E f32::consts::PI)`: one path
        // per `$n`, each qualified by its own `f32::consts`.
        let f = facts(
            "macro_rules! each {\n\
                 ($($n:expr)*) => { $( show($n); )* };\n\
             }\n\
             fn t() { each!(0 f32::consts::E f32::consts::PI x as u8); }\n",
        );
        let q = |name: &str| {
            f.refs
                .iter()
                .find(|r| r.name == name)
                .map(|r| r.qualifier.clone())
        };
        assert_eq!(q("E"), Some(Some("f32::consts".into())));
        assert_eq!(q("PI"), Some(Some("f32::consts".into())));
        assert_eq!(f.refs.iter().filter(|r| r.name == "show").count(), 0);
    }

    #[test]
    fn calls_in_invocation_tokens_are_typed_by_the_expansion() {
        let f = facts(
            "struct Stream;\n\
             impl Stream { fn next(&mut self) {} }\n\
             fn open() -> Stream { Stream }\n\
             macro_rules! with {\n\
                 (|$s:ident| $body:block) => {{ let mut $s = open(); $body }};\n\
             }\n\
             fn run() {\n\
                 with!(|stream| { stream.next(); });\n\
             }\n",
        );
        let r = f.refs.iter().find(|r| r.name == "next").expect("next call");
        assert_eq!((r.line, r.qualifier.as_deref()), (8, Some("stream")));
        assert_eq!(r.hint.as_deref(), Some("C:open"));
        let from = r.from.map(|i| f.symbols[i].name.as_str());
        assert_eq!(from, Some("run"));
    }

    #[test]
    fn no_matching_arm_expands_nothing() {
        let f = facts(
            "macro_rules! only_ident { ($n:ident) => { fn $n() {} }; }\n\
             only_ident!(1 + 2);\n",
        );
        assert!(f.symbols.iter().all(|s| s.kind != SymKind::Function));
    }

    #[test]
    fn deeply_nested_arguments_are_not_expanded() {
        let deep = format!("{}x{}", "(".repeat(200), ")".repeat(200));
        let src = format!(
            "macro_rules! m {{ ($t:tt, $n:ident) => {{ fn $n() {{}} }}; }}
m!({deep}, shallow_fn);
"
        );
        let f = facts(&src);
        assert!(f.symbols.iter().all(|s| s.name != "shallow_fn"));
    }

    #[test]
    fn a_name_the_transcriber_repeats_is_defined_once() {
        let f = facts(
            "macro_rules! twice { ($n:ident) => { fn $n() {} fn $n() {} }; }
             macro_rules! outer { ($n:ident) => { twice!($n); }; }
             outer!(once);
",
        );
        assert_eq!(f.symbols.iter().filter(|s| s.name == "once").count(), 1);
    }

    #[test]
    fn invocations_inside_one_that_does_not_expand_still_do() {
        let f = facts(
            "macro_rules! def { ($n:ident) => { fn $n() {} }; }
             macro_rules! only_lit { (1) => {}; }
             only_lit!(def!(inner_fn));
",
        );
        assert_eq!(sym(&f, "inner_fn").start_line, 3);
    }

    #[test]
    fn comments_inside_macros_do_not_swallow_the_expansion() {
        let f = facts(
            "struct De;
             macro_rules! num {
                 // one method per call
                 ($m:ident) => {
                     /// Deserialize a number.
                     fn $m(&self) -> u8 { 0 } // the default
                 };
             }
             impl De {
                 num!(/* signed */ deserialize_i8);
             }
",
        );
        assert_eq!(sym(&f, "deserialize_i8").start_line, 10);
    }

    #[test]
    fn a_child_named_before_its_parent_keeps_its_parent() {
        let f = facts(
            "macro_rules! m { ($f:ident, $m:ident) => { pub mod $m { pub fn $f() {} } }; }
             m!(inner_fn, outer_mod);
",
        );
        let inner = sym(&f, "inner_fn");
        let parent = inner.parent.map(|p| f.symbols[p].name.as_str());
        assert_eq!(parent, Some("outer_mod"));
        assert!(inner.parent.is_some_and(|p| {
            f.symbols
                .iter()
                .position(|s| s.name == "inner_fn")
                .is_some_and(|i| p < i)
        }));
    }

    #[test]
    fn expanded_definitions_have_their_own_signature_line() {
        let f = facts(
            "macro_rules! two {
                 ($a:ident, $b:ident) => {
                     fn $a() {}
                     fn $b(x: u8) -> u8 { x }
                 };
             }
             two!(first, second);
",
        );
        assert_eq!(sym(&f, "second").signature, "fn second(x: u8) -> u8 { x }");
    }

    #[test]
    fn literal_fragments_are_literals() {
        assert!(
            super::is_literal("1u8") && super::is_literal("'a'") && super::is_literal("r#\"x\"#")
        );
        assert!(
            !super::is_literal("'a") && !super::is_literal("break") && !super::is_literal("r#type")
        );
    }

    #[test]
    fn self_multiplying_macros_stay_bounded() {
        // Each level invokes the macro eight times: 8^4 without the shared budget.
        let src = "macro_rules! boom {\n\
                       ($n:ident) => { boom!($n); boom!($n); boom!($n); boom!($n);\n\
                                       boom!($n); boom!($n); boom!($n); boom!($n); };\n\
                   }\n\
                   fn f() { boom!(x); boom!(y); }\n";
        let t = std::time::Instant::now();
        let _ = facts(src);
        assert!(t.elapsed() < std::time::Duration::from_secs(5));
    }
}

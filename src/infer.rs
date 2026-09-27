//! Local type evidence for receiver calls, computed at extraction time.
//!
//! For a call like `repo.backend().list()` the resolver needs the *type* of the
//! receiver expression. Tree-sitter gives syntax only, so this module turns the receiver
//! into a small **hint** the resolver can evaluate against the whole repo later:
//!
//! | hint | meaning |
//! |---|---|
//! | `S` | the enclosing type (`self`, `this`, a Go receiver) |
//! | `B` | the bases of the enclosing type (`super`): a call resolves on the first base, in declaration order, that has the method — never on the enclosing type itself; as a value (`M:x@B`) it is the first base that binds |
//! | `T:<type>` | a written type (`*Store`, `Box<dyn Backend>`, `http.Handler`) |
//! | `C:<callee>` | the result of calling a function or constructor (`NewStore`, `Foo::new`); JS/TS may append `#f` (first argument is a function literal) or `#o` (another literal / `new`) to pick among call signatures |
//! | `M:<name>@<hint>` | the result of calling method `name` on `<hint>` |
//! | `F:<name>@<hint>` | field `name` of `<hint>` |
//! | `Q:<hint>` | `<hint>` unwrapped (Rust `?`, `.unwrap()`); `A:<hint>` awaited |
//! | `E:<hint>` | an element of the collection `<hint>` (loop variables, callback params) |
//! | `L` | a local variable or parameter with no known type (never a repo symbol) |
//!
//! Evidence comes from parameters, `let`/`:=`/`const`/assignments, struct and class
//! fields and function return types (JS/TS `Class.prototype` is `T:Class` when `Class` is
//! not a local). Everything is best effort: an unknown expression yields no hint and the
//! resolver falls back to name-based rules.
//!
//! JS/TS values: exported module-level `const`/`let`/`var` symbols carry their type as
//! `ret` (declared, else the initializer's hint), and TS call signatures are fields named
//! `()<shape>` (shape of the first parameter, [`param_shape`]; `0` = none) holding the
//! return type. `C:f` where `f` is such a value (imported, same file or `ns.f`) calls it:
//! a function type `(..) => R` gives `R`; an interface / type alias gives its fitting
//! signatures (own, else alias target's, else bases'), resolved only when they agree.
//! `C:v.m` / `C:ns.v.m` is method `m` of the value's type or its field `m`, called.
//!
//! `T:` names are bound where the hint was written: in JS/TS, a file declaring one type
//! name twice (sibling test callbacks each with `class Store`) binds the declaration
//! closest before the reference.
//!
//! ## Cost
//!
//! In tree-sitter 0.27 `Node::parent`, `prev_sibling` and `next_sibling` walk down from
//! the root (O(depth) each, O(width) for siblings), so calling them per reference makes a
//! long method chain (`a.b().b()…`, every call one level deeper) quadratic or worse.
//! Extraction therefore navigates through [`Nav`]: plain tree-sitter calls while a file
//! is cheap (real code is ~20 levels deep, and building a table for every file cost a
//! third of all extraction time), switching to a table built in one cursor walk —
//! parent / sibling / enclosing-function links in O(1) — once the file has used
//! [`NAV_TABLE_AFTER`] of extraction time. Two guards bound what is left: once the table
//! shows nesting deeper than [`MAX_INFER_DEPTH`] no more receiver hints are computed
//! (symbols and edges are still extracted), and `extract` stops computing hints once a
//! file has used its time budget (see `extract::HINT_BUDGET`). Either sets
//! `FileFacts::limited`.

use std::cell::{Cell, OnceCell};
use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};
use std::time::{Duration, Instant};

use tree_sitter::Node;

use crate::lang::Lang;

/// Deepest hint nesting kept (longer chains are dropped).
const MAX_DEPTH: u8 = 6;
/// Longest hint kept.
const MAX_HINT: usize = 200;
/// Syntax trees nested deeper than this get no receiver-type hints (real code stays far
/// below it; generated or hostile files — thousand-call chains — do not).
pub(crate) const MAX_INFER_DEPTH: usize = 512;

/// Hasher for tree-sitter node ids (pointers: already unique, only need mixing).
#[derive(Default)]
struct IdHasher(u64);

impl Hasher for IdHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0.rotate_left(8) ^ u64::from(b)).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        }
    }
    fn write_usize(&mut self, n: usize) {
        let h = (n as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        self.0 = h ^ (h >> 32);
    }
}

type IdMap<V> = HashMap<usize, V, BuildHasherDefault<IdHasher>>;

const NONE: u32 = u32::MAX;

/// Extraction time after which a file's lookups go through a [`NavTable`] (only
/// pathological or very large files get there).
const NAV_TABLE_AFTER: Duration = Duration::from_millis(100);

/// Tree navigation for one file: tree-sitter's own O(depth) calls until the file has
/// used [`NAV_TABLE_AFTER`], then O(1) lookups in a [`NavTable`] (see the module doc).
pub(crate) struct Nav<'a> {
    lang: Lang,
    root: Node<'a>,
    start: Instant,
    table: OnceCell<NavTable<'a>>,
    calls: Cell<u32>,
}

impl<'a> Nav<'a> {
    /// `start`: when extraction of this file began.
    pub(crate) fn new(lang: Lang, root: Node<'a>, start: Instant) -> Self {
        Self {
            lang,
            root,
            start,
            table: OnceCell::new(),
            calls: Cell::new(0),
        }
    }

    /// The table, once built; builds it when the file has run long (checked every 64
    /// lookups: between checks a lookup costs at most O(depth)).
    fn table(&self) -> Option<&NavTable<'a>> {
        if let Some(t) = self.table.get() {
            return Some(t);
        }
        let c = self.calls.get().wrapping_add(1);
        self.calls.set(c);
        if c % 64 == 0 && self.start.elapsed() > NAV_TABLE_AFTER {
            return Some(
                self.table
                    .get_or_init(|| NavTable::new(self.lang, self.root)),
            );
        }
        None
    }

    /// `n.parent()`.
    pub(crate) fn parent(&self, n: Node<'a>) -> Option<Node<'a>> {
        match self.table() {
            Some(t) => t.link(n, &t.parent).unwrap_or_else(|()| n.parent()),
            None => n.parent(),
        }
    }

    /// `n.prev_sibling()`.
    pub(crate) fn prev_sibling(&self, n: Node<'a>) -> Option<Node<'a>> {
        match self.table() {
            Some(t) => t.link(n, &t.prev).unwrap_or_else(|()| n.prev_sibling()),
            None => n.prev_sibling(),
        }
    }

    /// `n.next_sibling()`.
    pub(crate) fn next_sibling(&self, n: Node<'a>) -> Option<Node<'a>> {
        match self.table() {
            Some(t) => t.link(n, &t.next).unwrap_or_else(|()| n.next_sibling()),
            None => n.next_sibling(),
        }
    }

    /// Nearest function-like strict ancestor of `n`.
    fn enclosing_fn(&self, lang: Lang, n: Node<'a>) -> Option<Node<'a>> {
        if let Some(t) = self.table() {
            if let Ok(f) = t.link(n, &t.func) {
                return f;
            }
        }
        let mut cur = n.parent();
        while let Some(p) = cur {
            if is_function(lang, p) {
                return Some(p);
            }
            cur = p.parent();
        }
        None
    }

    /// Deepest nesting level in the tree (root = 0), once the table was built.
    pub(crate) fn max_depth(&self) -> Option<usize> {
        self.table.get().map(|t| t.max_depth)
    }
}

/// O(1) tree navigation: parent, siblings and the nearest enclosing function of every
/// node, recorded in a single cursor walk.
struct NavTable<'a> {
    nodes: Vec<Node<'a>>,
    index: IdMap<u32>,
    parent: Vec<u32>,
    prev: Vec<u32>,
    next: Vec<u32>,
    /// Nearest strict ancestor that is function-like for the language.
    func: Vec<u32>,
    max_depth: usize,
}

impl<'a> NavTable<'a> {
    fn new(lang: Lang, root: Node<'a>) -> Self {
        let mut nav = NavTable {
            nodes: Vec::new(),
            index: IdMap::default(),
            parent: Vec::new(),
            prev: Vec::new(),
            next: Vec::new(),
            func: Vec::new(),
            max_depth: 0,
        };
        // Ancestors of the cursor's node: (index, enclosing function for its children,
        // last child recorded so far).
        let mut stack: Vec<(u32, u32, u32)> = Vec::new();
        let mut cur = root.walk();
        loop {
            let n = cur.node();
            let i = nav.nodes.len() as u32;
            let (p, f, pr) = stack.last().map_or((NONE, NONE, NONE), |e| *e);
            nav.nodes.push(n);
            nav.index.insert(n.id(), i);
            nav.parent.push(p);
            nav.func.push(f);
            nav.prev.push(pr);
            nav.next.push(NONE);
            if pr != NONE {
                nav.next[pr as usize] = i;
            }
            if let Some(e) = stack.last_mut() {
                e.2 = i;
            }
            nav.max_depth = nav.max_depth.max(stack.len());
            if cur.goto_first_child() {
                let fc = if is_function(lang, n) { i } else { f };
                stack.push((i, fc, NONE));
                continue;
            }
            loop {
                if cur.goto_next_sibling() {
                    break;
                }
                if !cur.goto_parent() {
                    return nav;
                }
                stack.pop();
            }
        }
    }

    /// `links[n]` for a node recorded in the walk: `Ok(None)` when it has no such
    /// neighbour, `Err(())` when `n` is not from this tree (callers fall back to the
    /// O(depth) tree-sitter call).
    fn link(&self, n: Node<'_>, links: &[u32]) -> Result<Option<Node<'a>>, ()> {
        let &i = self.index.get(&n.id()).ok_or(())?;
        let j = links[i as usize];
        Ok((j != NONE).then(|| self.nodes[j as usize]))
    }
}

/// Function-like nodes whose parameters and body form a lookup environment.
fn is_function(lang: Lang, n: Node<'_>) -> bool {
    let k = n.kind();
    match lang {
        Lang::Go => matches!(
            k,
            "function_declaration" | "method_declaration" | "func_literal"
        ),
        Lang::Rust => matches!(k, "function_item" | "closure_expression"),
        Lang::TypeScript | Lang::Tsx | Lang::JavaScript => matches!(
            k,
            "function_declaration"
                | "function_expression"
                | "function"
                | "arrow_function"
                | "method_definition"
                | "generator_function_declaration"
                | "generator_function"
        ),
        Lang::Python => matches!(k, "function_definition" | "lambda"),
        Lang::C | Lang::Cpp => false,
    }
}

#[derive(Clone)]
struct Decl<'a> {
    name: String,
    /// Byte offset from which the binding is in effect.
    at: usize,
    /// Written type (or `S` for a receiver).
    hint: Option<String>,
    /// Initializer, evaluated lazily at lookup (building an environment must not
    /// evaluate expressions: they look names up in that same environment).
    value: Option<Node<'a>>,
    /// The binding is an element of `value` (`for x in xs`, `xs.map(|x| ..)`).
    elem: bool,
    /// The binding holds a function (closure, arrow function, nested def).
    func: bool,
}

/// Per-file inference state (function environments are built lazily).
pub(crate) struct Infer<'a> {
    lang: Lang,
    src: &'a [u8],
    envs: HashMap<usize, Vec<Decl<'a>>>,
    globals: Option<Vec<Decl<'a>>>,
    /// Evaluated initializers: (value node id, element-of) -> hint.
    memo: HashMap<(usize, bool), Option<String>>,
    root: Node<'a>,
    nav: &'a Nav<'a>,
    /// Receiver hints are computed (off for over-deep files or past the time budget).
    hints: bool,
}

fn text<'a>(n: Node<'_>, src: &'a [u8]) -> &'a str {
    n.utf8_text(src).unwrap_or("")
}

/// Type text with whitespace collapsed: one space between two word tokens
/// (`dyn Backend`, `&'a Repo`), none elsewhere (`Box<dyn Backend>`).
fn squash(s: &str) -> String {
    let word = |c: char| c.is_alphanumeric() || c == '_' || c == '\'';
    let mut out = String::with_capacity(s.len());
    let mut pending = false;
    for c in s.chars() {
        if c.is_whitespace() {
            pending = true;
            continue;
        }
        if pending && out.chars().last().is_some_and(word) && word(c) {
            out.push(' ');
        }
        pending = false;
        out.push(c);
    }
    out
}

fn mk(prefix: &str, inner: &str) -> Option<String> {
    let s = format!("{prefix}{inner}");
    (s.len() <= MAX_HINT).then_some(s)
}

impl<'a> Infer<'a> {
    pub(crate) fn new(lang: Lang, root: Node<'a>, src: &'a [u8], nav: &'a Nav<'a>) -> Self {
        Self {
            lang,
            src,
            envs: HashMap::new(),
            globals: None,
            memo: HashMap::new(),
            root,
            nav,
            hints: true,
        }
    }

    /// Whether receiver hints are being computed: not past the time budget, and not in a
    /// tree known to be nested deeper than [`MAX_INFER_DEPTH`].
    pub(crate) fn hints_enabled(&self) -> bool {
        self.hints && self.nav.max_depth().is_none_or(|d| d <= MAX_INFER_DEPTH)
    }

    /// Stop computing receiver hints for the rest of the file.
    pub(crate) fn disable_hints(&mut self) {
        self.hints = false;
    }

    fn t(&self, n: Node<'_>) -> &'a str {
        text(n, self.src)
    }

    fn is_function(&self, n: Node<'_>) -> bool {
        is_function(self.lang, n)
    }

    /// Receiver hint for a qualifier expression (`x` in `x.f()`).
    pub(crate) fn qualifier_hint(&mut self, q: Node<'a>) -> Option<String> {
        if !self.supported() || !self.hints_enabled() {
            return None;
        }
        self.eval(q, 0)
    }

    /// Receiver hint for a `.name(` call inside a macro token tree, where `q` is the token
    /// before the `.`: rebuild the chain from raw tokens (`repo.backend().exists(..)`,
    /// `k.id.short()`).
    pub(crate) fn token_receiver_hint(&mut self, q: Node<'a>) -> Option<String> {
        if !self.supported() || !self.hints_enabled() {
            return None;
        }
        self.token_chain(q, 0)
    }

    fn token_chain(&mut self, q: Node<'a>, depth: u8) -> Option<String> {
        if depth > MAX_DEPTH {
            return None;
        }
        let nav = self.nav;
        let prev = nav.prev_sibling(q);
        match q.kind() {
            "self" => Some("S".into()),
            "identifier" => {
                if prev.is_some_and(|p| p.kind() == ".") {
                    let base = self.token_chain(nav.prev_sibling(prev?)?, depth + 1)?;
                    return mk(&format!("F:{}@", self.t(q)), &base);
                }
                if prev.is_some_and(|p| p.kind() == "::") {
                    return None;
                }
                self.eval(q, depth + 1).filter(|h| h != "L")
            }
            "token_tree" if self.t(q).starts_with('(') => {
                // `<recv>.m(args)` / `path::f(args)` / `f(args)`
                let m = prev.filter(|p| p.kind() == "identifier")?;
                let before = nav.prev_sibling(m);
                match before.map(|b| b.kind()) {
                    Some(".") => {
                        let base = self.token_chain(nav.prev_sibling(before?)?, depth + 1)?;
                        mk(&format!("M:{}@", self.t(m)), &base)
                    }
                    Some("::") => {
                        let mut parts = vec![self.t(m).to_string()];
                        let mut cur = before;
                        while let Some(sep) = cur.filter(|c| c.kind() == "::") {
                            let seg = nav.prev_sibling(sep).filter(|s| s.kind() == "identifier")?;
                            parts.push(self.t(seg).to_string());
                            cur = nav.prev_sibling(seg);
                        }
                        parts.reverse();
                        mk("C:", &parts.join("::"))
                    }
                    _ => mk("C:", self.t(m)),
                }
            }
            _ => None,
        }
    }

    /// Whether a bare identifier used as a call/value is a local variable or parameter
    /// (not a function): such a name never refers to a repo-level symbol.
    pub(crate) fn bare_is_local(&mut self, name: Node<'a>) -> bool {
        if !self.supported() || !self.hints_enabled() {
            return false;
        }
        let n = self.t(name).to_string();
        matches!(self.lookup_local(&n, name), Some(d) if !d.func)
    }

    fn supported(&self) -> bool {
        !self.lang.is_c_family()
    }

    // ---- environments --------------------------------------------------------

    fn lookup_local(&mut self, name: &str, at: Node<'a>) -> Option<Decl<'a>> {
        let pos = at.start_byte();
        let nav = self.nav;
        let mut cur = nav.enclosing_fn(self.lang, at);
        while let Some(n) = cur {
            let env = self.env(n);
            if let Some(d) = env
                .iter()
                .filter(|d| d.name == name && d.at <= pos)
                .max_by_key(|d| d.at)
                .cloned()
            {
                return Some(d);
            }
            cur = nav.enclosing_fn(self.lang, n);
        }
        None
    }

    fn lookup(&mut self, name: &str, at: Node<'a>) -> Option<Decl<'a>> {
        if let Some(d) = self.lookup_local(name, at) {
            return Some(d);
        }
        if self.globals.is_none() {
            let mut out = Vec::new();
            let root = self.root;
            self.collect(root, &mut out, true);
            self.globals = Some(out);
        }
        self.globals.as_ref().and_then(|g| {
            g.iter()
                .filter(|d| d.name == name)
                .max_by_key(|d| d.at)
                .cloned()
        })
    }

    fn env(&mut self, f: Node<'a>) -> &Vec<Decl<'a>> {
        let id = f.id();
        if !self.envs.contains_key(&id) {
            let mut out = Vec::new();
            self.params(f, &mut out);
            let body = f
                .child_by_field_name("body")
                .or_else(|| f.child_by_field_name("value"));
            if let Some(b) = body {
                self.collect(b, &mut out, false);
            }
            self.envs.insert(id, out);
        }
        &self.envs[&id]
    }

    fn decl(out: &mut Vec<Decl<'a>>, name: &str, at: usize, hint: Option<String>, func: bool) {
        Self::decl_v(out, name, at, hint, None, func);
    }

    fn decl_v(
        out: &mut Vec<Decl<'a>>,
        name: &str,
        at: usize,
        hint: Option<String>,
        value: Option<Node<'a>>,
        func: bool,
    ) {
        if name.is_empty() || name == "_" {
            return;
        }
        out.push(Decl {
            name: name.to_string(),
            at,
            hint,
            value,
            elem: false,
            func,
        });
    }

    /// A binding that is an element of the collection `coll`.
    fn decl_elem(out: &mut Vec<Decl<'a>>, name: &str, at: usize, coll: Node<'a>) {
        if name.is_empty() || name == "_" {
            return;
        }
        out.push(Decl {
            name: name.to_string(),
            at,
            hint: None,
            value: Some(coll),
            elem: true,
            func: false,
        });
    }

    /// Collection whose elements a callback's first parameter receives:
    /// `xs.map(|x| ..)`, `xs.forEach(x => ..)`, `opt.and_then(|x| ..)`.
    fn callback_source(&self, f: Node<'a>) -> Option<Node<'a>> {
        let args = self.nav.parent(f).filter(|p| p.kind() == "arguments")?;
        let call = self
            .nav
            .parent(args)
            .filter(|p| p.kind() == "call_expression")?;
        let func = call.child_by_field_name("function")?;
        let (obj, field) = Self::member_parts(func)?;
        let m = self.t(field);
        ITER_CALLBACKS.contains(&m).then_some(obj)
    }

    /// A binding's type hint: its written type, else its initializer's.
    fn decl_hint(&mut self, d: &Decl<'a>, depth: u8) -> Option<String> {
        if d.hint.is_some() {
            return d.hint.clone();
        }
        let v = d.value?;
        let key = (v.id(), d.elem);
        if let Some(h) = self.memo.get(&key) {
            return h.clone();
        }
        // Guard against self-reference while evaluating (`x = x.next()` chains).
        self.memo.insert(key, None);
        let h = self.eval(v, depth + 1).and_then(|h| {
            if !d.elem {
                Some(h)
            } else if h == "L" {
                None
            } else {
                mk("E:", &h)
            }
        });
        self.memo.insert(key, h.clone());
        h
    }

    fn params(&mut self, f: Node<'a>, out: &mut Vec<Decl<'a>>) {
        let at = f.start_byte();
        let gens = self.generics(f);
        match self.lang {
            Lang::Go => {
                if let Some(r) = f.child_by_field_name("receiver") {
                    let mut c = r.walk();
                    for p in r.named_children(&mut c) {
                        let mut c2 = p.walk();
                        for n in p.children_by_field_name("name", &mut c2) {
                            Self::decl(out, self.t(n), at, Some("S".into()), false);
                        }
                    }
                }
                if let Some(ps) = f.child_by_field_name("parameters") {
                    let mut c = ps.walk();
                    for p in ps.named_children(&mut c) {
                        let ty = p
                            .child_by_field_name("type")
                            .and_then(|t| subst(&squash(self.t(t)), &gens))
                            .and_then(|t| mk("T:", &t));
                        let mut c2 = p.walk();
                        for n in p.children_by_field_name("name", &mut c2) {
                            Self::decl(out, self.t(n), at, ty.clone(), false);
                        }
                    }
                }
            }
            Lang::Rust => {
                let bounds = &gens;
                let Some(ps) = f.child_by_field_name("parameters") else {
                    return;
                };
                let mut c = ps.walk();
                for p in ps.named_children(&mut c) {
                    match p.kind() {
                        "self_parameter" => {}
                        "parameter" => {
                            let ty = p
                                .child_by_field_name("type")
                                .and_then(|t| subst(&squash(self.t(t)), bounds))
                                .and_then(|t| mk("T:", &t));
                            if let Some(pat) = p.child_by_field_name("pattern") {
                                for n in pattern_names(pat) {
                                    Self::decl(out, self.t(n), at, ty.clone(), false);
                                }
                            }
                        }
                        "identifier" | "mut_pattern" | "ref_pattern" => {
                            let names = pattern_names(p);
                            match self.callback_source(f) {
                                Some(src) if first_param(ps, p) && names.len() == 1 => {
                                    Self::decl_elem(out, self.t(names[0]), at, src);
                                }
                                _ => {
                                    for n in names {
                                        Self::decl(out, self.t(n), at, None, false);
                                    }
                                }
                            }
                        }
                        // Tuple / struct patterns (`|(repo, _)|`): bound, type unknown, and
                        // they shadow any outer variable of the same name.
                        _ => {
                            for n in pattern_names(p) {
                                Self::decl(out, self.t(n), at, None, false);
                            }
                        }
                    }
                }
            }
            Lang::TypeScript | Lang::Tsx | Lang::JavaScript => {
                let src = self.callback_source(f);
                if let Some(p) = f.child_by_field_name("parameter") {
                    match src {
                        Some(s) => Self::decl_elem(out, self.t(p), at, s),
                        None => Self::decl(out, self.t(p), at, None, false),
                    }
                }
                if let Some(ps) = f.child_by_field_name("parameters") {
                    let mut c = ps.walk();
                    for p in ps.named_children(&mut c) {
                        let ty = p
                            .child_by_field_name("type")
                            .and_then(|t| t.named_child(0))
                            .and_then(|t| subst(&squash(self.t(t)), &gens))
                            .and_then(|t| mk("T:", &t));
                        let pat = p.child_by_field_name("pattern").unwrap_or(p);
                        if let (Some(s), None, "identifier") = (src, &ty, pat.kind()) {
                            if first_param(ps, p) {
                                Self::decl_elem(out, self.t(pat), at, s);
                                continue;
                            }
                        }
                        if pat.kind() == "this" {
                            // `function f(this: Annotation, ..)`: a named type only (an
                            // inline `this: { .. }` cannot be looked up).
                            let named = p
                                .child_by_field_name("type")
                                .and_then(|t| t.named_child(0))
                                .is_some_and(|t| {
                                    matches!(
                                        t.kind(),
                                        "type_identifier"
                                            | "generic_type"
                                            | "nested_type_identifier"
                                    )
                                });
                            Self::decl(out, "this", at, if named { ty } else { None }, false);
                            continue;
                        }
                        for n in pattern_names(pat) {
                            Self::decl(out, self.t(n), at, ty.clone(), false);
                        }
                    }
                }
            }
            Lang::Python => {
                let Some(ps) = f.child_by_field_name("parameters") else {
                    return;
                };
                let mut c = ps.walk();
                for (i, p) in ps.named_children(&mut c).enumerate() {
                    match p.kind() {
                        "identifier" => {
                            let n = self.t(p);
                            let h = (i == 0 && matches!(n, "self" | "cls")).then(|| "S".into());
                            Self::decl(out, n, at, h, false);
                        }
                        "typed_parameter" => {
                            let ty = p
                                .child_by_field_name("type")
                                .and_then(|t| mk("T:", &squash(self.t(t))));
                            if let Some(n) = p.named_child(0) {
                                Self::decl(out, self.t(n), at, ty, false);
                            }
                        }
                        "default_parameter" | "typed_default_parameter" => {
                            let ty = p
                                .child_by_field_name("type")
                                .and_then(|t| mk("T:", &squash(self.t(t))));
                            if let Some(n) = p.child_by_field_name("name") {
                                Self::decl(out, self.t(n), at, ty, false);
                            }
                        }
                        "list_splat_pattern" | "dictionary_splat_pattern" => {
                            if let Some(n) = p.named_child(0) {
                                Self::decl(out, self.t(n), at, None, false);
                            }
                        }
                        _ => {}
                    }
                }
            }
            Lang::C | Lang::Cpp => {}
        }
    }

    /// Collect declarations under `node`, not descending into nested functions
    /// (whose names are recorded as function bindings).
    fn collect(&mut self, node: Node<'a>, out: &mut Vec<Decl<'a>>, top: bool) {
        // One cursor for the whole walk (a cursor per node dominated the cost).
        let mut cur = node.walk();
        loop {
            let n = cur.node();
            let mut descend = true;
            if n.id() != node.id() {
                if !n.is_named() {
                    descend = false;
                } else if self.is_function(n) {
                    if let Some(name) = n.child_by_field_name("name") {
                        Self::decl(out, self.t(name), n.start_byte(), None, true);
                    }
                    descend = false;
                } else if top && Self::is_block(n) {
                    descend = false;
                }
            }
            if descend {
                self.decl_of(n, out);
                if cur.goto_first_child() {
                    continue;
                }
            }
            loop {
                if cur.node().id() == node.id() {
                    return;
                }
                if cur.goto_next_sibling() {
                    break;
                }
                if !cur.goto_parent() {
                    return;
                }
            }
        }
    }

    /// Block-like nodes skipped when collecting file-level (global) declarations.
    fn is_block(n: Node<'_>) -> bool {
        matches!(
            n.kind(),
            "block" | "statement_block" | "class_body" | "declaration_list" | "impl_item"
        )
    }

    fn is_func_value(v: Node<'_>) -> bool {
        matches!(
            v.kind(),
            "func_literal"
                | "closure_expression"
                | "arrow_function"
                | "function_expression"
                | "function"
                | "lambda"
        )
    }

    fn decl_of(&mut self, n: Node<'a>, out: &mut Vec<Decl<'a>>) {
        let at = n.end_byte();
        match (self.lang, n.kind()) {
            (Lang::Go, "short_var_declaration") => {
                let (Some(l), Some(r)) = (
                    n.child_by_field_name("left"),
                    n.child_by_field_name("right"),
                ) else {
                    return;
                };
                let ls = named(l);
                let rs = named(r);
                for (i, ln) in ls.iter().enumerate() {
                    let (value, func) = if ls.len() == rs.len() {
                        (Some(rs[i]), Self::is_func_value(rs[i]))
                    } else if i == 0 && rs.len() == 1 {
                        (Some(rs[0]), false)
                    } else {
                        (None, false)
                    };
                    Self::decl_v(out, self.t(*ln), at, None, value, func);
                }
            }
            (Lang::Go, "var_spec") => {
                let ty = n
                    .child_by_field_name("type")
                    .and_then(|t| mk("T:", &squash(self.t(t))));
                let vals: Vec<Node<'_>> = n
                    .child_by_field_name("value")
                    .map(|v| named(v))
                    .unwrap_or_default();
                let mut c = n.walk();
                let names: Vec<Node<'_>> = n.children_by_field_name("name", &mut c).collect();
                for (i, nm) in names.iter().enumerate() {
                    let func = vals.get(i).is_some_and(|v| Self::is_func_value(*v));
                    Self::decl_v(out, self.t(*nm), at, ty.clone(), vals.get(i).copied(), func);
                }
            }
            (Lang::Go, "range_clause") => {
                if let Some(l) = n.child_by_field_name("left") {
                    let vars = named(l);
                    let right = n.child_by_field_name("right");
                    for (i, x) in vars.iter().enumerate() {
                        match right {
                            // `for _, v := range xs`: v is an element.
                            Some(r) if i == 1 => {
                                Self::decl_elem(out, self.t(*x), r.end_byte(), r);
                            }
                            _ => Self::decl(out, self.t(*x), n.start_byte(), None, false),
                        }
                    }
                }
            }
            (Lang::Go, "type_switch_statement") => {
                if let Some(a) = n.child_by_field_name("alias") {
                    for x in named(a) {
                        Self::decl(out, self.t(x), n.start_byte(), None, false);
                    }
                }
            }
            (Lang::Rust, "let_declaration") => {
                let Some(pat) = n.child_by_field_name("pattern") else {
                    return;
                };
                let names = pattern_names(pat);
                let simple = matches!(pat.kind(), "identifier" | "mut_pattern");
                let (hint, value) = if simple {
                    (
                        n.child_by_field_name("type")
                            .and_then(|t| mk("T:", &squash(self.t(t)))),
                        n.child_by_field_name("value"),
                    )
                } else {
                    (None, None)
                };
                let func = n
                    .child_by_field_name("value")
                    .is_some_and(|v| Self::is_func_value(v));
                for x in names {
                    Self::decl_v(out, self.t(x), at, hint.clone(), value, func);
                }
            }
            (Lang::Rust, "for_expression") => {
                let (Some(p), Some(v)) = (
                    n.child_by_field_name("pattern"),
                    n.child_by_field_name("value"),
                ) else {
                    return;
                };
                if matches!(p.kind(), "identifier" | "mut_pattern") {
                    for x in pattern_names(p) {
                        Self::decl_elem(out, self.t(x), v.end_byte(), v);
                    }
                } else {
                    for x in pattern_names(p) {
                        Self::decl(out, self.t(x), n.start_byte(), None, false);
                    }
                }
            }
            (Lang::Rust, "let_condition") => {
                let (Some(p), v) = (
                    n.child_by_field_name("pattern"),
                    n.child_by_field_name("value"),
                ) else {
                    return;
                };
                // `if let Some(x) = opt` / `Ok(x) = res`: x is the payload.
                let inner = (p.kind() == "tuple_struct_pattern")
                    .then(|| p.named_child(0))
                    .flatten()
                    .filter(|t| matches!(self.t(*t), "Some" | "Ok"))
                    .and_then(|_| p.named_child(1))
                    .map(|x| {
                        if matches!(x.kind(), "ref_pattern" | "mut_pattern") {
                            x.named_child(x.named_child_count().saturating_sub(1) as u32)
                                .unwrap_or(x)
                        } else {
                            x
                        }
                    })
                    .filter(|x| x.kind() == "identifier" && p.named_child_count() == 2);
                match (inner, v) {
                    (Some(x), Some(v)) => Self::decl_elem(out, self.t(x), v.end_byte(), v),
                    _ => {
                        for x in pattern_names(p) {
                            Self::decl(out, self.t(x), n.start_byte(), None, false);
                        }
                    }
                }
            }
            (Lang::Rust, "match_arm") => {
                if let Some(p) = n.child_by_field_name("pattern") {
                    for x in pattern_names(p) {
                        Self::decl(out, self.t(x), n.start_byte(), None, false);
                    }
                }
            }
            (Lang::TypeScript | Lang::Tsx | Lang::JavaScript, "variable_declarator") => {
                let Some(name) = n.child_by_field_name("name") else {
                    return;
                };
                let value = n.child_by_field_name("value");
                let simple = name.kind() == "identifier";
                let hint = if simple {
                    n.child_by_field_name("type")
                        .and_then(|t| t.named_child(0))
                        .and_then(|t| mk("T:", &squash(self.t(t))))
                } else {
                    None
                };
                let func = value.is_some_and(|v| Self::is_func_value(v));
                let value = if simple { value } else { None };
                for x in pattern_names(name) {
                    Self::decl_v(out, self.t(x), at, hint.clone(), value, func);
                }
            }
            (Lang::TypeScript | Lang::Tsx | Lang::JavaScript, "for_in_statement")
                if has_kind(n, "of") =>
            {
                let (Some(l), Some(r)) = (
                    n.child_by_field_name("left"),
                    n.child_by_field_name("right"),
                ) else {
                    return;
                };
                if l.kind() == "identifier" {
                    Self::decl_elem(out, self.t(l), r.end_byte(), r);
                } else {
                    for x in pattern_names(l) {
                        Self::decl(out, self.t(x), n.start_byte(), None, false);
                    }
                }
            }
            (
                Lang::TypeScript | Lang::Tsx | Lang::JavaScript,
                "for_in_statement" | "catch_clause",
            ) => {
                let pat = n
                    .child_by_field_name("left")
                    .or_else(|| n.child_by_field_name("parameter"));
                if let Some(p) = pat {
                    for x in pattern_names(p) {
                        Self::decl(out, self.t(x), n.start_byte(), None, false);
                    }
                }
            }
            (Lang::Python, "assignment") => {
                let Some(l) = n.child_by_field_name("left") else {
                    return;
                };
                if l.kind() == "identifier" {
                    let right = n.child_by_field_name("right");
                    let hint = n
                        .child_by_field_name("type")
                        .and_then(|t| mk("T:", &squash(self.t(t))));
                    let func = right.is_some_and(|v| Self::is_func_value(v));
                    Self::decl_v(out, self.t(l), at, hint, right, func);
                } else if matches!(l.kind(), "pattern_list" | "tuple_pattern" | "list_pattern") {
                    for x in pattern_names(l) {
                        Self::decl(out, self.t(x), at, None, false);
                    }
                }
            }
            (Lang::Python, "for_statement" | "for_in_clause")
                if n.child_by_field_name("left")
                    .is_some_and(|l| l.kind() == "identifier") =>
            {
                if let (Some(l), Some(r)) = (
                    n.child_by_field_name("left"),
                    n.child_by_field_name("right"),
                ) {
                    Self::decl_elem(out, self.t(l), r.end_byte(), r);
                }
            }
            (Lang::Python, "for_statement" | "with_item" | "for_in_clause" | "except_clause") => {
                let pat = n
                    .child_by_field_name("left")
                    .or_else(|| n.child_by_field_name("alias"))
                    .or_else(|| {
                        (n.kind() == "with_item")
                            .then(|| n.child_by_field_name("value"))
                            .flatten()
                            .and_then(|v| v.child_by_field_name("alias"))
                    });
                if let Some(p) = pat {
                    for x in pattern_names(p) {
                        Self::decl(out, self.t(x), n.start_byte(), None, false);
                    }
                }
            }
            (Lang::Python, "as_pattern") => {
                if let Some(a) = n.child_by_field_name("alias") {
                    for x in pattern_names(a) {
                        Self::decl(out, self.t(x), n.start_byte(), None, false);
                    }
                }
            }
            _ => {}
        }
    }

    /// Generic type parameters in scope of function `f` (and its Rust `impl`, Go generic
    /// receiver): name -> its single meaningful bound (`B: Backend`, `T Getter`,
    /// `T extends Base`), or `None` when unbounded or ambiguous (`T: Clone + Debug`), in
    /// which case a parameter of that type gets no hint rather than a wrong one.
    fn generics(&self, f: Node<'a>) -> Vec<(String, Option<String>)> {
        let mut acc: Vec<(String, Vec<String>)> = Vec::new();
        let mut add = |name: &str, bound: Option<String>| {
            let slot = if let Some(i) = acc.iter().position(|(n, _)| n == name) {
                i
            } else {
                acc.push((name.to_string(), Vec::new()));
                acc.len() - 1
            };
            if let Some(b) = bound {
                acc[slot].1.push(b);
            }
        };
        match self.lang {
            Lang::Rust => {
                let mut scopes = vec![f];
                if let Some(imp) = self
                    .nav
                    .parent(f)
                    .and_then(|p| self.nav.parent(p))
                    .filter(|p| p.kind() == "impl_item")
                {
                    scopes.push(imp);
                }
                for sc in scopes {
                    if let Some(tp) = sc.child_by_field_name("type_parameters") {
                        for p in named(tp) {
                            if p.kind() != "type_parameter" {
                                continue;
                            }
                            let Some(n) = p.child_by_field_name("name") else {
                                continue;
                            };
                            add(self.t(n), None);
                            if let Some(b) = p.child_by_field_name("bounds") {
                                for bb in self.rust_bound_list(b) {
                                    add(self.t(n), Some(bb));
                                }
                            }
                        }
                    }
                    for ch in named(sc) {
                        if ch.kind() != "where_clause" {
                            continue;
                        }
                        for wp in named(ch) {
                            let (Some(l), Some(b)) = (
                                wp.child_by_field_name("left"),
                                wp.child_by_field_name("bounds"),
                            ) else {
                                continue;
                            };
                            for bb in self.rust_bound_list(b) {
                                add(self.t(l), Some(bb));
                            }
                        }
                    }
                }
            }
            Lang::Go => {
                if let Some(tp) = f.child_by_field_name("type_parameters") {
                    for d in named(tp) {
                        let bound = d
                            .child_by_field_name("type")
                            .map(|t| squash(self.t(t)))
                            .filter(|t| {
                                simple_bound(t) && !matches!(t.as_str(), "any" | "comparable")
                            });
                        let mut c = d.walk();
                        let names: Vec<Node<'_>> =
                            d.children_by_field_name("name", &mut c).collect();
                        for n in names {
                            add(self.t(n), bound.clone());
                        }
                    }
                }
                // `func (s *Set[T]) Add(v T)`: T is the receiver type's parameter.
                if let Some(r) = f.child_by_field_name("receiver") {
                    let text = squash(self.t(r));
                    if let (Some(a), Some(b)) = (text.find('['), text.rfind(']')) {
                        for n in text[a + 1..b].split(',') {
                            add(n.trim(), None);
                        }
                    }
                }
            }
            Lang::TypeScript | Lang::Tsx | Lang::JavaScript => {
                if let Some(tp) = f.child_by_field_name("type_parameters") {
                    for p in named(tp) {
                        let Some(n) = p.child_by_field_name("name") else {
                            continue;
                        };
                        let bound = p
                            .child_by_field_name("constraint")
                            .and_then(|c| c.named_child(0))
                            .map(|t| squash(self.t(t)))
                            .filter(|t| simple_bound(t));
                        add(self.t(n), bound);
                    }
                }
            }
            _ => {}
        }
        acc.into_iter()
            .map(|(n, mut b)| {
                b.sort();
                b.dedup();
                (n, (b.len() == 1).then(|| b.remove(0)))
            })
            .collect()
    }

    /// Meaningful Rust trait bounds: no lifetimes, `?Sized` or std marker traits.
    fn rust_bound_list(&self, b: Node<'_>) -> Vec<String> {
        named(b)
            .into_iter()
            .filter(|x| !matches!(x.kind(), "lifetime" | "removed_trait_bound"))
            .map(|x| squash(self.t(x)))
            .filter(|t| {
                let head = t.split('<').next().unwrap_or(t);
                let head = head.rsplit("::").next().unwrap_or(head);
                !t.starts_with('?')
                    && !t.starts_with('\'')
                    && !matches!(
                        head,
                        "Send"
                            | "Sync"
                            | "Clone"
                            | "Copy"
                            | "Debug"
                            | "Default"
                            | "Sized"
                            | "Unpin"
                            | "Display"
                            | "Eq"
                            | "PartialEq"
                            | "Hash"
                            | "Ord"
                            | "PartialOrd"
                            | "Serialize"
                            | "Deserialize"
                    )
            })
            .collect()
    }

    // ---- expressions ---------------------------------------------------------

    fn eval(&mut self, e: Node<'a>, depth: u8) -> Option<String> {
        if depth > MAX_DEPTH {
            return None;
        }
        let d = depth + 1;
        let k = e.kind();
        match k {
            // TS `function f(this: Annotation, ..)`: the declared `this` type wins.
            "this" if self.lang.is_js_family() => match self.lookup_local("this", e) {
                Some(dcl) if dcl.hint.is_some() => dcl.hint,
                _ => Some("S".into()),
            },
            "self" | "this" => Some("S".into()),
            // `super.m()`: the enclosing class's bases.
            "super" if self.lang.is_js_family() => Some("B".into()),
            "identifier" => {
                let name = self.t(e).to_string();
                if self.lang == Lang::Python && matches!(name.as_str(), "self" | "cls") {
                    if let Some(dcl) = self.lookup_local(&name, e) {
                        return self.decl_hint(&dcl, depth);
                    }
                }
                match self.lookup(&name, e) {
                    Some(dcl) if dcl.func => None,
                    Some(dcl) => Some(self.decl_hint(&dcl, depth).unwrap_or_else(|| "L".into())),
                    None => None,
                }
            }
            "parenthesized_expression"
            | "non_null_expression"
            | "reference_expression"
            | "expression_statement" => e.named_child(0).and_then(|c| self.eval(c, d)),
            "unary_expression" => {
                let inner = e
                    .child_by_field_name("operand")
                    .or_else(|| e.named_child(0))?;
                self.eval(inner, d)
            }
            "try_expression" => {
                let inner = e.named_child(0)?;
                let h = self.eval(inner, d)?;
                mk("Q:", &h)
            }
            "await_expression" => {
                let inner = e.named_child(0)?;
                let h = self.eval(inner, d)?;
                if self.lang == Lang::Rust {
                    Some(h)
                } else {
                    mk("A:", &h)
                }
            }
            "field_expression" | "selector_expression" | "member_expression" | "attribute" => {
                let (obj, field) = Self::member_parts(e)?;
                // `Parent.prototype.m()`: an instance of class `Parent` (not of a local or
                // an ES5 constructor function).
                if self.lang.is_js_family()
                    && self.t(field) == "prototype"
                    && obj.kind() == "identifier"
                {
                    let name = self.t(obj);
                    return match self.lookup(name, obj) {
                        Some(_) => None,
                        None => mk("T:", name),
                    };
                }
                let base = self.eval(obj, d)?;
                if base == "L" {
                    return None;
                }
                mk(&format!("F:{}@", self.t(field)), &base)
            }
            "call_expression" | "call" => self.eval_call(e, d),
            "struct_expression" => {
                let n = e.child_by_field_name("name")?;
                mk("T:", &squash(self.t(n)))
            }
            "new_expression" => {
                let c = e.child_by_field_name("constructor")?;
                mk("T:", &squash(self.t(c)))
            }
            "type_assertion_expression" | "composite_literal" => {
                let t = e.child_by_field_name("type")?;
                mk("T:", &squash(self.t(t)))
            }
            "as_expression" | "satisfies_expression" => {
                let t = e.named_child(1)?;
                mk("T:", &squash(self.t(t)))
            }
            _ => None,
        }
    }

    /// (object, field) of a member access.
    fn member_parts(e: Node<'_>) -> Option<(Node<'_>, Node<'_>)> {
        let obj = e
            .child_by_field_name("value")
            .or_else(|| e.child_by_field_name("operand"))
            .or_else(|| e.child_by_field_name("object"))?;
        let field = e
            .child_by_field_name("field")
            .or_else(|| e.child_by_field_name("property"))
            .or_else(|| e.child_by_field_name("attribute"))?;
        Some((obj, field))
    }

    fn eval_call(&mut self, e: Node<'a>, d: u8) -> Option<String> {
        let mut f = e.child_by_field_name("function")?;
        if f.kind() == "generic_function" {
            f = f.child_by_field_name("function")?;
        }
        match f.kind() {
            "identifier" => {
                let name = self.t(f).to_string();
                if self.lang == Lang::Go && name == "new" {
                    let arg = e.child_by_field_name("arguments")?.named_child(0)?;
                    return mk("T:", &squash(self.t(arg)));
                }
                if self.lookup(&name, f).is_some_and(|dcl| !dcl.func) {
                    // Calling a local closure / function-typed variable.
                    return None;
                }
                mk("C:", &format!("{name}{}", self.arg_shape(e)))
            }
            "scoped_identifier" => {
                let path = squash(&strip_generics(self.t(f)));
                // `Arc::new(x)`, `Box::new(x)`: smart pointers deref to `x`'s type.
                let wrapper = path.rsplit_once("::").is_some_and(|(q, n)| {
                    n == "new"
                        && matches!(
                            q.rsplit("::").next().unwrap_or(q),
                            "Arc" | "Rc" | "Box" | "RefCell" | "Mutex" | "RwLock" | "Cell"
                        )
                });
                if wrapper {
                    let arg = e.child_by_field_name("arguments")?.named_child(0)?;
                    return self.eval(arg, d);
                }
                mk("C:", &path)
            }
            "field_expression" | "selector_expression" | "member_expression" | "attribute" => {
                let (obj, field) = Self::member_parts(f)?;
                let fname = self.t(field).to_string();
                let method_syntax = self.lang == Lang::Rust;
                if !method_syntax
                    && matches!(obj.kind(), "identifier" | "nested_identifier")
                    && self.lookup(self.t(obj), obj).is_none()
                {
                    // `pkg.F()`, `mod.f()`, `Class.create()`: a package-level callee.
                    let shape = self.arg_shape(e);
                    return mk("C:", &format!("{}.{fname}{shape}", self.t(obj)));
                }
                if self.lang.is_js_family() {
                    // `ns.value.m()`: a member of a module-level value in namespace `ns`.
                    if let Some(p) = self.bare_path(obj, 0) {
                        let shape = self.arg_shape(e);
                        return mk("C:", &format!("{p}.{fname}{shape}"));
                    }
                }
                let base = self.eval(obj, d)?;
                if base == "L" {
                    return None;
                }
                mk(&format!("M:{fname}@"), &base)
            }
            _ => None,
        }
    }

    /// JS/TS: the shape of a call's first argument, appended to a `C:` hint to pick
    /// among call signatures: `#f` a function literal, `#o` another literal or `new`.
    fn arg_shape(&self, call: Node<'a>) -> &'static str {
        if !self.lang.is_js_family() {
            return "";
        }
        let Some(a) = call
            .child_by_field_name("arguments")
            .and_then(|a| a.named_child(0))
        else {
            return "";
        };
        match a.kind() {
            "arrow_function" | "function_expression" | "function" | "generator_function" => "#f",
            "object" | "array" | "string" | "number" | "template_string" | "new_expression"
            | "true" | "false" => "#o",
            _ => "",
        }
    }

    /// `a.b` where `a` is not a local (a namespace import): the dotted text. Two levels
    /// at most.
    fn bare_path(&mut self, e: Node<'a>, depth: u8) -> Option<String> {
        match e.kind() {
            "member_expression" if depth == 0 => {
                let (obj, field) = Self::member_parts(e)?;
                if field.kind() != "property_identifier" {
                    return None;
                }
                let head = self.bare_path(obj, depth + 1)?;
                Some(format!("{head}.{}", self.t(field)))
            }
            "identifier" if depth > 0 => {
                let n = self.t(e);
                self.lookup(n, e).is_none().then(|| n.to_string())
            }
            _ => None,
        }
    }

    // ---- symbol facts --------------------------------------------------------

    /// Type hint of a module-level JS/TS value (`variable_declarator`): its declared type,
    /// else its initializer's.
    pub(crate) fn value_type(&mut self, decl: Node<'a>) -> Option<String> {
        if !self.supported() || !self.hints_enabled() {
            return None;
        }
        if let Some(t) = decl
            .child_by_field_name("type")
            .and_then(|t| t.named_child(0))
        {
            return mk("T:", &squash(self.t(t)));
        }
        let v = decl.child_by_field_name("value")?;
        self.eval(v, 0).filter(|h| h != "L")
    }

    /// A TS call signature (`<T>(fn: () => T): Box<T>` in an interface, or a function
    /// type): the field `()<shape>` -> its return type, `<shape>` from the first
    /// parameter's type (see [`param_shape`]).
    fn call_sig(&self, sig: Node<'a>) -> Option<(String, String)> {
        let ret = sig.child_by_field_name("return_type")?;
        let ret = if ret.kind() == "type_annotation" {
            ret.named_child(0)?
        } else {
            ret
        };
        let gens: Vec<String> = sig
            .child_by_field_name("type_parameters")
            .map(|tp| {
                named(tp)
                    .into_iter()
                    .filter_map(|p| p.child_by_field_name("name"))
                    .map(|n| self.t(n).to_string())
                    .collect()
            })
            .unwrap_or_default();
        let first = sig.child_by_field_name("parameters").and_then(|ps| {
            named(ps).into_iter().find(|p| {
                matches!(p.kind(), "required_parameter" | "optional_parameter")
                    && p.child_by_field_name("pattern")
                        .is_none_or(|n| self.t(n) != "this")
            })
        });
        let shape = match first {
            None => '0',
            Some(p) => {
                let ty = p
                    .child_by_field_name("type")
                    .and_then(|t| t.named_child(0))
                    .map(|t| squash(self.t(t)));
                param_shape(ty.as_deref(), &gens)
            }
        };
        Some((format!("(){shape}"), mk("T:", &squash(self.t(ret)))?))
    }

    /// Return type hint of a function/method definition node.
    pub(crate) fn ret_of(&self, def: Node<'a>) -> Option<String> {
        if !self.supported() {
            return None;
        }
        let r = match self.lang {
            Lang::Go => {
                let r = def.child_by_field_name("result")?;
                if r.kind() == "parameter_list" {
                    r.named_child(0)?.child_by_field_name("type")?
                } else {
                    r
                }
            }
            Lang::Rust => {
                let r = def.child_by_field_name("return_type")?;
                let b = self.generics(def);
                return subst(&squash(self.t(r)), &b).and_then(|t| mk("T:", &t));
            }
            Lang::TypeScript | Lang::Tsx | Lang::JavaScript => {
                let r = def.child_by_field_name("return_type")?;
                r.named_child(0).unwrap_or(r)
            }
            Lang::Python => def.child_by_field_name("return_type")?,
            Lang::C | Lang::Cpp => return None,
        };
        mk("T:", &squash(self.t(r)))
    }

    /// Field hints of a struct/class definition node: (name, hint). Go embedded fields
    /// are named `~Type`.
    pub(crate) fn fields_of(&mut self, def: Node<'a>) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = Vec::new();
        if !self.supported() {
            return out;
        }
        match self.lang {
            Lang::Go => {
                if let Some(it) = def
                    .child_by_field_name("type")
                    .filter(|t| t.kind() == "interface_type")
                {
                    // Embedded interfaces (`io.Reader`, `Base`) complete the method set.
                    let mut c = it.walk();
                    for el in it.named_children(&mut c) {
                        if el.kind() != "type_elem" {
                            continue;
                        }
                        let mut c2 = el.walk();
                        for ty in el.named_children(&mut c2) {
                            let tys = squash(self.t(ty));
                            let base = crate::extract::base_type_name(&tys);
                            out.push((format!("~{base}"), format!("T:{tys}")));
                        }
                    }
                    return out;
                }
                let Some(st) = def
                    .child_by_field_name("type")
                    .filter(|t| t.kind() == "struct_type")
                else {
                    return out;
                };
                let Some(list) = st.named_child(0) else {
                    return out;
                };
                let mut c = list.walk();
                for fd in list.named_children(&mut c) {
                    if fd.kind() != "field_declaration" {
                        continue;
                    }
                    let Some(ty) = fd.child_by_field_name("type") else {
                        continue;
                    };
                    let tys = squash(self.t(ty));
                    let mut c2 = fd.walk();
                    let names: Vec<Node<'_>> = fd.children_by_field_name("name", &mut c2).collect();
                    if names.is_empty() {
                        let base = crate::extract::base_type_name(&tys);
                        out.push((format!("~{base}"), format!("T:{tys}")));
                    }
                    for n in names {
                        out.push((self.t(n).to_string(), format!("T:{tys}")));
                    }
                }
            }
            Lang::Rust => {
                let Some(body) = def
                    .child_by_field_name("body")
                    .filter(|b| b.kind() == "field_declaration_list")
                else {
                    return out;
                };
                let mut c = body.walk();
                for fd in body.named_children(&mut c) {
                    if let (Some(n), Some(t)) = (
                        fd.child_by_field_name("name"),
                        fd.child_by_field_name("type"),
                    ) {
                        out.push((self.t(n).to_string(), format!("T:{}", squash(self.t(t)))));
                    }
                }
            }
            Lang::TypeScript | Lang::Tsx | Lang::JavaScript => {
                let body = if def.kind() == "type_alias_declaration" {
                    // `type F = (a: A) => R` / `type O = { m(): R; (x): Y }`
                    let Some(v) = def.child_by_field_name("value") else {
                        return out;
                    };
                    if v.kind() == "function_type" {
                        out.extend(self.call_sig(v));
                        return out;
                    }
                    if v.kind() != "object_type" {
                        return out;
                    }
                    Some(v)
                } else {
                    def.child_by_field_name("body")
                };
                let Some(body) = body else {
                    return out;
                };
                let mut c = body.walk();
                let members: Vec<Node<'_>> = body.named_children(&mut c).collect();
                for m in members {
                    match m.kind() {
                        "public_field_definition" | "field_definition" | "property_signature" => {
                            let Some(n) = m
                                .child_by_field_name("name")
                                .or_else(|| m.child_by_field_name("property"))
                            else {
                                continue;
                            };
                            let hint = m
                                .child_by_field_name("type")
                                .and_then(|t| t.named_child(0))
                                .and_then(|t| mk("T:", &squash(self.t(t))))
                                .or_else(|| {
                                    m.child_by_field_name("value").and_then(|v| self.eval(v, 0))
                                });
                            if let Some(h) = hint {
                                out.push((self.t(n).trim_start_matches('#').to_string(), h));
                            }
                        }
                        "call_signature" => out.extend(self.call_sig(m)),
                        "method_definition" => {
                            let is_ctor = m
                                .child_by_field_name("name")
                                .is_some_and(|n| self.t(n) == "constructor");
                            if !is_ctor {
                                continue;
                            }
                            if let Some(ps) = m.child_by_field_name("parameters") {
                                let mut c2 = ps.walk();
                                for p in ps.named_children(&mut c2) {
                                    let prop = has_kind(p, "accessibility_modifier")
                                        || has_kind(p, "readonly");
                                    if !prop {
                                        continue;
                                    }
                                    let ty = p
                                        .child_by_field_name("type")
                                        .and_then(|t| t.named_child(0))
                                        .and_then(|t| mk("T:", &squash(self.t(t))));
                                    if let (Some(pat), Some(ty)) =
                                        (p.child_by_field_name("pattern"), ty)
                                    {
                                        out.push((self.t(pat).to_string(), ty));
                                    }
                                }
                            }
                            if let Some(b) = m.child_by_field_name("body") {
                                self.self_assignments(b, &mut out);
                            }
                        }
                        _ => {}
                    }
                }
            }
            Lang::Python => {
                let Some(body) = def.child_by_field_name("body") else {
                    return out;
                };
                let mut c = body.walk();
                let items: Vec<Node<'_>> = body.named_children(&mut c).collect();
                for st in items {
                    // class-level `x: T = ...`
                    if st.kind() == "expression_statement" {
                        if let Some(a) = st.named_child(0).filter(|a| a.kind() == "assignment") {
                            if let (Some(l), Some(t)) =
                                (a.child_by_field_name("left"), a.child_by_field_name("type"))
                            {
                                if l.kind() == "identifier" {
                                    out.push((
                                        self.t(l).to_string(),
                                        format!("T:{}", squash(self.t(t))),
                                    ));
                                }
                            }
                        }
                    }
                    let func = if st.kind() == "decorated_definition" {
                        st.child_by_field_name("definition")
                    } else {
                        Some(st)
                    };
                    if let Some(f) = func.filter(|f| f.kind() == "function_definition") {
                        if let Some(b) = f.child_by_field_name("body") {
                            self.self_assignments(b, &mut out);
                        }
                    }
                }
            }
            Lang::C | Lang::Cpp => {}
        }
        out
    }

    /// `this.x = e` / `self.x = e` inside a method body -> field hints (first wins).
    fn self_assignments(&mut self, body: Node<'a>, out: &mut Vec<(String, String)>) {
        let mut stack = vec![body];
        while let Some(n) = stack.pop() {
            if n.id() != body.id() && self.is_function(n) {
                continue;
            }
            let is_assign = matches!(n.kind(), "assignment_expression" | "assignment");
            if is_assign {
                let l = n.child_by_field_name("left");
                let r = n.child_by_field_name("right");
                if let Some(l) = l.filter(|l| matches!(l.kind(), "member_expression" | "attribute"))
                {
                    if let Some((obj, field)) = Self::member_parts(l) {
                        let is_self = matches!(obj.kind(), "this")
                            || (obj.kind() == "identifier" && self.t(obj) == "self");
                        let name = self.t(field).trim_start_matches('#').to_string();
                        if is_self && !out.iter().any(|(k, _)| *k == name) {
                            let hint = n
                                .child_by_field_name("type")
                                .and_then(|t| mk("T:", &squash(self.t(t))))
                                .or_else(|| r.and_then(|v| self.eval(v, 0)));
                            if let Some(h) = hint.filter(|h| h != "L") {
                                out.push((name, h));
                            }
                        }
                    }
                }
            }
            let mut c = n.walk();
            for ch in n.named_children(&mut c) {
                stack.push(ch);
            }
        }
    }
}

/// Methods whose callback's first parameter is an element of the receiver.
const ITER_CALLBACKS: &[&str] = &[
    "map",
    "filter",
    "for_each",
    "any",
    "all",
    "find",
    "filter_map",
    "flat_map",
    "position",
    "take_while",
    "skip_while",
    "inspect",
    "and_then",
    "map_or",
    "map_or_else",
    "is_some_and",
    "is_none_or",
    "find_map",
    "max_by_key",
    "min_by_key",
    "sort_by_key",
    "retain",
    "partition",
    "forEach",
    "some",
    "every",
    "findIndex",
    "findLast",
    "flatMap",
];

/// Shape of a call signature's first parameter type: `f` a function type, `g` a type
/// parameter / `any` / `unknown` / no annotation (accepts anything), `n` anything else.
pub(crate) fn param_shape(ty: Option<&str>, generics: &[String]) -> char {
    let Some(t) = ty.map(str::trim) else {
        return 'g';
    };
    let mut depth = 0i32;
    let b = t.as_bytes();
    for (i, &c) in b.iter().enumerate() {
        match c {
            b'(' | b'[' | b'{' | b'<' => depth += 1,
            b'>' if i > 0 && b[i - 1] == b'=' => {
                if depth == 0 {
                    return 'f';
                }
            }
            b')' | b']' | b'}' | b'>' => depth -= 1,
            _ => {}
        }
    }
    if matches!(t, "any" | "unknown" | "Function") || generics.iter().any(|g| g == t) {
        'g'
    } else {
        'n'
    }
}

/// Whether `p` is the first named parameter in `ps`.
fn first_param(ps: Node<'_>, p: Node<'_>) -> bool {
    let mut c = ps.walk();
    let first = ps
        .named_children(&mut c)
        .next()
        .is_some_and(|x| x.id() == p.id());
    first
}

fn named(n: Node<'_>) -> Vec<Node<'_>> {
    let mut c = n.walk();
    n.named_children(&mut c).collect()
}

fn has_kind(n: Node<'_>, kind: &str) -> bool {
    let mut c = n.walk();
    let found = n.children(&mut c).any(|ch| ch.kind() == kind);
    found
}

/// Identifiers bound by a pattern (`x`, `mut x`, `(a, b)`, `{a, b: c}`, `[x, ...rest]`).
fn pattern_names(p: Node<'_>) -> Vec<Node<'_>> {
    let mut out = Vec::new();
    let mut stack = vec![p];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "identifier"
            | "shorthand_property_identifier_pattern"
            | "shorthand_field_identifier" => {
                out.push(n);
            }
            // Struct/enum paths inside patterns are not bindings.
            "scoped_identifier"
            | "type_identifier"
            | "field_identifier"
            | "property_identifier"
            | "string"
            | "number"
            | "integer_literal"
            | "type_annotation" => {}
            "pair_pattern" => {
                if let Some(v) = n.child_by_field_name("value") {
                    stack.push(v);
                }
            }
            "tuple_struct_pattern" | "struct_pattern" => {
                let mut c = n.walk();
                for (i, ch) in n.named_children(&mut c).enumerate() {
                    if i > 0 || n.kind() == "struct_pattern" {
                        stack.push(ch);
                    }
                }
            }
            "field_pattern" => {
                if let Some(v) = n.child_by_field_name("pattern") {
                    stack.push(v);
                } else if let Some(v) = n.child_by_field_name("name") {
                    if v.kind() == "shorthand_field_identifier" {
                        out.push(v);
                    }
                }
            }
            _ => {
                let mut c = n.walk();
                for ch in n.named_children(&mut c) {
                    stack.push(ch);
                }
            }
        }
    }
    out
}

/// Remove `<...>` / turbofish arguments from a path (`Vec::<u8>::new` -> `Vec::new`).
fn strip_generics(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut depth = 0i32;
    for ch in s.chars() {
        match ch {
            '<' => depth += 1,
            '>' if depth > 0 => depth -= 1,
            c if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out.replace("::::", "::")
}

/// Replace a bare generic parameter (`B`, `&B`, `&mut B`, `*T`) with its bound; `None`
/// when the parameter has no single meaningful bound (no hint beats a wrong one).
fn subst(ty: &str, gens: &[(String, Option<String>)]) -> Option<String> {
    let core = ty.trim_start_matches(['&', '*']);
    let core = core
        .strip_prefix("mut ")
        .unwrap_or(core)
        .trim_start_matches(['&', '*']);
    match gens.iter().find(|(n, _)| n == core) {
        Some((_, b)) => b.clone(),
        None => Some(ty.to_string()),
    }
}

/// A bound we can resolve: a plain (possibly qualified or generic) type name, not a
/// union, intersection, literal or anonymous interface.
fn simple_bound(t: &str) -> bool {
    !t.is_empty()
        && !t.contains(['|', '&', '{', '(', '~', '"', '\''])
        && t.chars()
            .next()
            .is_some_and(|c| c.is_alphabetic() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers() {
        assert_eq!(strip_generics("Vec::<u8>::new"), "Vec::new");
        let g = |b: Option<&str>| vec![("B".to_string(), b.map(str::to_string))];
        assert_eq!(subst("&B", &g(Some("Backend"))).as_deref(), Some("Backend"));
        assert_eq!(subst("&B", &g(None)), None);
        assert_eq!(
            subst("&mut B", &g(Some("Backend"))).as_deref(),
            Some("Backend")
        );
        assert_eq!(squash("&'a  Repo"), "&'a Repo");
        assert_eq!(squash("Box< dyn Backend >"), "Box<dyn Backend>");
        assert_eq!(squash("impl Backend + Send"), "impl Backend+Send");
        assert_eq!(subst("Foo", &g(Some("Backend"))).as_deref(), Some("Foo"));
    }
}

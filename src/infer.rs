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
//! | `F:<name>@<hint>` | field `name` of `<hint>`; Rust: `F:1@..` is a tuple element or positional field, `F:Number.0@T:Value` payload 0 of enum variant `Value::Number` (from match/`if let`/`let` patterns) |
//! | `Q:<hint>` | `<hint>` unwrapped (Rust `?`, `.unwrap()`); `A:<hint>` awaited |
//! | `E:<hint>` | an element of the collection `<hint>` (loop variables, callback params) |
//! | `G:<name>` / `G:<pkg>.<name>` | Go: a package-level var/const not declared in this file (same package, or of imported package `pkg`); evaluated through the var's stored type. `G:<var>.<field>` when `<var>` is not a package |
//! | `N:<guard>@<hint>` | a variable narrowed by type guard `<guard>` (see JS/TS narrowing), else `<hint>` |
//! | `O:<a>-<b>` | JS/TS: the object literal / inline object type on lines `a..=b` of the file the hint belongs to (a parameter typed `{ m(): X }`, or the return of an unannotated function returning `{ m() {..} }`): its methods are the method symbols written there (the first on the line when two such types share one) |
//! | `L` | a local variable or parameter with no known type (never a repo symbol) |
//! | `X` | ([`SCOPED`]) not a type: a Rust capitalized name in value or pattern position (`&NoProgress`, `Ident =>`); it resolves through the file's own scope (definitions, imports, module, globs), never by a crate-wide name guess — a glob-imported enum variant is no symbol and must not land on a same-named type |
//! | `I:<n>@<hint>` | Python: element `n` of the tuple `<hint>` (`for a, b in xs`) |
//! | `P:<n>@<method>` | Python: untyped parameter `n` of `<method>`, typed by the same position in a base class's `<method>` |
//! | `V:<hint>` | Python: a value of the collection `<hint>` (`d[k]`: `dict[K, V]` -> `V`) |
//! | `D:<hint>` | Python: a decorated `def`, as the decorator's result |
//! | `U` / `U:<local>` | ([`BINDING`]) not a type: the reference is the name inside an import statement (`use a::B`, `import { b }`, `from m import c`); it resolves only through the binding that import makes, under `<local>` when aliased (`use a::B as C` -> `U:C`), never by name |
//!
//! Go package-level vars and consts store their type hint as the symbol's `ret` (the
//! written type, else the initializer's hint). A var bound to a function
//! (`var NewX = NewBase[int, cfg]`) stores `C:<function>`, so calling the var evaluates
//! to the function's result.
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
//! `M:get@<hint>` on a TS `Map<K, V>` / `WeakMap` / `ReadonlyMap` is a `V`.
//!
//! JS/TS narrowing: a variable read inside the consequence of `if (x instanceof C)` is
//! `T:C`; inside `if (G(x))` (also `&&` chains and `?:`) it is `G:<G>@<hint>`: the type
//! guard `G` (a function or function-typed value whose type reads `x is T`) asserts `T`;
//! when `G` is no type guard, `<hint>` (the variable's own hint) applies.
//!
//! `T:` names are bound where the hint was written: in JS/TS, a file declaring one type
//! name twice (sibling test callbacks each with `class Store`) binds the declaration
//! closest before the reference.
//!
//! Rust literals give `T:` hints of their std shape, `_` standing for an unknown part:
//! `"s"` -> `T:&str`, `b"s"` -> `T:&[u8;_]`, `[0u8; n]` -> `T:[u8;_]`, `vec![..]` ->
//! `T:Vec<_>`, `format!(..)` -> `T:String`, `x as T` -> `T:T`, a range index
//! `&b"s"[..]` -> `T:[u8]`; module-level `const`/`static` items bind their declared type.
//! A capitalized non-local value is a unit struct (`Special.put_u8(..)` -> `T:Special`),
//! `ManuallyDrop::new(x)` / `black_box(x)` are `x`, and `*p` with `p: *mut T` is a `T`.
//! The resolver adds `M:` results of shape-revealing std methods on a known non-repo
//! receiver (`to_vec` -> `Vec<_>`, `as_bytes` -> `&[u8]`, see `RUST_STD_SHAPES`).
//!
//! A second hint, the **argument hint** (`at` in the edge's `extra`, [`Infer::arg_hint`]),
//! uses the same grammar for the first argument of a Rust call named like a std trait
//! method a type may implement several times (`Bytes::from(x)`, `a.partial_cmp(b)`, the
//! resolver's `TRAIT_OVERLOADS`): the impl whose first parameter type fits it is picked.
//!
//! Rust generic parameters with a single meaningful bound are substituted in parameter
//! and return types directly; the function symbol also records them in its `fields` as
//! `(<T>, T:<bound>)` ([`Infer::generic_fields`]) so the resolver can bind what only it
//! sees: a field typed with a bare generic (`self.formatter` of `formatter: F` inside
//! `impl<W, F: Formatter>`) and a generic path qualifier (`R::discard(..)`).
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
use std::time::Duration;

use crate::clock::Instant;

use tree_sitter::Node;

use crate::lang::Lang;

/// Hint of a name inside an import statement (see the module table).
pub const BINDING: &str = "U";
/// Hint of a reference resolved through the file's scope only (see the module doc).
pub const SCOPED: &str = "X";

/// The local name a binding hint binds (`U:<local>` -> `<local>`; `U` -> `""`: the name
/// itself), `None` for any other hint.
pub(crate) fn binding_local(hint: &str) -> Option<&str> {
    match hint.strip_prefix(BINDING)? {
        "" => Some(""),
        rest => rest.strip_prefix(':'),
    }
}

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

    /// Strict ancestors of `n`, root first. Without the table this is one cursor descent
    /// from the root: `parent()` is O(depth), so walking up with it is O(depth^2).
    pub(crate) fn ancestors(&self, n: Node<'a>) -> Vec<Node<'a>> {
        let mut out = Vec::new();
        if self.table.get().is_none() {
            let mut c = self.root.walk();
            loop {
                let node = c.node();
                if node.id() == n.id() {
                    return out;
                }
                out.push(node);
                if c.goto_first_child_for_byte(n.start_byte()).is_none()
                    || c.node().start_byte() > n.start_byte()
                {
                    break;
                }
            }
            // `n` was not reached (zero-width nodes): walk up instead.
            out.clear();
        }
        let mut cur = self.parent(n);
        while let Some(p) = cur {
            out.push(p);
            cur = self.parent(p);
        }
        out.reverse();
        out
    }

    /// Function-like strict ancestors of `n`, innermost first.
    fn fn_chain(&self, lang: Lang, n: Node<'a>) -> Vec<Node<'a>> {
        if self.table.get().is_some() {
            let mut out = Vec::new();
            let mut cur = self.enclosing_fn(lang, n);
            while let Some(f) = cur {
                out.push(f);
                cur = self.enclosing_fn(lang, f);
            }
            return out;
        }
        let mut out: Vec<Node<'a>> = self
            .ancestors(n)
            .into_iter()
            .filter(|&a| is_function(lang, a))
            .collect();
        out.reverse();
        out
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
    /// Python: a decorated `def`; `value` is its topmost decorator's expression, and
    /// used as a value the name is that decorator's result (`@click.group() def cli`).
    deco: bool,
    /// Python `with m as x`: `x` is what `m.__enter__()` returns.
    enter: bool,
    /// Python `for a, b in xs`: position in the element tuple.
    tup: Option<u8>,
}

/// One function's bindings, indexed by name (a lookup per reference: a linear scan made
/// big functions quadratic).
struct Env<'a> {
    decls: Vec<Decl<'a>>,
    by_name: HashMap<String, Vec<usize>>,
}

/// Per-file inference state (function environments are built lazily).
pub(crate) struct Infer<'a> {
    lang: Lang,
    src: &'a [u8],
    envs: HashMap<usize, Env<'a>>,
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
        if self.lang == Lang::Python {
            if let Some(c) = self.isinstance_narrowing(q) {
                return mk("T:", &c);
            }
        }
        self.eval(q, 0)
    }

    /// Python: the class `x` is narrowed to where `x` (an identifier or attribute chain)
    /// is used: inside `if isinstance(x, C):`, or after `assert isinstance(x, C)` in the
    /// same block. Single classes only (a tuple of classes has no one type).
    fn isinstance_narrowing(&self, q: Node<'a>) -> Option<String> {
        let target = self.t(q);
        let class_of = |cond: Node<'a>| -> Option<String> {
            if cond.kind() != "call" {
                return None;
            }
            let f = cond.child_by_field_name("function")?;
            if self.t(f) != "isinstance" {
                return None;
            }
            let args = cond.child_by_field_name("arguments")?;
            let (a, c) = (args.named_child(0)?, args.named_child(1)?);
            (self.t(a) == target && matches!(c.kind(), "identifier" | "attribute"))
                .then(|| self.t(c).to_string())
        };
        let mut child = q;
        let mut cur = self.nav.parent(q);
        while let Some(n) = cur {
            match n.kind() {
                "if_statement" | "elif_clause" => {
                    let inside = n
                        .child_by_field_name("consequence")
                        .is_some_and(|b| b.id() == child.id());
                    if inside {
                        if let Some(c) = n.child_by_field_name("condition").and_then(class_of) {
                            return Some(c);
                        }
                    }
                }
                "block" | "module" => {
                    // `assert isinstance(x, C)` earlier in this block.
                    let mut sib = self.nav.prev_sibling(child);
                    while let Some(st) = sib {
                        if st.kind() == "assert_statement" {
                            if let Some(c) = st.named_child(0).and_then(class_of) {
                                return Some(c);
                            }
                        }
                        sib = self.nav.prev_sibling(st);
                    }
                }
                "function_definition" | "class_definition" | "lambda" => return None,
                _ => {}
            }
            child = n;
            cur = self.nav.parent(n);
        }
        None
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
                // `default` is a (contextual) keyword token: `X::default()`.
                let m = prev.filter(|p| matches!(p.kind(), "identifier" | "default"))?;
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
        // A nested `def` / `fn` / JS function is a symbol of its own; a closure, Go func
        // literal or lambda bound to a local is not, so a call through it is no repo call.
        let literal_only = matches!(self.lang, Lang::Rust | Lang::Go | Lang::Python);
        matches!(self.lookup_local(&n, name), Some(d)
            if !d.func || (literal_only && d.value.is_some_and(Self::is_func_value)))
    }

    fn supported(&self) -> bool {
        !self.lang.is_c_family()
    }

    // ---- environments --------------------------------------------------------

    fn lookup_local(&mut self, name: &str, at: Node<'a>) -> Option<Decl<'a>> {
        let pos = at.start_byte();
        let nav = self.nav;
        for n in nav.fn_chain(self.lang, at) {
            let env = self.env(n);
            if let Some(d) = env
                .by_name
                .get(name)
                .into_iter()
                .flatten()
                .map(|&i| &env.decls[i])
                .filter(|d| d.at <= pos)
                .max_by_key(|d| d.at)
                .cloned()
            {
                return Some(d);
            }
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

    fn env(&mut self, f: Node<'a>) -> &Env<'a> {
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
            let mut by_name: HashMap<String, Vec<usize>> = HashMap::new();
            for (i, d) in out.iter().enumerate() {
                by_name.entry(d.name.clone()).or_default().push(i);
            }
            self.envs.insert(
                id,
                Env {
                    decls: out,
                    by_name,
                },
            );
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
            deco: false,
            enter: false,
            tup: None,
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
            deco: false,
            enter: false,
            tup: None,
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
        // `@F:1`: a projection of the value's hint (`let (a, b) = f()` binds b to
        // `F:1@C:f`), see `rust_pattern_decls`.
        let proj = d.hint.as_deref().and_then(|h| h.strip_prefix('@'));
        if let Some(p) = proj {
            let p = p.to_string();
            let base = self.decl_hint(
                &Decl {
                    hint: None,
                    ..d.clone()
                },
                depth,
            )?;
            return (base != "L").then(|| mk(&format!("{p}@"), &base)).flatten();
        }
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
            if d.enter {
                mk("M:__enter__@", &h)
            } else if !d.elem {
                Some(h)
            } else if h == "L" {
                None
            } else {
                let e = mk("E:", &h)?;
                match d.tup {
                    Some(i) => mk(&format!("I:{i}@"), &e),
                    None => Some(e),
                }
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
                // Parameters, then named results (`func f() (g *Group, err error)`).
                let lists = [
                    f.child_by_field_name("parameters"),
                    f.child_by_field_name("result")
                        .filter(|r| r.kind() == "parameter_list"),
                ];
                for ps in lists.into_iter().flatten() {
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
                        let tn = p.child_by_field_name("type").and_then(|t| t.named_child(0));
                        let ty = match tn {
                            // `(v: { entries(): X })`: the members written there.
                            Some(t) if t.kind() == "object_type" => object_hint(t),
                            _ => tn
                                .and_then(|t| subst(&squash(self.t(t)), &gens))
                                .and_then(|t| mk("T:", &t)),
                        };
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
                // A method (directly in a class body): an untyped parameter may take its
                // type from the same method of a base class (`P:`).
                let method = f
                    .parent()
                    .and_then(|p| {
                        if p.kind() == "decorated_definition" {
                            p.parent()
                        } else {
                            Some(p)
                        }
                    })
                    .filter(|b| b.kind() == "block")
                    .and_then(|b| b.parent())
                    .filter(|c| c.kind() == "class_definition")
                    .and(f.child_by_field_name("name"))
                    .map(|n| self.t(n).to_string());
                let mut c = ps.walk();
                for (i, p) in ps.named_children(&mut c).enumerate() {
                    match p.kind() {
                        "identifier" => {
                            let n = self.t(p);
                            let h = if i == 0 && matches!(n, "self" | "cls") {
                                Some("S".into())
                            } else {
                                method.as_ref().map(|m| format!("P:{i}@{m}"))
                            };
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
                        // `@click.group() def cli`: as a value, `cli` is the decorator's
                        // result. The topmost decorator is applied last.
                        let deco = n
                            .parent()
                            .filter(|p| p.kind() == "decorated_definition")
                            .and_then(|p| p.named_child(0))
                            .filter(|d| d.kind() == "decorator")
                            .and_then(|d| d.named_child(0));
                        if let (Some(v), Some(last)) = (deco, out.last_mut()) {
                            last.value = Some(v);
                            last.deco = true;
                            // `@click.command(cls=AliasedGroup)`: an instance of `cls`.
                            last.hint = self.cls_argument(v);
                        }
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
                // `var vc VC` inside a generic function: VC's bound.
                let ty = n.child_by_field_name("type").and_then(|t| {
                    let gens = self
                        .nav
                        .enclosing_fn(self.lang, n)
                        .map(|f| self.generics(f))
                        .unwrap_or_default();
                    subst(&squash(self.t(t)), &gens).and_then(|t| mk("T:", &t))
                });
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
            // `case msg := <-ch:` in a `select`.
            (Lang::Go, "receive_statement") => {
                let declares = (0..n.child_count())
                    .filter_map(|i| n.child(i))
                    .any(|c| c.kind() == ":=");
                if let Some(l) = n.child_by_field_name("left").filter(|_| declares) {
                    for x in named(l) {
                        Self::decl(out, self.t(x), at, None, false);
                    }
                }
            }
            // A function-local `const Size = 8`.
            (Lang::Go, "const_spec") => {
                let mut c = n.walk();
                let names: Vec<Node<'_>> = n.children_by_field_name("name", &mut c).collect();
                for nm in names {
                    Self::decl(out, self.t(nm), at, None, false);
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
                if !simple {
                    // `let (a, b) = f()`, `let Point { x, y } = p`.
                    self.rust_pattern_decls(pat, n.child_by_field_name("value"), at, out);
                    return;
                }
                for x in names {
                    Self::decl_v(out, self.t(x), at, hint.clone(), value, func);
                }
            }
            // Module-level only: a const in a function body is a repo symbol, not a local.
            (Lang::Rust, "const_item" | "static_item")
                if self
                    .nav
                    .parent(n)
                    .is_some_and(|p| matches!(p.kind(), "source_file" | "declaration_list")) =>
            {
                if let (Some(nm), Some(t)) =
                    (n.child_by_field_name("name"), n.child_by_field_name("type"))
                {
                    let hint = mk("T:", &squash(self.t(t)));
                    Self::decl(out, self.t(nm), at, hint, false);
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
                    _ => self.rust_pattern_decls(p, v, n.start_byte(), out),
                }
            }
            (Lang::Rust, "match_arm") => {
                if let Some(p) = n.child_by_field_name("pattern") {
                    // `match_pattern` wraps the pattern (and an `if` guard).
                    let p = if p.kind() == "match_pattern" {
                        p.named_child(0).unwrap_or(p)
                    } else {
                        p
                    };
                    let scrut = self
                        .nav
                        .parent(n)
                        .and_then(|b| self.nav.parent(b))
                        .filter(|m| m.kind() == "match_expression")
                        .and_then(|m| m.child_by_field_name("value"));
                    self.rust_pattern_decls(p, scrut, n.start_byte(), out);
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
            // `for name, cmd in xs`: element tuple positions.
            (Lang::Python, "for_statement" | "for_in_clause")
                if n.child_by_field_name("left").is_some_and(|l| {
                    matches!(l.kind(), "pattern_list" | "tuple_pattern")
                        && l.named_child_count() <= 8
                        && (0..l.named_child_count() as u32)
                            .all(|i| l.named_child(i).is_some_and(|c| c.kind() == "identifier"))
                }) =>
            {
                if let (Some(l), Some(r)) = (
                    n.child_by_field_name("left"),
                    n.child_by_field_name("right"),
                ) {
                    // A comprehension's variables are in scope in its whole expression,
                    // which comes before the `for` clause.
                    let at = if n.kind() == "for_in_clause" {
                        n.parent().map_or(r.end_byte(), |p| p.start_byte())
                    } else {
                        r.end_byte()
                    };
                    for i in 0..l.named_child_count() as u32 {
                        let Some(x) = l.named_child(i) else { continue };
                        Self::decl_elem(out, self.t(x), at, r);
                        if let Some(last) = out.last_mut() {
                            last.tup = Some(i as u8);
                        }
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
                    let at = if n.kind() == "for_in_clause" {
                        n.parent().map_or(r.end_byte(), |p| p.start_byte())
                    } else {
                        r.end_byte()
                    };
                    Self::decl_elem(out, self.t(l), at, r);
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
                    // `with m as x`: typed through `m.__enter__()`.
                    let managed = n
                        .parent()
                        .filter(|p| p.kind() == "with_item")
                        .and_then(|_| n.named_child(0));
                    let names = pattern_names(a);
                    for x in &names {
                        Self::decl(out, self.t(*x), n.start_byte(), None, false);
                        if let (Some(v), true, Some(last)) =
                            (managed, names.len() == 1, out.last_mut())
                        {
                            last.value = Some(v);
                            last.enter = true;
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// Bindings of a Rust pattern matched against `scrut` (a match arm, `if let`, `let`),
    /// with the types the pattern itself reveals:
    /// - `x` (catch-all) is the scrutinee; `Some(x)` / `Ok(x)` its payload (`E:`);
    /// - `Value::Number(n)` is payload 0 of that variant: `F:Number.0@T:Value` (enum
    ///   symbols record variant payloads as fields `Variant.<i>` / `Variant.<name>`);
    /// - `Compound::Map { ser, .. }` / `Point { x, .. }`: `F:Map.ser@T:Compound`,
    ///   `F:x@T:Point`;
    /// - `(a, b)`: element `i` of the scrutinee (`@F:i`, projected in `decl_hint`).
    ///
    /// Every other bound name is declared with no type (it still shadows).
    fn rust_pattern_decls(
        &self,
        p: Node<'a>,
        scrut: Option<Node<'a>>,
        at: usize,
        out: &mut Vec<Decl<'a>>,
    ) {
        let mut p = p;
        while p.kind() == "reference_pattern" {
            match p.named_child(0) {
                Some(c) => p = c,
                None => break,
            }
        }
        let binding = |x: Node<'a>| -> Option<Node<'a>> {
            let x = if matches!(x.kind(), "ref_pattern" | "mut_pattern") {
                x.named_child(x.named_child_count().saturating_sub(1) as u32)?
            } else {
                x
            };
            (x.kind() == "identifier").then_some(x)
        };
        let decl = |name: Node<'a>, hint: Option<String>, value: Option<Node<'a>>, elem| Decl {
            name: self.t(name).to_string(),
            at,
            hint,
            value,
            elem,
            func: false,
            deco: false,
            enter: false,
            tup: None,
        };
        let mut typed: Vec<(Node<'a>, Decl<'a>)> = Vec::new();
        match p.kind() {
            "identifier" | "mut_pattern" | "ref_pattern" => {
                // A lowercase name binds the scrutinee (`FOO` / `None` are constants).
                if let (Some(x), Some(v)) = (binding(p), scrut) {
                    if self
                        .t(x)
                        .starts_with(|c: char| c.is_lowercase() || c == '_')
                    {
                        typed.push((x, decl(x, None, Some(v), false)));
                    }
                }
            }
            "tuple_struct_pattern" => {
                let kids = named(p);
                let Some(path) = kids.first() else { return };
                let path = squash(self.t(*path));
                let payload = &kids[1..];
                let last = path.rsplit("::").next().unwrap_or(&path);
                if matches!(last, "Some" | "Ok") {
                    if let ([one], Some(v)) = (payload, scrut) {
                        if let Some(x) = binding(*one) {
                            typed.push((x, decl(x, None, Some(v), true)));
                        }
                    }
                } else if let Some((qual, variant)) = path.rsplit_once("::") {
                    for (i, c) in payload.iter().enumerate() {
                        if let Some(x) = binding(*c) {
                            let h = mk(&format!("F:{variant}.{i}@T:"), qual);
                            typed.push((x, decl(x, h, None, false)));
                        }
                    }
                }
            }
            "struct_pattern" => {
                let Some(ty) = p.child_by_field_name("type") else {
                    return;
                };
                let path = squash(self.t(ty));
                // `Enum::Variant { .. }` (two capitalized segments) or `Struct { .. }`.
                let (prefix, base) = match path.rsplit_once("::") {
                    Some((q, v))
                        if v.starts_with(char::is_uppercase)
                            && q.rsplit("::")
                                .next()
                                .is_some_and(|s| s.starts_with(char::is_uppercase)) =>
                    {
                        (format!("{v}."), q.to_string())
                    }
                    _ => (String::new(), path.clone()),
                };
                for fp in named(p) {
                    if fp.kind() != "field_pattern" {
                        continue;
                    }
                    let Some(fname) = fp.child_by_field_name("name") else {
                        continue;
                    };
                    let x = match fp.child_by_field_name("pattern") {
                        Some(pat) => binding(pat),
                        None => Some(fname),
                    };
                    if let Some(x) = x {
                        let h = mk(&format!("F:{prefix}{}@T:", self.t(fname)), &base);
                        typed.push((x, decl(x, h, None, false)));
                    }
                }
            }
            "tuple_pattern" => {
                if let Some(v) = scrut {
                    for (i, c) in named(p).into_iter().enumerate() {
                        if let Some(x) = binding(c) {
                            typed.push((x, decl(x, Some(format!("@F:{i}")), Some(v), false)));
                        }
                    }
                }
            }
            _ => {}
        }
        for x in pattern_names(p) {
            match typed.iter().position(|(n, _)| n.id() == x.id()) {
                Some(i) => out.push(typed.swap_remove(i).1),
                None => Self::decl(out, self.t(x), at, None, false),
            }
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
                // `func (s *Set[T]) Add(v T)`: T is the receiver type's parameter, bound
                // (by position) as in the type's declaration when it is in this file.
                if let Some(r) = f.child_by_field_name("receiver") {
                    let text = squash(self.t(r));
                    // `]` before `[` happens in (broken) real input: `func (s ]T[) F()`,
                    // or brackets inside comments in the receiver; `get` keeps the
                    // slices in bounds instead of panicking the parser thread.
                    if let (Some(a), Some(b)) = (text.find('['), text.rfind(']')) {
                        if let (Some(head), Some(list)) = (text.get(..a), text.get(a + 1..b)) {
                            let tname = head
                                .rsplit([' ', '*', '('])
                                .next()
                                .unwrap_or("")
                                .to_string();
                            let bounds = self.go_type_param_bounds(&tname);
                            for (i, n) in list.split(',').enumerate() {
                                add(n.trim(), bounds.get(i).cloned().flatten());
                            }
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

    /// Go: the bound of each type parameter (in order) of the generic type `name`
    /// declared at the top level of this file (`type Base[T any, VC Creator[T]] struct`
    /// -> [None, Some("Creator[T]")]); empty when the type is declared elsewhere.
    fn go_type_param_bounds(&self, name: &str) -> Vec<Option<String>> {
        let mut out = Vec::new();
        for decl in named(self.root) {
            if decl.kind() != "type_declaration" {
                continue;
            }
            for spec in named(decl) {
                if spec.kind() != "type_spec"
                    || spec
                        .child_by_field_name("name")
                        .is_none_or(|n| self.t(n) != name)
                {
                    continue;
                }
                let Some(tp) = spec.child_by_field_name("type_parameters") else {
                    return out;
                };
                for d in named(tp) {
                    let bound = d
                        .child_by_field_name("type")
                        .map(|t| squash(self.t(t)))
                        .filter(|t| simple_bound(t) && !matches!(t.as_str(), "any" | "comparable"));
                    let mut c = d.walk();
                    let count = d.children_by_field_name("name", &mut c).count();
                    for _ in 0..count {
                        out.push(bound.clone());
                    }
                }
                return out;
            }
        }
        out
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
                    Some(dcl) if dcl.deco => {
                        if dcl.hint.is_some() {
                            return dcl.hint;
                        }
                        let v = dcl.value?;
                        let h = self.eval(v, d)?;
                        mk("D:", &h)
                    }
                    Some(dcl) if dcl.func => None,
                    Some(dcl) => {
                        let h = self.decl_hint(&dcl, depth).unwrap_or_else(|| "L".into());
                        if self.lang.is_js_family() {
                            match self.narrowed(e, &name, dcl.at) {
                                Some(Narrow::Type(t)) => return mk("T:", &t).or(Some(h)),
                                Some(Narrow::Guard(g)) => {
                                    return mk(&format!("N:{g}@"), &h).or(Some(h));
                                }
                                None => {}
                            }
                        }
                        Some(h)
                    }
                    // Go: a package-level var declared in another file (or a package
                    // name, which the resolver tells apart).
                    None if self.lang == Lang::Go => mk("G:", &name),
                    // A capitalized, non-constant name used as a value: a unit struct.
                    None if self.lang == Lang::Rust
                        && name.starts_with(|c: char| c.is_ascii_uppercase())
                        && name.chars().any(|c| c.is_ascii_lowercase())
                        && !matches!(name.as_str(), "None" | "Some" | "Ok" | "Err" | "Self") =>
                    {
                        mk("T:", &name)
                    }
                    None => None,
                }
            }
            // Python `d[k]`: a value of the subscripted collection.
            "subscript" if self.lang == Lang::Python => {
                let v = e.child_by_field_name("value")?;
                let h = self.eval(v, d)?;
                if h == "L" {
                    return None;
                }
                mk("V:", &h)
            }
            "parenthesized_expression"
            | "non_null_expression"
            | "reference_expression"
            | "expression_statement" => e
                // Rust `&mut x`: `mutable_specifier` is the first named child.
                .child_by_field_name("value")
                .or_else(|| e.named_child(0))
                .and_then(|c| self.eval(c, d)),
            "unary_expression" => {
                let inner = e
                    .child_by_field_name("operand")
                    .or_else(|| e.named_child(0))?;
                let h = self.eval(inner, d)?;
                // `*p` with `p: *mut T` is a `T` (other derefs keep the hint as before).
                if self.lang == Lang::Rust && self.t(e).starts_with('*') {
                    for ptr in ["T:*mut ", "T:*const "] {
                        if let Some(t) = h.strip_prefix(ptr) {
                            return mk("T:", t);
                        }
                    }
                }
                Some(h)
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
                if self.lang == Lang::Go {
                    if let Some(h) = self.anon_struct_field(obj, self.t(field)) {
                        return Some(h);
                    }
                    // `pkg.Var` (or a field of a package-level var declared elsewhere).
                    if obj.kind() == "identifier" && self.lookup(self.t(obj), obj).is_none() {
                        return mk("G:", &format!("{}.{}", self.t(obj), self.t(field)));
                    }
                }
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
            // Go `xs[i]`, `m[k]` (also the comma-ok `v, ok := m[k]`): an element.
            "index_expression" if self.lang == Lang::Go => {
                let o = e.child_by_field_name("operand")?;
                let h = self.eval(o, d)?;
                if h == "L" {
                    return None;
                }
                mk("E:", &h)
            }
            // Go `F[T](x)` parses as a conversion to a generic type; `F` is usually a
            // generic function (a generic type evaluates to itself either way).
            "type_conversion_expression" => {
                let t = e
                    .child_by_field_name("type")
                    .filter(|t| t.kind() == "generic_type")?
                    .child_by_field_name("type")?;
                let name = self.t(t);
                if self.lookup(name, t).is_some_and(|dcl| !dcl.func) {
                    // `handlers[k](x)`: indexing a local collection of functions.
                    return None;
                }
                mk("C:", name)
            }
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
            _ if self.lang == Lang::Rust => self.eval_rust_literal(e, d),
            _ => None,
        }
    }

    /// Go: the written type of field `field` of a variable whose value is an anonymous
    /// struct literal, or an element of one (table tests: `tests := []struct{ f Flag
    /// }{..}; for _, tt := range tests { tt.f.Names() }`). Struct type text is not a
    /// symbol, so the field is read from the literal's syntax here.
    fn anon_struct_field(&mut self, obj: Node<'a>, field: &str) -> Option<String> {
        if obj.kind() != "identifier" {
            return None;
        }
        let d = self.lookup(self.t(obj), obj)?;
        if d.hint.is_some() || d.func {
            return None;
        }
        let mut v = d.value?;
        if v.kind() == "identifier" {
            // `range tests`: the collection's own declaration.
            let d2 = self.lookup(self.t(v), v)?;
            if d2.hint.is_some() || d2.elem || d2.func {
                return None;
            }
            v = d2.value?;
        }
        if v.kind() != "composite_literal" {
            return None;
        }
        let mut ty = v.child_by_field_name("type")?;
        if d.elem {
            ty = match ty.kind() {
                "slice_type" | "array_type" | "implicit_length_array_type" => {
                    ty.child_by_field_name("element")?
                }
                "map_type" => ty.child_by_field_name("value")?,
                _ => return None,
            };
        }
        if ty.kind() == "pointer_type" {
            ty = ty.named_child(0)?;
        }
        if ty.kind() != "struct_type" {
            return None;
        }
        let list = ty.named_child(0)?;
        for fd in named(list) {
            if fd.kind() != "field_declaration" {
                continue;
            }
            let mut c = fd.walk();
            let hit = fd
                .children_by_field_name("name", &mut c)
                .any(|n| self.t(n) == field);
            if hit {
                let t = fd.child_by_field_name("type")?;
                if t.kind() == "struct_type" {
                    return None;
                }
                return mk("T:", &squash(self.t(t)));
            }
        }
        None
    }

    /// Rust literal shapes: `"s"` -> `&str`, `b"s"` -> `&[u8;_]`, `[x; n]` -> `[_;_]`,
    /// `vec![..]` -> `Vec<_>`, `format!(..)` -> `String`, `x as T` -> `T`, and a range
    /// index of a known type (`&b"s"[..]` -> `[u8]`). `_` is an unknown part.
    fn eval_rust_literal(&mut self, e: Node<'a>, d: u8) -> Option<String> {
        match e.kind() {
            "string_literal" | "raw_string_literal" => {
                let t = self.t(e);
                if t.starts_with('b') {
                    Some("T:&[u8;_]".into())
                } else if t.starts_with('c') {
                    None
                } else {
                    Some("T:&str".into())
                }
            }
            "array_expression" => {
                // `[0u8; 10]`, `[b'X'; 8]`: the element type when a literal shows it.
                let el = e.named_child(0).map_or("_", |x| {
                    let t = self.t(x);
                    match x.kind() {
                        "char_literal" if t.starts_with('b') => "u8",
                        "integer_literal" | "float_literal" => [
                            "u8", "u16", "u32", "u64", "u128", "usize", "i8", "i16", "i32", "i64",
                            "i128", "isize", "f32", "f64",
                        ]
                        .into_iter()
                        .find(|s| t.ends_with(s))
                        .unwrap_or("_"),
                        _ => "_",
                    }
                });
                mk("T:", &format!("[{el};_]"))
            }
            "macro_invocation" => {
                let m = e.child_by_field_name("macro")?;
                match self.t(m) {
                    "vec" => Some("T:Vec<_>".into()),
                    "format" => Some("T:String".into()),
                    _ => None,
                }
            }
            "type_cast_expression" => {
                let t = e.child_by_field_name("type")?;
                mk("T:", &squash(self.t(t)))
            }
            "index_expression" => {
                // `x[..]`, `x[a..b]`: a slice of `x`'s elements.
                let idx = e.named_child(1)?;
                if idx.kind() != "range_expression" {
                    return None;
                }
                let base = self.eval(e.named_child(0)?, d)?;
                let slice = slice_of(base.strip_prefix("T:")?)?;
                mk("T:", &slice)
            }
            _ => None,
        }
    }

    /// Type hint of the first argument of the call whose callee name is `name` (Rust,
    /// calls named like a std trait method several impls may provide: `Bytes::from(x)`),
    /// for picking the impl by argument type. Also reads a single-identifier argument
    /// in a macro token tree (`assert_eq!(Vec::from(b), ..)`).
    pub(crate) fn arg_hint(&mut self, name: Node<'a>) -> Option<String> {
        if self.lang != Lang::Rust || !self.hints_enabled() {
            return None;
        }
        let nav = self.nav;
        let parent = nav.parent(name)?;
        if parent.kind() == "token_tree" {
            let args = nav.next_sibling(name)?;
            let inner = named(args);
            if args.kind() != "token_tree" || !self.t(args).starts_with('(') || inner.len() != 1 {
                return None;
            }
            return self.eval(inner[0], 0).filter(|h| h != "L");
        }
        let mut call = parent;
        for _ in 0..3 {
            if call.kind() == "call_expression" {
                break;
            }
            call = nav.parent(call)?;
        }
        if call.kind() != "call_expression" {
            return None;
        }
        let arg = call.child_by_field_name("arguments")?.named_child(0)?;
        self.eval(arg, 0).filter(|h| h != "L")
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

    /// `cls=X` of a Python decorator call -> `T:X`.
    fn cls_argument(&self, call: Node<'a>) -> Option<String> {
        let args = call.child_by_field_name("arguments")?;
        let mut c = args.walk();
        let kw = args.named_children(&mut c).find(|a| {
            a.kind() == "keyword_argument"
                && a.child_by_field_name("name")
                    .is_some_and(|n| self.t(n) == "cls")
        })?;
        let v = kw.child_by_field_name("value")?;
        matches!(v.kind(), "identifier" | "attribute").then(|| format!("T:{}", self.t(v)))
    }

    fn eval_call(&mut self, e: Node<'a>, d: u8) -> Option<String> {
        let mut f = e.child_by_field_name("function")?;
        // Python `cast(T, x)` / `t.cast(T, x)`: the result is `T`.
        if self.lang == Lang::Python {
            let name = self.t(f);
            if matches!(name, "cast" | "t.cast" | "typing.cast") {
                let ty = e.child_by_field_name("arguments")?.named_child(0)?;
                if ty.kind() != "keyword_argument" {
                    return mk("T:", &squash(self.t(ty)));
                }
            }
        }
        if f.kind() == "generic_function" {
            f = f.child_by_field_name("function")?;
        }
        // Go `F[T](..)` / `pkg.F[T](..)`: an explicitly instantiated generic function.
        if self.lang == Lang::Go && f.kind() == "index_expression" {
            f = f.child_by_field_name("operand")?;
        }
        match f.kind() {
            "identifier" => {
                let name = self.t(f).to_string();
                let dcl = self.lookup(&name, f);
                if self.lang == Lang::Go && matches!(name.as_str(), "new" | "make") && dcl.is_none()
                {
                    let arg = e.child_by_field_name("arguments")?.named_child(0)?;
                    return mk("T:", &squash(self.t(arg)));
                }
                match dcl {
                    // Calling a function-typed variable.
                    Some(d) if !d.func => None,
                    // Go: calling a local closure (`do := func() *X {..}`): its result.
                    Some(d) if self.lang == Lang::Go && d.value.is_some() => {
                        let lit = d.value.filter(|v| v.kind() == "func_literal")?;
                        self.ret_of(lit)
                    }
                    // Rust `black_box(x)` is `x`.
                    None if self.lang == Lang::Rust && name == "black_box" => {
                        let arg = e.child_by_field_name("arguments")?.named_child(0)?;
                        self.eval(arg, d)
                    }
                    _ => mk("C:", &format!("{name}{}", self.arg_shape(e))),
                }
            }
            "scoped_identifier" => {
                let path = squash(&strip_generics(self.t(f)));
                // `Arc::new(x)`, `Box::new(x)`: smart pointers deref to `x`'s type.
                // `ManuallyDrop::new(x)` / `into_inner(x)` and `black_box(x)` pass `x` through.
                let wrapper = path.rsplit_once("::").is_some_and(|(q, n)| {
                    let q = q.rsplit("::").next().unwrap_or(q);
                    (n == "new"
                        && matches!(
                            q,
                            "Arc"
                                | "Rc"
                                | "Box"
                                | "RefCell"
                                | "Mutex"
                                | "RwLock"
                                | "Cell"
                                | "ManuallyDrop"
                        ))
                        || (q == "ManuallyDrop" && n == "into_inner")
                        || n == "black_box"
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
                // Python `pkg.mod.f()`: a dotted module path whose root is not a local.
                if self.lang == Lang::Python && obj.kind() == "attribute" {
                    let path = self.t(obj);
                    let root = path.split('.').next().unwrap_or(path);
                    let plain = path
                        .chars()
                        .all(|c| c.is_alphanumeric() || c == '_' || c == '.');
                    if plain && self.lookup(root, obj).is_none() {
                        return mk("C:", &format!("{path}.{fname}"));
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

    /// JS/TS narrowing of variable `name` (declared at byte `at`) at `e`: inside the
    /// consequence of the innermost `if (G(name))` / `if (name instanceof C)` (also in
    /// `&&` chains and `?:`) after the declaration.
    fn narrowed(&mut self, e: Node<'a>, name: &str, at: usize) -> Option<Narrow> {
        let nav = self.nav;
        let mut child = e;
        let mut cur = nav.parent(e);
        while let Some(p) = cur {
            // A callback keeps the narrowing of a variable declared outside it (TypeScript
            // does so for variables never reassigned); one declared inside ends the walk.
            if self.is_function(p) && at >= p.start_byte() {
                return None;
            }
            let (cond, cons) = match p.kind() {
                "if_statement" | "ternary_expression" => (
                    p.child_by_field_name("condition"),
                    p.child_by_field_name("consequence"),
                ),
                _ => (None, None),
            };
            if let (Some(cond), Some(cons)) = (cond, cons) {
                if cons.id() == child.id() && p.start_byte() >= at {
                    if let Some(n) = self.guard(cond, name, 0) {
                        return Some(n);
                    }
                }
            }
            child = p;
            cur = nav.parent(p);
        }
        None
    }

    /// The narrowing a condition proves for `name`.
    fn guard(&mut self, c: Node<'a>, name: &str, depth: u8) -> Option<Narrow> {
        if depth > MAX_DEPTH {
            return None;
        }
        match c.kind() {
            "parenthesized_expression" => self.guard(c.named_child(0)?, name, depth + 1),
            "call_expression" => {
                let arg = c.child_by_field_name("arguments")?.named_child(0)?;
                if arg.kind() != "identifier" || self.t(arg) != name {
                    return None;
                }
                let f = c.child_by_field_name("function")?;
                let callee = match f.kind() {
                    "identifier" => {
                        let g = self.t(f);
                        // A local variable holding a function is no module-level guard.
                        if self.lookup_local(g, f).is_some_and(|d| !d.func) {
                            return None;
                        }
                        g.to_string()
                    }
                    "member_expression" => self.bare_path(f, 0)?,
                    _ => return None,
                };
                Some(Narrow::Guard(callee))
            }
            "binary_expression" => {
                let op = c.child_by_field_name("operator").map(|o| self.t(o))?;
                let l = c.child_by_field_name("left")?;
                let r = c.child_by_field_name("right")?;
                match op {
                    "instanceof" if l.kind() == "identifier" && self.t(l) == name => {
                        matches!(r.kind(), "identifier" | "member_expression")
                            .then(|| Narrow::Type(squash(self.t(r))))
                    }
                    "&&" => self
                        .guard(l, name, depth + 1)
                        .or_else(|| self.guard(r, name, depth + 1)),
                    _ => None,
                }
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
    /// Python `def` without a return annotation: the hint its `return` statements agree
    /// on (nested functions and classes excluded), if every one evaluates to it.
    pub(crate) fn py_returned(&mut self, def: Node<'a>) -> Option<String> {
        if self.lang != Lang::Python || def.child_by_field_name("return_type").is_some() {
            return None;
        }
        let body = def.child_by_field_name("body")?;
        let mut returns = Vec::new();
        let mut stack = vec![body];
        while let Some(n) = stack.pop() {
            match n.kind() {
                "function_definition" | "class_definition" | "lambda" => continue,
                "return_statement" => {
                    returns.push(n.named_child(0)?);
                    if returns.len() > 8 {
                        return None;
                    }
                    continue;
                }
                _ => {}
            }
            let mut c = n.walk();
            stack.extend(n.named_children(&mut c));
        }
        let mut out: Option<String> = None;
        for r in returns {
            let h = self.eval(r, 0)?;
            if h == "L" {
                return None;
            }
            match &out {
                Some(o) if *o != h => return None,
                Some(_) => {}
                None => out = Some(h),
            }
        }
        out
    }

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
                if let Some(r) = def.child_by_field_name("return_type") {
                    r.named_child(0).unwrap_or(r)
                } else {
                    // Unannotated, returning an object literal: its methods.
                    let f = if def.kind() == "variable_declarator" {
                        def.child_by_field_name("value")?
                    } else {
                        def
                    };
                    return self.returned_object(f).and_then(object_hint);
                }
            }
            Lang::Python => def.child_by_field_name("return_type")?,
            Lang::C | Lang::Cpp => return None,
        };
        mk("T:", &squash(self.t(r)))
    }

    /// Go: type hint of package-level var/const `name` declared by `spec` (a `var_spec`
    /// or `const_spec`): its written type, else its initializer's hint. A var bound to a
    /// function (`var NewX = NewBase[int, cfg]`) gets `C:<function>`, so calling the var
    /// evaluates to the function's result.
    pub(crate) fn var_ret_of(&mut self, spec: Node<'a>, name: &str) -> Option<String> {
        if self.lang != Lang::Go || !self.hints_enabled() {
            return None;
        }
        if let Some(t) = spec.child_by_field_name("type") {
            return mk("T:", &squash(self.t(t)));
        }
        let mut c = spec.walk();
        let i = spec
            .children_by_field_name("name", &mut c)
            .position(|n| self.t(n) == name)?;
        let v = *named(spec.child_by_field_name("value")?).get(i)?;
        let func = match v.kind() {
            // `F[A, B]`
            "type_instantiation_expression" => v.child_by_field_name("type"),
            // `F[A]` (or indexing a collection: `table[k]`)
            "index_expression" => v.child_by_field_name("operand").filter(|o| {
                match o.kind() {
                    "identifier" => match self.lookup(self.t(*o), *o) {
                        Some(d) => d.func,
                        // Not declared in this file: an instantiation when the index is
                        // not a value in scope either.
                        None => v.child_by_field_name("index").is_some_and(|ix| {
                            ix.kind() == "identifier" && self.lookup(self.t(ix), ix).is_none()
                        }),
                    },
                    "selector_expression" => true,
                    _ => false,
                }
            }),
            // `var f = g`
            "identifier" => Some(v).filter(|t| self.lookup(self.t(*t), *t).is_some_and(|d| d.func)),
            _ => None,
        };
        if let Some(f) = func {
            return mk("C:", &squash(self.t(f)));
        }
        self.eval(v, 0).filter(|h| h != "L")
    }

    /// The object literal a JS/TS function returns, when that is its only `return`
    /// (or an arrow's expression body).
    fn returned_object(&self, f: Node<'a>) -> Option<Node<'a>> {
        if !self.is_function(f) {
            return None;
        }
        let body = f.child_by_field_name("body")?;
        let unparen = |n: Node<'a>| {
            let mut n = n;
            while n.kind() == "parenthesized_expression" {
                n = n.named_child(0)?;
            }
            Some(n)
        };
        if body.kind() != "statement_block" {
            return unparen(body).filter(|o| o.kind() == "object");
        }
        let mut rets = Vec::new();
        let mut stack = vec![body];
        while let Some(n) = stack.pop() {
            if n.kind() == "return_statement" {
                rets.push(n);
                if rets.len() > 1 {
                    return None;
                }
                continue;
            }
            if n.id() != body.id() && self.is_function(n) {
                continue;
            }
            stack.extend(named(n));
        }
        let [r] = rets.as_slice() else {
            return None;
        };
        unparen(r.named_child(0)?).filter(|o| o.kind() == "object")
    }

    /// Rust generic parameters in scope of function `def` (its own and its `impl`'s) with a
    /// single meaningful bound, as (`<T>`, `T:<bound>`): stored in the function symbol's
    /// `fields`, so the resolver can bind a field or path typed with a bare generic
    /// (`self.formatter: F`, `R::discard(..)`) to the bound trait.
    pub(crate) fn generic_fields(&self, def: Node<'a>) -> Vec<(String, String)> {
        if self.lang != Lang::Rust {
            return Vec::new();
        }
        self.generics(def)
            .into_iter()
            .filter_map(|(n, b)| Some((format!("<{n}>"), mk("T:", &b?)?)))
            .collect()
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
                let Some(body) = def.child_by_field_name("body") else {
                    return out;
                };
                if body.kind() == "enum_variant_list" {
                    // Variant payloads: `Number(Number)` -> `Number.0`, `Map { ser }` ->
                    // `Map.ser` (read by `F:` hints of match patterns).
                    for v in named(body) {
                        let (Some(vn), Some(vb)) =
                            (v.child_by_field_name("name"), v.child_by_field_name("body"))
                        else {
                            continue;
                        };
                        let prefix = format!("{}.", self.t(vn));
                        self.rust_field_list(vb, &prefix, &mut out);
                    }
                } else {
                    self.rust_field_list(body, "", &mut out);
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

    /// Rust named (`{ a: A }`) or positional (`(A, B)` -> `0`, `1`) fields, names
    /// prefixed with `prefix`.
    fn rust_field_list(&self, body: Node<'a>, prefix: &str, out: &mut Vec<(String, String)>) {
        match body.kind() {
            "field_declaration_list" => {
                for fd in named(body) {
                    if let (Some(n), Some(t)) = (
                        fd.child_by_field_name("name"),
                        fd.child_by_field_name("type"),
                    ) {
                        out.push((
                            format!("{prefix}{}", self.t(n)),
                            format!("T:{}", squash(self.t(t))),
                        ));
                    }
                }
            }
            "ordered_field_declaration_list" => {
                let mut c = body.walk();
                let types: Vec<Node<'_>> = body.children_by_field_name("type", &mut c).collect();
                for (i, t) in types.into_iter().enumerate() {
                    out.push((format!("{prefix}{i}"), format!("T:{}", squash(self.t(t)))));
                }
            }
            _ => {}
        }
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

/// `O:<first line>-<last line>` of an object literal / inline object type.
fn object_hint(o: Node<'_>) -> Option<String> {
    mk(
        "O:",
        &format!(
            "{}-{}",
            o.start_position().row + 1,
            o.end_position().row + 1
        ),
    )
}

/// What a condition proves about a variable (see `Infer::narrowed`).
enum Narrow {
    /// `x instanceof C`: the written type.
    Type(String),
    /// `G(x)`: whatever type guard `G` asserts.
    Guard(String),
}

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
            | "scoped_type_identifier"
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

/// Type of a range index into a (squashed) Rust type: `&[u8;_]` / `Vec<u8>` / `[u8]` ->
/// `[u8]`, `String` / `&str` -> `str`.
fn slice_of(ty: &str) -> Option<String> {
    let mut t = ty.trim();
    loop {
        let before = t;
        t = t.trim_start_matches('&');
        if let Some(rest) = t.strip_prefix('\'') {
            t = rest.trim_start_matches(|c: char| c.is_alphanumeric() || c == '_');
        }
        t = t.trim_start();
        t = t
            .strip_prefix("mut")
            .filter(|r| r.starts_with([' ', '[']))
            .unwrap_or(t);
        t = t.trim_start();
        if t == before {
            break;
        }
    }
    if let Some(inner) = t.strip_prefix('[').and_then(|x| x.strip_suffix(']')) {
        let mut depth = 0i32;
        let mut cut = inner.len();
        for (i, c) in inner.char_indices() {
            match c {
                '<' | '[' | '(' => depth += 1,
                '>' | ']' | ')' => depth -= 1,
                ';' if depth == 0 => {
                    cut = i;
                    break;
                }
                _ => {}
            }
        }
        return Some(format!("[{}]", &inner[..cut]));
    }
    if let Some(arg) = t.strip_prefix("Vec<").and_then(|x| x.strip_suffix('>')) {
        return Some(format!("[{arg}]"));
    }
    matches!(t, "str" | "String").then(|| "str".to_string())
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

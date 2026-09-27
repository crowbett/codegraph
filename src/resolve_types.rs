//! Receiver types: evaluate the extraction-time hints of [`crate::infer`] against the
//! whole repo, so `x.method()` resolves through the type of `x` instead of by name.
//!
//! A hint evaluates to a type symbol in this repo (`Known`), a type provably outside the
//! repo (`External`: stdlib, a dependency, a primitive; the call then resolves to nothing
//! instead of a same-named repo method), or `Unknown` (fall back to name rules).

use std::collections::{HashMap, HashSet};

use super::{Confidence, Ctx, EdgeKind, Lang, SymKind, E};

/// Evaluated receiver type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Ty {
    Known(usize),
    /// Outside the repo. `Some(id)`: a type some in-repo trait is implemented for
    /// (`impl BufMut for Vec<u8>`), an index into `Ctx::ext_names`, so its extension
    /// methods still resolve.
    External(Option<u32>),
    Unknown,
}

/// Outcome of typed receiver resolution.
pub(super) enum Typed {
    /// The receiver's type is unknown: fall back to name rules.
    NoOpinion,
    /// The receiver's type is known; `None` = the call does not land in the repo.
    Resolved(Option<(usize, Confidence)>),
}

/// A type still in text form, with the context needed to bind it.
#[derive(Debug, Clone)]
enum Val {
    Text {
        fi: usize,
        from: Option<i64>,
        text: String,
        unwrap: bool,
        awaited: bool,
        /// An element of the collection type `text`.
        elem: bool,
    },
    Ty(Ty),
}

const MAX_DEPTH: u8 = 10;

/// Rust methods that return (a reference to) their receiver's type.
const PASSTHROUGH: &[&str] = &[
    "clone",
    "as_ref",
    "as_mut",
    "borrow",
    "borrow_mut",
    "to_owned",
    "deref",
    "deref_mut",
    "lock",
    "read",
    "write",
    "get_ref",
    "get_mut",
    "by_ref",
];
/// Rust associated functions that construct `Self` (derived or std-trait impls).
const SELF_CTORS: &[&str] = &[
    "default",
    "new",
    "from",
    "try_from",
    "from_str",
    "clone",
    "with_capacity",
];
/// `T`, `K2`, `Fut`-less short all-caps names: likely a generic type parameter.
/// Types a Python decorator's result may give the decorated name, from the decorator's
/// type text: union members, with `Callable[[..], R]` replaced by `R` (recursively), so
/// `Group | Callable[[F], Group | GrpType]` (usable with or without arguments) gives
/// `Group` and `GrpType`. The caller keeps the one that binds to a repo type.
fn decorated_results(text: &str, out: &mut Vec<String>, depth: u8) {
    fn bare(t: &str) -> &str {
        let t = t.trim();
        t.strip_prefix("typing.")
            .or_else(|| t.strip_prefix("t."))
            .or_else(|| t.strip_prefix("collections.abc."))
            .unwrap_or(t)
    }
    if depth > 4 {
        return;
    }
    let t = bare(text);
    let members: Vec<&str> = match t.strip_prefix("Union[").and_then(|u| u.strip_suffix(']')) {
        Some(inner) => split_top(inner, ','),
        None => split_top(t, '|'),
    };
    if members.len() > 1 {
        for m in members {
            decorated_results(m, out, depth + 1);
        }
        return;
    }
    if t == "None" || t.is_empty() {
        return;
    }
    match t
        .strip_prefix("Callable[")
        .and_then(|c| c.strip_suffix(']'))
    {
        Some(inner) => {
            if let Some(r) = split_top(inner, ',').last() {
                decorated_results(r, out, depth + 1);
            }
        }
        None => {
            if !out.iter().any(|o| o == t) {
                out.push(t.to_string());
            }
        }
    }
}

fn looks_generic(name: &str) -> bool {
    name.len() <= 2
        && name
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
}

/// Method name -> (parameter count, result count) when known.
type MethodSet<'s> = HashMap<&'s str, Option<(usize, usize)>>;

/// Method sets of stdlib interfaces commonly embedded in repo interfaces.
fn std_iface(qualified: &str) -> Option<&'static [(&'static str, (usize, usize))]> {
    Some(match qualified {
        "http.ResponseWriter" => &[
            ("Header", (0, 1)),
            ("Write", (1, 2)),
            ("WriteHeader", (1, 0)),
        ],
        "http.Handler" => &[("ServeHTTP", (2, 0))],
        "http.Flusher" => &[("Flush", (0, 0))],
        "http.Hijacker" => &[("Hijack", (0, 3))],
        "io.Reader" => &[("Read", (1, 2))],
        "io.Writer" => &[("Write", (1, 2))],
        "io.Closer" => &[("Close", (0, 1))],
        "io.ReadCloser" => &[("Read", (1, 2)), ("Close", (0, 1))],
        "io.WriteCloser" => &[("Write", (1, 2)), ("Close", (0, 1))],
        "io.ReadWriter" => &[("Read", (1, 2)), ("Write", (1, 2))],
        "io.ReadWriteCloser" => &[("Read", (1, 2)), ("Write", (1, 2)), ("Close", (0, 1))],
        "io.StringWriter" => &[("WriteString", (1, 2))],
        "fmt.Stringer" => &[("String", (0, 1))],
        "error" => &[("Error", (0, 1))],
        "sort.Interface" => &[("Len", (0, 1)), ("Less", (2, 1)), ("Swap", (2, 0))],
        "context.Context" => &[
            ("Deadline", (0, 2)),
            ("Done", (0, 1)),
            ("Err", (0, 1)),
            ("Value", (1, 1)),
        ],
        _ => return None,
    })
}

/// Count of top-level comma-separated items in `s` (empty = 0).
fn arity(s: &str) -> usize {
    if s.trim().is_empty() {
        0
    } else {
        split_top(s, ',').len()
    }
}

/// (parameters, results) of a one-line Go method signature: `func (r *T) Name(a, b int)
/// (int, error) {` or an interface element `Name(ctx context.Context) error`.
fn go_shape(sig: &str, name: &str) -> Option<(usize, usize)> {
    let at = sig.find(&format!("{name}("))? + name.len();
    let rest = &sig[at..];
    let mut depth = 0i32;
    let mut close = None;
    for (i, c) in rest.char_indices() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(i);
                    break;
                }
            }
            _ => {}
        }
    }
    let close = close?;
    let params = arity(&rest[1..close]);
    let tail = rest[close + 1..].trim();
    // Cut the body (`{`, `{}`, `{ return x }`): a brace at depth 0 after a space or at
    // the start (`interface{}` in a result type has none).
    let mut depth = 0i32;
    let mut cut = tail.len();
    let mut prev = ' ';
    for (i, c) in tail.char_indices() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth -= 1,
            '{' if depth == 0 && prev == ' ' => {
                cut = i;
                break;
            }
            _ => {}
        }
        prev = c;
    }
    let tail = tail[..cut].trim();
    let results = if tail.is_empty() {
        0
    } else if let Some(inner) = tail.strip_prefix('(').and_then(|t| t.strip_suffix(')')) {
        arity(inner)
    } else {
        1
    };
    Some((params, results))
}

/// Rust iterator adaptors whose items are the receiver's items.
const RUST_ITER_SAME: &[&str] = &[
    "iter",
    "iter_mut",
    "into_iter",
    "filter",
    "skip",
    "take",
    "rev",
    "peekable",
    "chain",
    "cloned",
    "copied",
    "by_ref",
    "skip_while",
    "take_while",
    "step_by",
    "fuse",
    "inspect",
    "drain",
    "as_slice",
];
/// TS/JS array methods returning an array of the same elements.
const TS_ARRAY_SAME: &[&str] = &[
    "filter",
    "slice",
    "concat",
    "sort",
    "reverse",
    "toSorted",
    "toReversed",
    "values",
];

/// `Result<T, E>` / `Option<T>` (behind references) -> `T`; anything else unchanged.
fn unwrap_text(lang: Lang, text: &str) -> String {
    if lang != Lang::Rust {
        return text.to_string();
    }
    let t = text.trim().trim_start_matches('&');
    let (head, arg) = outer_generic(t, '<', '>');
    match (head.rsplit("::").next().unwrap_or(head), arg) {
        ("Result" | "Option", Some(a)) => a.to_string(),
        _ => text.to_string(),
    }
}

/// Element type of a collection type text (`Vec<T>`, `[]T`, `T[]`, `list[T]`,
/// `Option<T>`), or `None` when unknown.
fn element_of(lang: Lang, text: &str) -> Option<String> {
    let t = text.trim();
    match lang {
        Lang::Rust => {
            let mut t = t;
            loop {
                let before = t;
                t = t.trim_start_matches('&').trim_start();
                if t.starts_with('\'') {
                    t = t.find(' ').map_or("", |i| &t[i + 1..]);
                }
                t = t.strip_prefix("mut ").unwrap_or(t);
                if t == before {
                    break;
                }
            }
            if let Some(inner) = t.strip_prefix('[').and_then(|x| x.strip_suffix(']')) {
                return Some(split_top(inner, ';')[0].to_string());
            }
            let (head, arg) = outer_generic(t, '<', '>');
            let last = head.rsplit("::").next().unwrap_or(head);
            match last {
                "Vec" | "VecDeque" | "HashSet" | "BTreeSet" | "BinaryHeap" | "LinkedList"
                | "Option" | "Result" | "IntoIter" | "Iter" => arg.map(str::to_string),
                "Box" | "Rc" | "Arc" => arg.and_then(|a| element_of(lang, a)),
                _ => {
                    // `impl Iterator<Item = T>`
                    let i = t.find("Item=")?;
                    let rest = &t[i + 5..];
                    Some(split_top(rest.trim_end_matches('>'), ',')[0].to_string())
                }
            }
        }
        Lang::Go => {
            let t = t.trim_start_matches('*');
            if let Some(rest) = t.strip_prefix("[]") {
                return Some(rest.to_string());
            }
            if let Some(rest) = t.strip_prefix("chan ") {
                return Some(rest.to_string());
            }
            if t.starts_with("map[") {
                let (_, v) = t.split_at(t.find(']')? + 1);
                return Some(v.to_string());
            }
            if t.starts_with('[') {
                return Some(t[t.find(']')? + 1..].to_string());
            }
            None
        }
        Lang::TypeScript | Lang::Tsx | Lang::JavaScript => {
            let t = t.strip_prefix("readonly ").unwrap_or(t);
            if let Some(inner) = t.strip_suffix("[]") {
                return Some(inner.trim_matches(['(', ')']).to_string());
            }
            let (head, arg) = outer_generic(t, '<', '>');
            matches!(
                head,
                "Array"
                    | "ReadonlyArray"
                    | "Set"
                    | "ReadonlySet"
                    | "Iterable"
                    | "IterableIterator"
                    | "Generator"
                    | "AsyncIterable"
            )
            .then(|| arg.map(str::to_string))
            .flatten()
        }
        Lang::Python => {
            let (head, arg) = outer_generic(t, '[', ']');
            let last = head.rsplit('.').next().unwrap_or(head);
            matches!(
                last,
                "list"
                    | "List"
                    | "set"
                    | "Set"
                    | "frozenset"
                    | "FrozenSet"
                    | "Iterable"
                    | "Iterator"
                    | "Sequence"
                    | "Collection"
                    | "Generator"
                    | "dict"
                    | "Dict"
                    | "Mapping"
                    | "tuple"
                    | "Tuple"
            )
            .then(|| arg.map(str::to_string))
            .flatten()
        }
        Lang::C | Lang::Cpp => None,
    }
}

/// Methods that unwrap an `Option`/`Result`.
const UNWRAP: &[&str] = &[
    "unwrap",
    "expect",
    "unwrap_or_default",
    "unwrap_or",
    "unwrap_or_else",
    "unwrap_unchecked",
];

fn builtin_type(lang: Lang, name: &str) -> bool {
    let list: &[&str] = match lang {
        Lang::Go => &[
            "string",
            "int",
            "int8",
            "int16",
            "int32",
            "int64",
            "uint",
            "uint8",
            "uint16",
            "uint32",
            "uint64",
            "uintptr",
            "byte",
            "rune",
            "bool",
            "float32",
            "float64",
            "complex64",
            "complex128",
            "error",
            "any",
        ],
        Lang::Rust => &[
            "str",
            "String",
            "Vec",
            "HashMap",
            "HashSet",
            "BTreeMap",
            "BTreeSet",
            "VecDeque",
            "Path",
            "PathBuf",
            "OsStr",
            "OsString",
            "File",
            "Duration",
            "Instant",
            "SystemTime",
            "u8",
            "u16",
            "u32",
            "u64",
            "u128",
            "usize",
            "i8",
            "i16",
            "i32",
            "i64",
            "i128",
            "isize",
            "f32",
            "f64",
            "bool",
            "char",
            "Mutex",
            "RwLock",
            "Cell",
            "RefCell",
            "Sender",
            "Receiver",
            "Formatter",
            "Error",
            "Value",
        ],
        Lang::TypeScript | Lang::Tsx | Lang::JavaScript => &[
            "string",
            "number",
            "boolean",
            "bigint",
            "symbol",
            "object",
            "unknown",
            "any",
            "void",
            "never",
            "Map",
            "Set",
            "WeakMap",
            "WeakSet",
            "Array",
            "ReadonlyArray",
            "Record",
            "Partial",
            "Readonly",
            "Required",
            "Date",
            "RegExp",
            "Error",
            "URL",
            "URLSearchParams",
            "Headers",
            "Request",
            "Response",
            "Uint8Array",
            "ArrayBuffer",
            "HTMLElement",
            "Element",
            "Document",
            "Window",
            "Event",
            "AbortController",
            "AbortSignal",
            "TextEncoder",
            "TextDecoder",
            "Worker",
            "Buffer",
            "Function",
        ],
        Lang::Python => &[
            "str",
            "int",
            "float",
            "bool",
            "bytes",
            "dict",
            "list",
            "set",
            "tuple",
            "frozenset",
            "object",
            "Any",
            "Dict",
            "List",
            "Set",
            "Tuple",
            "Path",
            "Decimal",
            "datetime",
            "date",
            "Connection",
            "Cursor",
        ],
        Lang::C | Lang::Cpp => &[],
    };
    list.contains(&name)
}

/// Split `Name<A, B>` / `Name[A]` into (`Name`, first argument).
fn outer_generic(s: &str, open: char, close: char) -> (&str, Option<&str>) {
    let Some(i) = s.find(open) else {
        return (s, None);
    };
    let head = &s[..i];
    let body = &s[i + 1..];
    let mut depth = 0i32;
    for (j, c) in body.char_indices() {
        match c {
            c if c == open || c == '(' || c == '[' || c == '<' => depth += 1,
            c if (c == close || c == ')' || c == ']' || c == '>') && depth > 0 => depth -= 1,
            c if (c == ',' || c == close) && depth == 0 => return (head, Some(&body[..j])),
            _ => {}
        }
    }
    (head, None)
}

/// Split a top-level `sep` (outside brackets); returns the parts.
fn split_top(s: &str, sep: char) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;
    for (i, c) in s.char_indices() {
        match c {
            '<' | '(' | '[' | '{' => depth += 1,
            '>' | ')' | ']' | '}' => depth -= 1,
            c if c == sep && depth == 0 => {
                out.push(&s[start..i]);
                start = i + c.len_utf8();
            }
            _ => {}
        }
    }
    out.push(&s[start..]);
    out
}

/// Normalized type reference: optional qualifier (module path) + simple name.
enum Norm {
    Name(Option<String>, String),
    SelfType,
    External,
    Unknown,
}

fn normalize(lang: Lang, text: &str, unwrap: bool, awaited: bool, depth: u8) -> Norm {
    if depth > 6 || text.is_empty() {
        return Norm::Unknown;
    }
    let d = depth + 1;
    match lang {
        Lang::Rust => {
            let mut t = text.trim();
            loop {
                let before = t;
                t = t.trim_start_matches('&').trim_start();
                if t.starts_with('\'') {
                    // lifetime: `'a Repo`
                    t = t.find(' ').map_or("", |i| &t[i + 1..]);
                }
                for kw in ["mut ", "dyn ", "impl "] {
                    t = t.strip_prefix(kw).unwrap_or(t);
                }
                if t == before {
                    break;
                }
            }
            let t = split_top(t, '+').first().copied().unwrap_or(t);
            if t.starts_with('(')
                || t.starts_with('[')
                || t.starts_with("fn(")
                || t.starts_with('*')
            {
                return Norm::Unknown;
            }
            let (head, arg) = outer_generic(t, '<', '>');
            let last = head.rsplit("::").next().unwrap_or(head);
            match last {
                "Box" | "Arc" | "Rc" | "Cow" | "Pin" | "ManuallyDrop" | "Mutex" | "RwLock"
                | "RefCell" | "Cell" => {
                    arg.map_or(Norm::Unknown, |a| normalize(lang, a, unwrap, awaited, d))
                }
                "Option" | "Result" if unwrap => {
                    arg.map_or(Norm::Unknown, |a| normalize(lang, a, false, awaited, d))
                }
                "Option" | "Result" => Norm::External,
                "Self" => Norm::SelfType,
                _ => {
                    let qual = head.rfind("::").map(|i| head[..i].to_string());
                    Norm::Name(qual, last.to_string())
                }
            }
        }
        Lang::Go => {
            let t = text.trim().trim_start_matches('*');
            if t.starts_with('[')
                || t.starts_with("map[")
                || t.starts_with("chan")
                || t.starts_with("<-")
                || t.starts_with("func")
            {
                return Norm::External;
            }
            if t.starts_with("interface") || t.starts_with("struct") {
                return Norm::Unknown;
            }
            let (head, _) = outer_generic(t, '[', ']');
            match head.rsplit_once('.') {
                Some((q, n)) => Norm::Name(Some(q.to_string()), n.to_string()),
                None => Norm::Name(None, head.to_string()),
            }
        }
        Lang::TypeScript | Lang::Tsx | Lang::JavaScript => {
            let t = text.trim();
            let t = t.strip_prefix("readonly ").unwrap_or(t);
            let parts: Vec<&str> = split_top(t, '|')
                .into_iter()
                .map(str::trim)
                .filter(|p| !matches!(*p, "" | "null" | "undefined"))
                .collect();
            if parts.len() != 1 {
                return Norm::Unknown;
            }
            let t = parts[0];
            if t.ends_with("[]") || t.starts_with('{') || t.starts_with('(') || t.starts_with('"') {
                return Norm::External;
            }
            let (head, arg) = outer_generic(t, '<', '>');
            if head == "Promise" || head == "PromiseLike" {
                return if awaited {
                    arg.map_or(Norm::Unknown, |a| normalize(lang, a, unwrap, false, d))
                } else {
                    Norm::External
                };
            }
            match head.rsplit_once('.') {
                Some((q, n)) => Norm::Name(Some(q.to_string()), n.to_string()),
                None => Norm::Name(None, head.to_string()),
            }
        }
        Lang::Python => {
            let t = text.trim().trim_matches(['"', '\'']);
            let parts: Vec<&str> = split_top(t, '|')
                .into_iter()
                .map(str::trim)
                .filter(|p| *p != "None")
                .collect();
            if parts.len() != 1 {
                return Norm::Unknown;
            }
            let (head, arg) = outer_generic(parts[0], '[', ']');
            let last = head.rsplit('.').next().unwrap_or(head);
            match last {
                "Optional" => arg.map_or(Norm::Unknown, |a| normalize(lang, a, unwrap, awaited, d)),
                "list" | "dict" | "set" | "tuple" | "List" | "Dict" | "Set" | "Tuple"
                | "Iterator" | "Iterable" | "Sequence" | "Mapping" | "Callable" => Norm::External,
                _ => match head.rsplit_once('.') {
                    Some((q, n)) => Norm::Name(Some(q.to_string()), n.to_string()),
                    None => Norm::Name(None, head.to_string()),
                },
            }
        }
        Lang::C | Lang::Cpp => Norm::Unknown,
    }
}

impl Ctx {
    /// Evaluate a receiver hint in file `fi` (reference inside symbol `from`).
    pub(super) fn eval_hint(&self, fi: usize, from: Option<i64>, hint: &str) -> Ty {
        let key = (fi, from, hint.to_string());
        if let Some(&t) = self
            .memo
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&key)
        {
            return t;
        }
        let v = self.eval_val(fi, from, hint, 0);
        let t = self.bind_val(&v, 0);
        self.memo
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(key, t);
        t
    }

    fn eval_val(&self, fi: usize, from: Option<i64>, h: &str, depth: u8) -> Val {
        if depth > MAX_DEPTH {
            return Val::Ty(Ty::Unknown);
        }
        let d = depth + 1;
        if h == "S" {
            let (ty, _) = self.self_type(from);
            return Val::Ty(match ty {
                Some(name) => self.find_type(fi, None, &name, from),
                None => Ty::Unknown,
            });
        }
        if h == "L" {
            return Val::Ty(Ty::Unknown);
        }
        if let Some(t) = h.strip_prefix("T:") {
            return Val::Text {
                fi,
                from,
                text: t.to_string(),
                unwrap: false,
                awaited: false,
                elem: false,
            };
        }
        if let Some(inner) = h.strip_prefix("E:") {
            return match self.eval_val(fi, from, inner, d) {
                Val::Text {
                    fi,
                    from,
                    text,
                    unwrap,
                    awaited,
                    elem: false,
                } => Val::Text {
                    fi,
                    from,
                    text,
                    unwrap,
                    awaited,
                    elem: true,
                },
                _ => Val::Ty(Ty::Unknown),
            };
        }
        // Python `@deco def f`: `f` is what calling the decorator's value returns.
        if let Some(inner) = h.strip_prefix("D:") {
            return match self.eval_val(fi, from, inner, d) {
                Val::Text {
                    fi,
                    from,
                    text,
                    unwrap: false,
                    awaited: false,
                    elem: false,
                } => {
                    let mut cands = Vec::new();
                    decorated_results(&text, &mut cands, 0);
                    let mut known: Vec<usize> = Vec::new();
                    for text in cands {
                        let v = Val::Text {
                            fi,
                            from,
                            text,
                            unwrap: false,
                            awaited: false,
                            elem: false,
                        };
                        if let Ty::Known(t) = self.bind_val(&v, d) {
                            if !known.contains(&t) {
                                known.push(t);
                            }
                        }
                    }
                    match known.as_slice() {
                        [t] => Val::Ty(Ty::Known(*t)),
                        _ => Val::Ty(Ty::Unknown),
                    }
                }
                _ => Val::Ty(Ty::Unknown),
            };
        }
        if let Some(inner) = h.strip_prefix("Q:").or_else(|| h.strip_prefix("A:")) {
            let is_q = h.starts_with('Q');
            return match self.eval_val(fi, from, inner, d) {
                Val::Text {
                    fi,
                    from,
                    text,
                    unwrap,
                    awaited,
                    elem,
                } => Val::Text {
                    fi,
                    from,
                    text,
                    unwrap: unwrap || is_q,
                    awaited: awaited || !is_q,
                    elem,
                },
                v @ Val::Ty(_) => v,
            };
        }
        if let Some(callee) = h.strip_prefix("C:") {
            return self.eval_call(fi, from, callee, d);
        }
        let (tag, rest) = h.split_at(2.min(h.len()));
        let Some((name, base)) = rest.split_once('@') else {
            return Val::Ty(Ty::Unknown);
        };
        match tag {
            "M:" => {
                let lang = self.files[fi].lang;
                let bv = self.eval_val(fi, from, base, d);
                let bt = self.bind_val(&bv, d);
                // The receiver's own method wins over the name-based shortcuts below
                // (`Db::read`, `Bag.values` are not std's).
                if let Ty::Known(t) = bt {
                    if let Some((m, _)) = self.method_on(t, name, &mut HashSet::new(), d) {
                        return match self.syms[m].ret.clone() {
                            Some(r) => {
                                let mf = self.syms[m].file;
                                self.eval_val(mf, Some(self.syms[m].id), &r, d)
                            }
                            None => Val::Ty(Ty::Unknown),
                        };
                    }
                }
                // An in-repo trait's method on a std type (`vec.put(..)` with
                // `impl BufMut for Vec<u8>`).
                if let Ty::External(Some(id)) = bt {
                    if let Some(m) = self.extension_method(fi, id, name) {
                        return match self.syms[m].ret.clone() {
                            Some(r) => {
                                let mf = self.syms[m].file;
                                self.eval_val(mf, Some(self.syms[m].id), &r, d)
                            }
                            None => Val::Ty(Ty::Unknown),
                        };
                    }
                }
                if lang == Lang::Rust && PASSTHROUGH.contains(&name) {
                    return bv;
                }
                // Iterator adaptors keep the element type: track the collection itself.
                let same_elems = match lang {
                    Lang::Rust => RUST_ITER_SAME.contains(&name),
                    Lang::TypeScript | Lang::Tsx | Lang::JavaScript => {
                        TS_ARRAY_SAME.contains(&name)
                    }
                    _ => false,
                };
                if same_elems && matches!(bv, Val::Text { elem: false, .. }) {
                    return bv;
                }
                if lang == Lang::Rust && UNWRAP.contains(&name) {
                    if let Val::Text {
                        fi,
                        from,
                        text,
                        awaited,
                        elem,
                        ..
                    } = bv
                    {
                        return Val::Text {
                            fi,
                            from,
                            text,
                            unwrap: true,
                            awaited,
                            elem,
                        };
                    }
                    return bv;
                }
                match bt {
                    Ty::Known(_) | Ty::External(_) => Val::Ty(Ty::External(None)),
                    Ty::Unknown => Val::Ty(Ty::Unknown),
                }
            }
            "F:" => match self.bind_val(&self.eval_val(fi, from, base, d), d) {
                Ty::Known(t) => self.field_of(t, name, &mut HashSet::new(), d),
                other => Val::Ty(other),
            },
            _ => Val::Ty(Ty::Unknown),
        }
    }

    fn field_of(&self, t: usize, name: &str, seen: &mut HashSet<usize>, d: u8) -> Val {
        if !seen.insert(t) || d > MAX_DEPTH {
            return Val::Ty(Ty::Unknown);
        }
        if let Some(target) = self.alias_target(t) {
            return self.field_of(target, name, seen, d + 1);
        }
        let s = &self.syms[t];
        if let Some((_, h)) = s.fields.iter().find(|(k, _)| k == name) {
            return self.eval_val(s.file, Some(s.id), h, d + 1);
        }
        // Go embedded fields promote their fields.
        for (k, h) in &s.fields {
            if k.starts_with('~') {
                if let Ty::Known(b) =
                    self.bind_val(&self.eval_val(s.file, Some(s.id), h, d + 1), d + 1)
                {
                    let v = self.field_of(b, name, seen, d + 1);
                    if !matches!(v, Val::Ty(Ty::Unknown)) {
                        return v;
                    }
                }
            }
        }
        Val::Ty(Ty::Unknown)
    }

    fn eval_call(&self, fi: usize, from: Option<i64>, callee: &str, d: u8) -> Val {
        let lang = self.files[fi].lang;
        let (qual, name) = if let Some((q, n)) = callee.rsplit_once("::") {
            (Some(q), n)
        } else if let Some((q, n)) = callee.rsplit_once('.') {
            (Some(q), n)
        } else {
            (None, callee)
        };
        let fake = E {
            id: 0,
            file: fi,
            from_symbol: from,
            from_name: None,
            name: name.to_string(),
            qualifier: qual.map(str::to_string),
            kind: EdgeKind::Call,
            extra: None,
            old_to: None,
            old_conf: None,
            old_to_file: None,
            hint: None,
            dot: false,
            line: 0,
        };
        if let Some((s, _)) = self.resolve_edge(&fake, &self.scopes[fi]) {
            let sym = &self.syms[s];
            if sym.kind.is_type_like() {
                return Val::Ty(Ty::Known(s));
            }
            return match sym.ret.clone() {
                Some(r) => self.eval_val(sym.file, Some(sym.id), &r, d),
                None => Val::Ty(Ty::Unknown),
            };
        }
        // `Type::default()` / `Type::from(..)` from a derive or a std trait impl: no symbol,
        // but it returns the type itself.
        if let (Lang::Rust, Some(q)) = (lang, qual) {
            if SELF_CTORS.contains(&name) {
                let (tq, tn) = match q.rsplit_once("::") {
                    Some((a, b)) => (Some(a), b),
                    None => (None, q),
                };
                if let Ty::Known(t) = self.find_type(fi, tq, tn, from) {
                    return Val::Ty(Ty::Known(t));
                }
            }
        }
        // Nothing in the repo: a call into a module bound outside the repo, or a
        // constructor of a type we don't have, returns an external type.
        let scope = &self.scopes[fi];
        let external = match qual {
            Some(q) => {
                let head = q.split("::").next().unwrap_or(q);
                let head = head.split('.').next().unwrap_or(head);
                scope.modules.get(head).is_some_and(Vec::is_empty)
                    || scope.names.get(head).is_some_and(|(_, t)| t.is_empty())
                    || (lang == Lang::Rust
                        && head.chars().next().is_some_and(char::is_uppercase)
                        && !self.any_type_named(head))
                    || (lang == Lang::Rust && matches!(head, "std" | "core" | "alloc"))
            }
            None => scope.names.get(name).is_some_and(|(_, t)| t.is_empty()),
        };
        // `Vec::with_capacity(..)`, `Vec::new()`: the external type itself.
        let ext = match (lang, qual) {
            (Lang::Rust, Some(q)) if SELF_CTORS.contains(&name) || name == "with_capacity" => {
                self.ext_id(lang, q.rsplit("::").next().unwrap_or(q))
            }
            _ => None,
        };
        Val::Ty(if external {
            Ty::External(ext)
        } else {
            Ty::Unknown
        })
    }

    fn any_type_named(&self, name: &str) -> bool {
        self.by_name
            .get(name)
            .is_some_and(|v| v.iter().any(|&s| self.syms[s].kind.is_type_like()))
    }

    fn bind_val(&self, v: &Val, d: u8) -> Ty {
        match v {
            Val::Ty(t) => *t,
            Val::Text {
                fi,
                from,
                text,
                unwrap,
                awaited,
                elem,
            } => {
                let lang = self.files[*fi].lang;
                let owned;
                let unwrapped;
                let text = if *elem && *unwrap {
                    // `for x in f()?`: unwrap the Result first, then take elements.
                    unwrapped = unwrap_text(lang, text);
                    unwrapped.as_str()
                } else {
                    text
                };
                let text = if *elem {
                    match element_of(lang, text) {
                        Some(t) => {
                            owned = t;
                            &owned
                        }
                        None => return Ty::Unknown,
                    }
                } else {
                    text
                };
                match normalize(lang, text, *unwrap, *awaited, 0) {
                    Norm::External => Ty::External(None),
                    Norm::Unknown => Ty::Unknown,
                    Norm::SelfType => match self.self_type(*from).0 {
                        Some(n) => self.find_type(*fi, None, &n, *from),
                        None => Ty::Unknown,
                    },
                    Norm::Name(q, n) => {
                        let _ = d;
                        self.find_type(*fi, q.as_deref(), &n, *from)
                    }
                }
            }
        }
    }

    /// Type symbols named `name` defined in `files`, or re-exported by them.
    fn type_syms_via(&self, files: &[usize], name: &str) -> Vec<usize> {
        let direct = self.type_syms_in(files, name);
        if !direct.is_empty() {
            return direct;
        }
        let via: Vec<usize> = self
            .lookup_in(files, name, EdgeKind::Ref, 2)
            .into_iter()
            .filter(|&s| self.syms[s].kind.is_type_like())
            .collect();
        if !via.is_empty() {
            return via;
        }
        self.reexported(files, name, EdgeKind::Ref)
            .into_iter()
            .filter(|&s| self.syms[s].kind.is_type_like())
            .collect()
    }

    fn type_syms_in(&self, files: &[usize], name: &str) -> Vec<usize> {
        self.by_name
            .get(name)
            .into_iter()
            .flatten()
            .copied()
            .filter(|&s| self.syms[s].kind.is_type_like() && files.contains(&self.syms[s].file))
            .collect()
    }

    /// Bind a type name as seen from file `fi`.
    fn find_type(&self, fi: usize, qual: Option<&str>, name: &str, from: Option<i64>) -> Ty {
        let f = &self.files[fi];
        let lang = f.lang;
        let scope = &self.scopes[fi];
        let _ = from;
        if let Some(q) = qual {
            if lang == Lang::Rust {
                return match self.rust_path(fi, &format!("{q}::{name}")) {
                    Some((files, Some(item))) => {
                        self.pick_type(fi, &self.type_syms_in(&files, &item))
                    }
                    Some((files, None)) => self.pick_type(fi, &self.type_syms_in(&files, name)),
                    None => {
                        let head = q.split("::").next().unwrap_or(q);
                        let bound = scope
                            .modules
                            .get(head)
                            .or_else(|| scope.names.get(head).map(|(_, t)| t));
                        match bound {
                            Some(t) if t.is_empty() => Ty::External(None),
                            _ if matches!(head, "std" | "core" | "alloc") => Ty::External(None),
                            _ => Ty::Unknown,
                        }
                    }
                };
            }
            return match scope.modules.get(q) {
                Some(t) if t.is_empty() => Ty::External(None),
                // `click.Group` where `click/__init__.py` re-exports `Group`.
                Some(t) => self.pick_type(fi, &self.type_syms_via(t, name)),
                None => {
                    if lang == Lang::Python {
                        if let Some(files) = self.py_module(fi, q) {
                            return self.pick_type(fi, &self.type_syms_in(&files, name));
                        }
                    }
                    Ty::Unknown
                }
            };
        }
        // Same file.
        let local = self.type_syms_in(&[fi], name);
        if !local.is_empty() {
            return self.pick_type(fi, &local);
        }
        if let Some((orig, targets)) = scope.names.get(name) {
            if targets.is_empty() {
                return Ty::External(None);
            }
            let m = self.type_syms_in(targets, orig);
            let m = if m.is_empty() {
                self.reexported(targets, orig, EdgeKind::Ref)
                    .into_iter()
                    .filter(|&s| self.syms[s].kind.is_type_like())
                    .collect()
            } else {
                m
            };
            return self.pick_type(fi, &m);
        }
        if let Some(targets) = scope.defaults.get(name) {
            if let Some(s) = self.default_export(targets) {
                if self.syms[s].kind.is_type_like() {
                    return Ty::Known(s);
                }
            }
        }
        let all: Vec<usize> = self
            .by_name
            .get(name)
            .into_iter()
            .flatten()
            .copied()
            .filter(|&s| {
                self.syms[s].kind.is_type_like() && {
                    let o = self.files[self.syms[s].file].lang;
                    o == lang || (lang.is_js_family() && o.is_js_family())
                }
            })
            .collect();
        let same_unit: Vec<usize> = all
            .iter()
            .copied()
            .filter(|&s| self.files[self.syms[s].file].package == f.package)
            .collect();
        match lang {
            Lang::Go | Lang::Rust if !same_unit.is_empty() => self.pick_type(fi, &same_unit),
            Lang::Rust if !self.type_syms_in(&scope.globs, name).is_empty() => {
                self.pick_type(fi, &self.type_syms_in(&scope.globs, name))
            }
            // A type that exists nowhere in the repo is a builtin, a std/dependency type or
            // a global (Go builtins, `Vec`, `TempDir`, `Map`, `HTMLElement`).
            // `T`, `W`, `F`: a generic parameter we can't see (struct/impl generics), not a
            // std type.
            _ if all.is_empty() && looks_generic(name) => Ty::Unknown,
            _ if all.is_empty() => Ty::External(self.ext_id(lang, name)),
            _ if builtin_type(lang, name) && same_unit.is_empty() => {
                Ty::External(self.ext_id(lang, name))
            }
            _ => Ty::Unknown,
        }
    }

    fn pick_type(&self, fi: usize, cands: &[usize]) -> Ty {
        match self.best(fi, cands) {
            Some(s) => Ty::Known(s),
            None => Ty::Unknown,
        }
    }

    /// Method `name` callable on type symbol `t`: its own methods, then promoted
    /// (Go embedding), trait default and inherited methods. `bool` = found via a base.
    pub(super) fn method_on(
        &self,
        t: usize,
        name: &str,
        seen: &mut HashSet<usize>,
        d: u8,
    ) -> Option<(usize, bool)> {
        if !seen.insert(t) || d > MAX_DEPTH {
            return None;
        }
        let ts = &self.syms[t];
        let tf = &self.files[ts.file];
        let fam = |o: Lang| o == tf.lang || (tf.lang.is_js_family() && o.is_js_family());
        let own: Vec<usize> = self
            .by_member
            .get(&(ts.name.clone(), name.to_string()))
            .into_iter()
            .flatten()
            .copied()
            .filter(|&s| {
                let m = &self.syms[s];
                let mf = &self.files[m.file];
                fam(mf.lang)
                    && match tf.lang {
                        Lang::Go | Lang::Rust => mf.package == tf.package,
                        _ => m.parent == Some(ts.id),
                    }
            })
            .collect();
        // Several types share this name in the unit (`x::Config`, `y::Config`): keep the
        // methods whose qualified name sits under this type's.
        let own = if own.len() > 1 {
            let prefix = format!("{}.", ts.qkey);
            let under: Vec<usize> = own
                .iter()
                .copied()
                .filter(|&m| self.syms[m].qkey.starts_with(&prefix))
                .collect();
            if under.is_empty() {
                own
            } else {
                under
            }
        } else {
            own
        };
        if let Some(b) = self.best(ts.file, &own) {
            return Some((b, false));
        }
        // A type alias (`type BoolFlag = FlagBase[bool]`, `type R = Repo<u8>;`) has the
        // methods of its target.
        if let Some(target) = self.alias_target(t) {
            return self.method_on(target, name, seen, d + 1);
        }
        // Go: promoted through embedded fields.
        for (k, h) in &ts.fields {
            if k.starts_with('~') {
                if let Ty::Known(b) =
                    self.bind_val(&self.eval_val(ts.file, Some(ts.id), h, d + 1), d + 1)
                {
                    if let Some((m, _)) = self.method_on(b, name, seen, d + 1) {
                        return Some((m, true));
                    }
                }
            }
        }
        // Traits / base classes / implemented interfaces.
        for (qual, base) in self.base_quals.get(&ts.name).into_iter().flatten() {
            let Ty::Known(b) = self.find_type(ts.file, qual.as_deref(), base, Some(ts.id)) else {
                continue;
            };
            if let Some((m, _)) = self.method_on(b, name, seen, d + 1) {
                return Some((m, true));
            }
        }
        None
    }

    /// Go implicit interface satisfaction: (type, interface) pairs where the type's
    /// method set (own methods, both receivers, plus methods promoted from embedded
    /// in-repo types) covers the interface's (own plus embedded in-repo interfaces),
    /// comparing names and, when both signatures are on one line, parameter and result
    /// counts. Interfaces embedding something outside the repo are skipped: their method
    /// set is unknown, and a partial set would claim implementations that don't exist.
    pub(super) fn go_implicit_impls(&self) -> Vec<(usize, usize)> {
        let mut methods: HashMap<(&str, &str), MethodSet<'_>> = HashMap::new();
        for s in &self.syms {
            let f = &self.files[s.file];
            if s.kind == SymKind::Method && f.lang == Lang::Go {
                if let Some(c) = &s.container {
                    methods
                        .entry((f.package.as_str(), c.as_str()))
                        .or_default()
                        .insert(s.name.as_str(), go_shape(&s.sig, &s.name));
                }
            }
        }
        let mut ifaces: Vec<(usize, MethodSet<'_>)> = Vec::new();
        for (i, s) in self.syms.iter().enumerate() {
            if s.kind == SymKind::Interface && self.files[s.file].lang == Lang::Go {
                if let Some(set) = self.iface_methods(i, &methods, &mut HashSet::new()) {
                    if !set.is_empty() {
                        ifaces.push((i, set));
                    }
                }
            }
        }
        let mut out = Vec::new();
        if ifaces.is_empty() {
            return out;
        }
        // Method sets of every concrete type, then an inverted index: method name -> types.
        // Each interface only checks the types that have its rarest method, instead of
        // every type (types x interfaces blows up on large Go repos).
        let mut tsets: Vec<(usize, MethodSet<'_>)> = Vec::new();
        for (t, s) in self.syms.iter().enumerate() {
            if !matches!(s.kind, SymKind::Struct | SymKind::Type)
                || self.files[s.file].lang != Lang::Go
            {
                continue;
            }
            let mut mset = MethodSet::new();
            self.type_methods(t, &methods, &mut mset, &mut HashSet::new());
            if !mset.is_empty() {
                tsets.push((t, mset));
            }
        }
        let mut by_method: HashMap<&str, Vec<usize>> = HashMap::new();
        for (k, (_, mset)) in tsets.iter().enumerate() {
            for name in mset.keys() {
                by_method.entry(name).or_default().push(k);
            }
        }
        for (i, iset) in &ifaces {
            let Some(rarest) = iset
                .keys()
                .min_by_key(|n| by_method.get(*n).map_or(0, Vec::len))
            else {
                continue;
            };
            for &k in by_method.get(rarest).into_iter().flatten() {
                let (t, mset) = &tsets[k];
                let covers = iset.iter().all(|(name, ishape)| {
                    mset.get(name).is_some_and(|tshape| match (ishape, tshape) {
                        (Some(a), Some(b)) => a == b,
                        _ => true,
                    })
                });
                if covers {
                    out.push((*t, *i));
                }
            }
        }
        out.sort_unstable();
        out
    }

    fn iface_methods<'s>(
        &'s self,
        i: usize,
        methods: &HashMap<(&'s str, &'s str), MethodSet<'s>>,
        seen: &mut HashSet<usize>,
    ) -> Option<MethodSet<'s>> {
        if !seen.insert(i) {
            return Some(MethodSet::new());
        }
        let s = &self.syms[i];
        let pkg = self.files[s.file].package.as_str();
        let mut set = methods
            .get(&(pkg, s.name.as_str()))
            .cloned()
            .unwrap_or_default();
        for (k, h) in &s.fields {
            if !k.starts_with('~') {
                continue;
            }
            match self.bind_val(&self.eval_val(s.file, Some(s.id), h, 0), 0) {
                Ty::Known(j) if self.syms[j].kind == SymKind::Interface => {
                    set.extend(self.iface_methods(j, methods, seen)?);
                }
                _ => {
                    let std = h.strip_prefix("T:").and_then(std_iface)?;
                    set.extend(std.iter().map(|(n, sh)| (*n, Some(*sh))));
                }
            }
        }
        Some(set)
    }

    fn type_methods<'s>(
        &'s self,
        t: usize,
        methods: &HashMap<(&'s str, &'s str), MethodSet<'s>>,
        out: &mut MethodSet<'s>,
        seen: &mut HashSet<usize>,
    ) {
        if !seen.insert(t) {
            return;
        }
        let s = &self.syms[t];
        let pkg = self.files[s.file].package.as_str();
        if let Some(m) = methods.get(&(pkg, s.name.as_str())) {
            for (k, v) in m {
                out.entry(k).or_insert(*v);
            }
        }
        for (k, h) in &s.fields {
            if k.starts_with('~') {
                if let Ty::Known(b) = self.bind_val(&self.eval_val(s.file, Some(s.id), h, 0), 0) {
                    self.type_methods(b, methods, out, seen);
                } else {
                    // Embedding a stdlib interface promotes its methods.
                    let t = h.strip_prefix("T:").map(|t| t.trim_start_matches('*'));
                    for (n, sh) in t.and_then(std_iface).into_iter().flatten() {
                        out.entry(n).or_insert(Some(*sh));
                    }
                }
            }
        }
    }

    /// The type an alias symbol stands for, read from its signature
    /// (`type A = B[T]`, `type A = B<T>;`).
    fn alias_target(&self, t: usize) -> Option<usize> {
        let s = &self.syms[t];
        if s.kind != SymKind::Type {
            return None;
        }
        let (_, rhs) = s.sig.split_once('=')?;
        let rhs = rhs.trim().trim_end_matches(';').trim();
        if rhs.is_empty() {
            return None;
        }
        let v = Val::Text {
            fi: s.file,
            from: Some(s.id),
            text: rhs.to_string(),
            unwrap: false,
            awaited: false,
            elem: false,
        };
        match self.bind_val(&v, 0) {
            Ty::Known(b) if b != t => Some(b),
            _ => None,
        }
    }

    /// Id of an external type name some in-repo trait is implemented for (Rust only).
    fn ext_id(&self, lang: Lang, name: &str) -> Option<u32> {
        if lang != Lang::Rust {
            return None;
        }
        self.ext_ids.get(name).copied()
    }

    /// Method `name` that an in-repo trait adds to external type `ext_names[id]`, as seen
    /// from file `fi`: the trait must be in scope there (imported, glob-imported or
    /// declared in the file), its impl for the type provides the method or the trait
    /// does (a provided method), and exactly one such method exists.
    pub(super) fn extension_method(&self, fi: usize, id: u32, name: &str) -> Option<usize> {
        let ty = self.ext_names.get(id as usize)?;
        let traits = self.bases.get(ty)?;
        let scope = &self.scopes[fi];
        let mut found: Vec<usize> = Vec::new();
        let mut seen: HashSet<&str> = HashSet::new();
        for tr in traits {
            if !seen.insert(tr.as_str()) {
                continue;
            }
            let trait_syms: Vec<usize> = self
                .by_name
                .get(tr)
                .into_iter()
                .flatten()
                .copied()
                .filter(|&t| self.syms[t].kind == SymKind::Trait)
                .collect();
            let in_scope = scope.names.contains_key(tr.as_str())
                || trait_syms.iter().any(|&t| {
                    let f = self.syms[t].file;
                    f == fi || scope.globs.contains(&f)
                });
            if trait_syms.is_empty() || !in_scope {
                continue;
            }
            let provided = self.by_member.get(&(tr.clone(), name.to_string()));
            if provided.is_none_or(Vec::is_empty) {
                continue;
            }
            match self.by_member.get(&(ty.clone(), name.to_string())) {
                Some(own) if !own.is_empty() => found.extend(own.iter().copied()),
                _ => found.extend(provided.into_iter().flatten().copied()),
            }
        }
        found.sort_unstable();
        found.dedup();
        match found.as_slice() {
            [m] => Some(*m),
            _ => None,
        }
    }

    /// `super().m()` (Python): `m` found through the bases of the class enclosing the
    /// call, never the class itself.
    pub(super) fn resolve_super(&self, e: &E) -> Option<(usize, Confidence)> {
        let mut cur = e.from_symbol.and_then(|id| self.by_id.get(&id).copied());
        let class = loop {
            let i = cur?;
            let s = &self.syms[i];
            if s.kind.is_type_like() {
                break i;
            }
            cur = s.parent.and_then(|p| self.by_id.get(&p).copied());
        };
        let cs = &self.syms[class];
        let mut seen = HashSet::new();
        for (qual, base) in self.base_quals.get(&cs.name).into_iter().flatten() {
            if let Ty::Known(b) = self.find_type(cs.file, qual.as_deref(), base, Some(cs.id)) {
                if let Some((m, _)) = self.method_on(b, &e.name, &mut seen, 0) {
                    return Some((m, Confidence::Exact));
                }
            }
        }
        None
    }

    /// Resolve a receiver call through its type hint.
    pub(super) fn resolve_typed(&self, e: &E) -> Typed {
        let Some(hint) = e.hint.as_deref() else {
            return Typed::NoOpinion;
        };
        if hint == "L" || !matches!(e.kind, EdgeKind::Call | EdgeKind::Value) {
            return Typed::NoOpinion;
        }
        match self.eval_hint(e.file, e.from_symbol, hint) {
            Ty::Unknown => Typed::NoOpinion,
            Ty::External(Some(id)) => Typed::Resolved(
                self.extension_method(e.file, id, &e.name)
                    .map(|m| (m, Confidence::Exact)),
            ),
            Ty::External(None) => Typed::Resolved(None),
            Ty::Known(t) => Typed::Resolved(
                self.method_on(t, &e.name, &mut HashSet::new(), 0)
                    .map(|(m, via_base)| {
                        let c = if via_base {
                            Confidence::Likely
                        } else {
                            Confidence::Exact
                        };
                        (m, c)
                    }),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(n: Norm) -> String {
        match n {
            Norm::Name(q, n) => format!("{}|{n}", q.unwrap_or_default()),
            Norm::SelfType => "Self".into(),
            Norm::External => "ext".into(),
            Norm::Unknown => "?".into(),
        }
    }

    #[test]
    fn rust_types() {
        let n = |t: &str, u: bool| name(normalize(Lang::Rust, t, u, false, 0));
        assert_eq!(n("&mut Box<dyn Backend>", false), "|Backend");
        assert_eq!(n("mutex::Guard", false), "mutex|Guard");
        assert_eq!(n("Result<Self,String>", true), "Self");
        assert_eq!(n("Result<Self,String>", false), "ext");
        assert_eq!(n("&'a Repo", false), "|Repo");
        assert_eq!(n("impl Backend+Send", false), "|Backend");
        assert_eq!(n("crate::a::Store<T>", false), "crate::a|Store");
        assert_eq!(n("Arc<Mutex<Store>>", false), "|Store");
    }

    #[test]
    fn go_shapes() {
        assert_eq!(
            go_shape(
                "func (n *Notifier) Send(ctx context.Context, m Message, sinks []string) error {",
                "Send"
            ),
            Some((3, 1))
        );
        assert_eq!(go_shape("Flush() error", "Flush"), Some((0, 1)));
        assert_eq!(
            go_shape("func (f *flushWriter) Flush() {", "Flush"),
            Some((0, 0))
        );
        assert_eq!(go_shape("Get(a, b int) (int, error)", "Get"), Some((2, 2)));
        assert_eq!(go_shape("func (s *S) Run(", "Run"), None);
        assert_eq!(
            go_shape("func (testPrintLogger) Print(v ...any) {}", "Print"),
            Some((1, 0))
        );
        assert_eq!(
            go_shape("func f() (map[string]interface{}, error) {", "f"),
            Some((0, 2))
        );
        assert_eq!(
            go_shape("func (x T) Get() interface{} { return nil }", "Get"),
            Some((0, 1))
        );
    }

    #[test]
    fn elements() {
        let e = |l, t| element_of(l, t).unwrap_or_default();
        assert_eq!(e(Lang::Rust, "&Vec<Snapshot>"), "Snapshot");
        assert_eq!(e(Lang::Rust, "&[Id]"), "Id");
        assert_eq!(e(Lang::Rust, "Option<&Tree>"), "&Tree");
        assert_eq!(e(Lang::Rust, "implIterator<Item=Entry>"), "Entry");
        assert_eq!(e(Lang::Go, "[]*Rule"), "*Rule");
        assert_eq!(e(Lang::Go, "map[string]*Rule"), "*Rule");
        assert_eq!(e(Lang::TypeScript, "readonly Tool[]"), "Tool");
        assert_eq!(e(Lang::TypeScript, "Array<Tool>"), "Tool");
        assert_eq!(e(Lang::Python, "list[Txn]"), "Txn");
    }

    #[test]
    fn other_types() {
        assert_eq!(
            name(normalize(Lang::Go, "*Store", false, false, 0)),
            "|Store"
        );
        assert_eq!(
            name(normalize(Lang::Go, "http.Handler", false, false, 0)),
            "http|Handler"
        );
        assert_eq!(name(normalize(Lang::Go, "[]Store", false, false, 0)), "ext");
        assert_eq!(
            name(normalize(Lang::TypeScript, "Promise<Foo>", false, true, 0)),
            "|Foo"
        );
        assert_eq!(
            name(normalize(
                Lang::TypeScript,
                "Foo|undefined",
                false,
                false,
                0
            )),
            "|Foo"
        );
        assert_eq!(
            name(normalize(Lang::Python, "Optional[Store]", false, false, 0)),
            "|Store"
        );
    }
}

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
        /// Line the text is seen from (a reference, or the symbol it belongs to): picks
        /// among same-file definitions sharing a name.
        line: u32,
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
/// `t.X` / `typing.X` / `collections.abc.X` -> `X`.
fn strip_typing(t: &str) -> &str {
    let t = t.trim();
    t.strip_prefix("typing.")
        .or_else(|| t.strip_prefix("t."))
        .or_else(|| t.strip_prefix("collections.abc."))
        .or_else(|| t.strip_prefix("cabc."))
        .unwrap_or(t)
}

/// Annotation of parameter `i` in a one-line Python `def` signature
/// (`def f(self, ctx: Context, name: str) -> X:` with `i = 1` -> `Context`).
fn param_annotation(sig: &str, i: usize) -> Option<String> {
    let open = sig.find('(')?;
    let body = &sig[open + 1..];
    let mut depth = 0i32;
    let mut end = None;
    for (k, ch) in body.char_indices() {
        match ch {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' if depth > 0 => depth -= 1,
            ')' => {
                end = Some(k);
                break;
            }
            _ => {}
        }
    }
    let params = split_top(&body[..end?], ',');
    let p = params.get(i)?.trim();
    let (_, ty) = p.split_once(':')?;
    let ty = split_top(ty, '=')[0].trim();
    (!ty.is_empty()).then(|| ty.to_string())
}

/// Item `i` of a tuple type (`tuple[str, Command]`, `Tuple[..]`, Rust `(A, B)`).
fn tuple_item(text: &str, i: usize) -> Option<String> {
    let t = strip_typing(text);
    let inner = t
        .strip_prefix("tuple[")
        .or_else(|| t.strip_prefix("Tuple["))
        .and_then(|x| x.strip_suffix(']'))
        .or_else(|| t.strip_prefix('(').and_then(|x| x.strip_suffix(')')))?;
    let parts = split_top(inner, ',');
    if parts.iter().any(|p| p.trim() == "...") {
        return None;
    }
    parts
        .get(i)
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
}

/// The value type of subscripting a Python collection type.
fn subscript_value(text: &str) -> Option<String> {
    let t = strip_typing(text);
    let (head, args) = t.split_once('[')?;
    let args = split_top(args.strip_suffix(']')?, ',');
    let pick = match head {
        "dict" | "Dict" | "Mapping" | "MutableMapping" | "defaultdict" | "OrderedDict" => 1,
        "list" | "List" | "Sequence" | "MutableSequence" => 0,
        _ => return None,
    };
    args.get(pick)
        .map(|a| a.trim().to_string())
        .filter(|a| !a.is_empty())
}

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
type MethodSet<'s> = HashMap<&'s str, Option<GoShape>>;

/// A Go method's shape: parameter and result counts, and each parameter's type when it is
/// readable (package qualifiers dropped; `None` for what may be a type parameter).
#[derive(Clone, Debug, PartialEq, Eq)]
struct GoShape {
    params: usize,
    results: usize,
    types: Vec<Option<String>>,
}

impl GoShape {
    fn counts((params, results): (usize, usize)) -> Self {
        Self {
            params,
            results,
            types: Vec::new(),
        }
    }

    /// Whether a method of this shape can implement an interface method of shape `iface`:
    /// same counts, and no parameter whose type is known on both sides differs.
    fn fits(&self, iface: &GoShape) -> bool {
        self.params == iface.params
            && self.results == iface.results
            && self
                .types
                .iter()
                .zip(&iface.types)
                .all(|(a, b)| match (a, b) {
                    (Some(a), Some(b)) => a == b,
                    _ => true,
                })
    }
}

/// Parameter types of a Go parameter list (`a, b int, c *pkg.T` -> `int, int, *T`).
fn go_param_types(list: &str) -> Vec<Option<String>> {
    let items: Vec<&str> = split_top(list, ',')
        .into_iter()
        .map(str::trim)
        .filter(|i| !i.is_empty())
        .collect();
    // Named when any item is `name Type`: then a bare item shares the next item's type.
    let named_item = |i: &str| -> Option<String> {
        let (name, ty) = i.split_once(char::is_whitespace)?;
        let ident = name.chars().all(|c| c.is_alphanumeric() || c == '_')
            && !matches!(name, "chan" | "func" | "map" | "struct" | "interface");
        ident.then(|| ty.trim().to_string())
    };
    let named = items.iter().any(|i| named_item(i).is_some());
    let mut out = vec![None; items.len()];
    let mut cur: Option<String> = None;
    for (k, i) in items.iter().enumerate().rev() {
        let ty = if named {
            if let Some(t) = named_item(i) {
                cur = Some(t);
            }
            cur.clone()
        } else {
            Some((*i).to_string())
        };
        out[k] = ty.and_then(|t| go_norm_type(&t));
    }
    out
}

/// A Go type written in a signature, comparable across packages: no spaces, no package
/// qualifiers, `interface{}` as `any`. `None` when it may involve a type parameter.
fn go_norm_type(t: &str) -> Option<String> {
    // A func type may name its parameters or not (`func(ctx context.Context) error` is
    // `func(context.Context) error`): not compared.
    if t.contains("func(") {
        return None;
    }
    let t: String = t.chars().filter(|c| !c.is_whitespace()).collect();
    let t = t.replace("interface{}", "any");
    let b: Vec<char> = t.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < b.len() {
        if b[i].is_alphabetic() || b[i] == '_' {
            let start = i;
            while i < b.len() && (b[i].is_alphanumeric() || b[i] == '_') {
                i += 1;
            }
            let ident: String = b[start..i].iter().collect();
            if b.get(i) == Some(&'.') && b.get(i + 1) != Some(&'.') {
                i += 1; // package qualifier
                continue;
            }
            if ident.len() <= 2 && ident.chars().all(|c| c.is_ascii_uppercase()) {
                return None; // `T`, `K`, `TT`: a type parameter, most likely
            }
            out.push_str(&ident);
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    Some(out)
}

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
        "fmt.GoStringer" => &[("GoString", (0, 1))],
        "flag.Value" => &[("String", (0, 1)), ("Set", (1, 1))],
        "flag.Getter" => &[("String", (0, 1)), ("Set", (1, 1)), ("Get", (0, 1))],
        "encoding.TextMarshaler" => &[("MarshalText", (0, 2))],
        "encoding.TextUnmarshaler" => &[("UnmarshalText", (1, 1))],
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

/// A call signature of shape `sig` (`infer::param_shape`; `0` = no parameters) accepts a
/// first argument of shape `arg` (`f` a function literal, `o` another literal, `None`
/// unknown).
fn shape_ok(arg: Option<char>, sig: char) -> bool {
    match arg {
        None => true,
        Some('f') => matches!(sig, 'f' | 'g'),
        Some(_) => matches!(sig, 'n' | 'g'),
    }
}

/// `<T>(a: A, b: B) => R` / `(a: A) => R` (squashed TS type text) -> (shape of the first
/// parameter, `R`).
fn fn_type_parts(text: &str) -> Option<(char, String)> {
    let mut t = text.trim();
    let mut gens: Vec<String> = Vec::new();
    if t.starts_with('<') {
        let close = matching(t, 0)?;
        gens = split_top(&t[1..close], ',')
            .into_iter()
            .map(|g| {
                let g = g.trim();
                let end = g.find(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$'));
                g[..end.unwrap_or(g.len())].to_string()
            })
            .collect();
        t = t[close + 1..].trim_start();
    }
    if !t.starts_with('(') {
        return None;
    }
    let close = matching(t, 0)?;
    let ret = t[close + 1..].trim_start().strip_prefix("=>")?.trim();
    if ret.is_empty() {
        return None;
    }
    let params = &t[1..close];
    let shape = match split_top(params, ',')
        .into_iter()
        .map(str::trim)
        .find(|p| !p.is_empty() && !p.starts_with("this:") && !p.starts_with("this?:"))
    {
        None => '0',
        Some(p) => {
            let ty = split_once_top(p, ':').map(|(_, ty)| ty);
            crate::infer::param_shape(ty, &gens)
        }
    };
    Some((shape, ret.to_string()))
}

/// Index of the bracket closing the one at byte `open` of `s` (`(`/`<`/`[`/`{`; a `>` of
/// `=>` does not close anything).
fn matching(s: &str, open: usize) -> Option<usize> {
    let b = s.as_bytes();
    let mut depth = 0i32;
    for (i, &c) in b.iter().enumerate().skip(open) {
        match c {
            b'(' | b'<' | b'[' | b'{' => depth += 1,
            b'>' if i > 0 && b[i - 1] == b'=' => {}
            b')' | b'>' | b']' | b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// Split at the first top-level `sep`.
fn split_once_top(s: &str, sep: char) -> Option<(&str, &str)> {
    let parts = split_top(s, sep);
    if parts.len() < 2 {
        return None;
    }
    let head = parts[0];
    Some((head, &s[head.len() + sep.len_utf8()..]))
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
fn go_shape(sig: &str, name: &str) -> Option<GoShape> {
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
    let types = go_param_types(&rest[1..close]);
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
    Some(GoShape {
        params,
        results,
        types,
    })
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
                // Squashed `&mut[T]`.
                t = t.strip_prefix("mut[").map_or(t, |_| &t[3..]);
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

/// A TS primitive type or literal type (`boolean`, `"a"`, `42`, `false`).
fn ts_primitive(t: &str) -> bool {
    matches!(
        t,
        "boolean" | "string" | "number" | "bigint" | "symbol" | "void" | "true" | "false"
    ) || t.starts_with(['"', '\'', '`'])
        || t.chars()
            .next()
            .is_some_and(|c| c.is_ascii_digit() || c == '-')
}

/// (qualifier, name) of a callee path (`a::b::f`, `pkg.f`, `f`).
fn split_callee(callee: &str) -> (Option<&str>, &str) {
    if let Some((q, n)) = callee.rsplit_once("::") {
        (Some(q), n)
    } else if let Some((q, n)) = callee.rsplit_once('.') {
        (Some(q), n)
    } else {
        (None, callee)
    }
}

/// `V` of a TS `Map<K, V>` / `WeakMap` / `ReadonlyMap` type text (nullable allowed).
fn map_value(text: &str) -> Option<&str> {
    let parts: Vec<&str> = split_top(text.trim(), '|')
        .into_iter()
        .map(str::trim)
        .filter(|p| !matches!(*p, "" | "null" | "undefined"))
        .collect();
    let [t] = parts.as_slice() else {
        return None;
    };
    let (head, _) = outer_generic(t, '<', '>');
    if !matches!(head, "Map" | "WeakMap" | "ReadonlyMap") {
        return None;
    }
    let inner = t[head.len()..].strip_prefix('<')?.strip_suffix('>')?;
    match split_top(inner, ',').as_slice() {
        [_, v] => Some(v.trim()),
        _ => None,
    }
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
    let mut prev = ' ';
    for (i, c) in s.char_indices() {
        let arrow = c == '>' && prev == '=';
        prev = c;
        match c {
            '<' | '(' | '[' | '{' => depth += 1,
            // `=>` of a function type closes nothing.
            '>' if arrow => {}
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

/// Result type of std methods whose name gives the shape away (receiver not a repo type
/// with that method): `b"x".to_vec()`, `s.into_bytes()`, `v.as_slice()`.
const RUST_STD_SHAPES: &[(&str, &str)] = &[
    ("to_vec", "Vec<_>"),
    ("into_vec", "Vec<_>"),
    ("into_bytes", "Vec<u8>"),
    ("as_bytes", "&[u8]"),
    ("as_slice", "&[_]"),
    ("as_mut_slice", "&mut [_]"),
    ("into_boxed_slice", "Box<[_]>"),
    ("to_string", "String"),
];

/// Structural shape of a Rust type for matching an argument against a parameter:
/// references, lifetimes and paths dropped, `_` = unknown.
#[derive(Debug, PartialEq)]
enum Shape {
    Wild,
    Slice(Box<Shape>),
    Array(Box<Shape>),
    Named(String, Vec<Shape>),
    Other,
}

impl Shape {
    fn parse(text: &str) -> Shape {
        let mut t = text.trim();
        loop {
            let before = t;
            t = t.trim_start_matches(['&', '*', ' ']);
            if let Some(rest) = t.strip_prefix('\'') {
                t = rest.trim_start_matches(|c: char| c.is_alphanumeric() || c == '_');
            }
            for kw in ["mut", "const", "dyn"] {
                if let Some(rest) = t.strip_prefix(kw) {
                    if rest.starts_with([' ', '[', '(']) {
                        t = rest;
                    }
                }
            }
            if t == before {
                break;
            }
        }
        if t == "_" {
            return Shape::Wild;
        }
        if let Some(inner) = t.strip_prefix('[').and_then(|x| x.strip_suffix(']')) {
            let parts = split_top(inner, ';');
            let el = Box::new(Shape::parse(parts[0]));
            return if parts.len() > 1 {
                Shape::Array(el)
            } else {
                Shape::Slice(el)
            };
        }
        if t.is_empty() || t.starts_with(['(', '[']) || t.starts_with("fn") || t.starts_with("impl")
        {
            return Shape::Other;
        }
        let (head, args) = match t.find('<') {
            Some(i) if t.ends_with('>') => (&t[..i], &t[i + 1..t.len() - 1]),
            _ => (t, ""),
        };
        let head = head.rsplit("::").next().unwrap_or(head).trim();
        let args = if args.is_empty() {
            Vec::new()
        } else {
            split_top(args, ',')
                .into_iter()
                .filter(|a| !a.trim_start().starts_with('\''))
                .map(Shape::parse)
                .collect()
        };
        Shape::Named(head.to_string(), args)
    }

    /// A parameter type we cannot compare (generic, `impl Trait`, tuple).
    fn opaque(&self) -> bool {
        match self {
            Shape::Named(h, _) => looks_generic(h) || h == "Self",
            Shape::Slice(e) | Shape::Array(e) => e.opaque(),
            Shape::Wild | Shape::Other => true,
        }
    }

    /// Whether an argument of shape `self` fits a parameter of shape `p`.
    fn fits(&self, p: &Shape) -> bool {
        match (self, p) {
            (Shape::Wild, _) | (_, Shape::Wild) => true,
            (Shape::Slice(a), Shape::Slice(b)) | (Shape::Array(a), Shape::Array(b)) => a.fits(b),
            (Shape::Named(a, aa), Shape::Named(b, bb)) => {
                a == b
                    && (aa.is_empty()
                        || bb.is_empty()
                        || (aa.len() == bb.len() && aa.iter().zip(bb).all(|(x, y)| x.fits(y))))
            }
            _ => false,
        }
    }
}

/// First non-`self` parameter type of Rust function `name` in its one-line signature
/// (`fn from(slice: &'static [u8]) -> Bytes {`), or `None` when not on the line.
fn first_param_type<'s>(sig: &'s str, name: &str) -> Option<&'s str> {
    let at = sig.find(&format!("fn {name}"))? + 3 + name.len();
    let rest = &sig[at..];
    // Skip the generic parameter list.
    let mut depth = 0i32;
    let mut open = None;
    for (i, c) in rest.char_indices() {
        match c {
            '<' => depth += 1,
            '>' => depth -= 1,
            '(' if depth == 0 => {
                open = Some(i);
                break;
            }
            _ => {}
        }
    }
    let rest = &rest[open? + 1..];
    let mut depth = 0i32;
    let mut close = None;
    for (i, c) in rest.char_indices() {
        match c {
            '(' | '<' | '[' => depth += 1,
            ')' if depth == 0 => {
                close = Some(i);
                break;
            }
            ')' | '>' | ']' => depth -= 1,
            _ => {}
        }
    }
    let params = &rest[..close?];
    split_top(params, ',')
        .into_iter()
        .map(str::trim)
        .find(|p| !p.is_empty() && !is_self_param(p))
        .and_then(|p| p.split_once(':').map(|(_, t)| t.trim()))
}

/// `self`, `&self`, `&'a mut self`, `mut self`, `self: Box<Self>`.
fn is_self_param(p: &str) -> bool {
    let mut t = p.trim_start_matches(['&', ' ']);
    if let Some(rest) = t.strip_prefix('\'') {
        t = rest
            .trim_start_matches(|c: char| c.is_alphanumeric() || c == '_')
            .trim_start();
    }
    let t = t.strip_prefix("mut ").unwrap_or(t).trim_start();
    t == "self" || t.starts_with("self:") || t.starts_with("self ")
}

/// Element `idx` of a Rust tuple type value (`(ExtendedFloat, bool)`, behind references,
/// after `?`/unwrap), or `None` when `v` is not a tuple text.
fn tuple_elem(v: &Val, idx: &str) -> Option<Val> {
    let Val::Text {
        fi,
        from,
        line,
        text,
        unwrap,
        elem,
        ..
    } = v
    else {
        return None;
    };
    let t = if *unwrap {
        unwrap_text(Lang::Rust, text)
    } else {
        text.clone()
    };
    let t = if *elem {
        element_of(Lang::Rust, &t)?
    } else {
        t
    };
    let t = t.trim().trim_start_matches('&');
    let inner = t.strip_prefix('(')?.strip_suffix(')')?;
    let part = split_top(inner, ',')
        .get(idx.parse::<usize>().ok()?)?
        .trim();
    (!part.is_empty()).then(|| Val::Text {
        fi: *fi,
        from: *from,
        line: *line,
        text: part.to_string(),
        unwrap: false,
        awaited: false,
        elem: false,
    })
}

/// `F` for a Rust type text `F`, `&F`, `&'a mut F` (a candidate bare generic), else "".
fn generic_core(text: &str) -> &str {
    let mut t = text.trim();
    loop {
        let before = t;
        t = t.trim_start_matches(['&', ' ']);
        if let Some(rest) = t.strip_prefix('\'') {
            t = rest.trim_start_matches(|c: char| c.is_alphanumeric() || c == '_');
        }
        t = t.strip_prefix("mut ").unwrap_or(t);
        if t == before {
            break;
        }
    }
    if t.chars().all(|c| c.is_alphanumeric() || c == '_') {
        t
    } else {
        ""
    }
}

/// The normalized slice/array name (`[u8]`) of a Rust type text behind references
/// (`&'a mut [u8]`), or `None` for anything else.
fn rust_slice_type(text: &str) -> Option<String> {
    let mut t = text.trim();
    loop {
        let before = t;
        t = t.trim_start_matches(['&', ' ']);
        if let Some(rest) = t.strip_prefix('\'') {
            t = rest.trim_start_matches(|c: char| c.is_alphanumeric() || c == '_');
        }
        // Squashed hints drop the space: `&mut[u8]`.
        if let Some(rest) = t.strip_prefix("mut") {
            if rest.starts_with([' ', '[']) {
                t = rest;
            }
        }
        if t == before {
            break;
        }
    }
    t.starts_with('[')
        .then(|| crate::extract::rust_slice_name(t))
        .flatten()
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
            // `Annotation | boolean`: a method call needs the one object type.
            let objs: Vec<&str> = parts.iter().copied().filter(|p| !ts_primitive(p)).collect();
            let parts = if parts.len() > 1 && objs.len() == 1 {
                objs
            } else {
                parts
            };
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
    pub(super) fn eval_hint(&self, fi: usize, from: Option<i64>, line: u32, hint: &str) -> Ty {
        // The line only matters in a file declaring one type name twice.
        let line = if self.dup_types(fi) { line } else { 0 };
        let key = (fi, from, line, hint.to_string());
        if let Some(&t) = self
            .memo
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&key)
        {
            return t;
        }
        let v = self.eval_val(fi, from, line, hint, 0);
        let t = self.bind_val(&v, 0);
        self.memo
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(key, t);
        t
    }

    fn eval_val(&self, fi: usize, from: Option<i64>, line: u32, h: &str, depth: u8) -> Val {
        if depth > MAX_DEPTH {
            return Val::Ty(Ty::Unknown);
        }
        let d = depth + 1;
        if h == "S" {
            if let Some(t) = self.self_sym(from) {
                return Val::Ty(Ty::Known(t));
            }
            let (ty, _) = self.self_type(from);
            return Val::Ty(match ty {
                Some(name) => self.find_type(fi, None, &name, from, line),
                None => Ty::Unknown,
            });
        }
        if h == "B" {
            // The base type of the enclosing type: the first base that binds.
            return Val::Ty(
                self.base_types(fi, from, line)
                    .into_iter()
                    .next()
                    .unwrap_or(Ty::Unknown),
            );
        }
        if h == "L" {
            return Val::Ty(Ty::Unknown);
        }
        if let Some(t) = h.strip_prefix("T:") {
            return Val::Text {
                fi,
                from,
                line,
                text: t.to_string(),
                unwrap: false,
                awaited: false,
                elem: false,
            };
        }
        if let Some(inner) = h.strip_prefix("E:") {
            return match self.eval_val(fi, from, line, inner, d) {
                Val::Text {
                    fi,
                    from,
                    line,
                    text,
                    unwrap,
                    awaited,
                    elem: false,
                } => Val::Text {
                    fi,
                    from,
                    line,
                    text,
                    unwrap,
                    awaited,
                    elem: true,
                },
                // An element of an element (`grid[i][j]`, `for _, x := range xss[i]`):
                // take the inner element type now.
                Val::Text {
                    fi,
                    from,
                    line,
                    text,
                    unwrap,
                    awaited,
                    elem: true,
                } => {
                    let lang = self.files[fi].lang;
                    let outer = if unwrap {
                        unwrap_text(lang, &text)
                    } else {
                        text
                    };
                    match element_of(lang, &outer) {
                        Some(text) => Val::Text {
                            fi,
                            from,
                            line,
                            text,
                            unwrap: false,
                            awaited,
                            elem: true,
                        },
                        None => Val::Ty(Ty::Unknown),
                    }
                }
                _ => Val::Ty(Ty::Unknown),
            };
        }
        // Python untyped parameter `i` of method `m`: the annotation at the same position
        // of `m` in a base class of the method's class.
        if let Some(rest) = h.strip_prefix("P:") {
            return self
                .inherited_param(from, rest, d)
                .unwrap_or(Val::Ty(Ty::Unknown));
        }
        // Python `for a, b in xs`: position `i` of the element tuple.
        if let Some(rest) = h.strip_prefix("I:") {
            let Some((i, inner)) = rest.split_once('@') else {
                return Val::Ty(Ty::Unknown);
            };
            let Ok(i) = i.parse::<usize>() else {
                return Val::Ty(Ty::Unknown);
            };
            return match self.eval_val(fi, from, line, inner, d) {
                Val::Text {
                    fi,
                    from,
                    line,
                    text,
                    unwrap: false,
                    awaited: false,
                    elem,
                } => {
                    let lang = self.files[fi].lang;
                    let tuple = if elem {
                        element_of(lang, &text)
                    } else {
                        Some(text)
                    };
                    match tuple.as_deref().and_then(|t| tuple_item(t, i)) {
                        Some(text) => Val::Text {
                            fi,
                            from,
                            line,
                            text,
                            unwrap: false,
                            awaited: false,
                            elem: false,
                        },
                        None => Val::Ty(Ty::Unknown),
                    }
                }
                _ => Val::Ty(Ty::Unknown),
            };
        }
        // Python `d[k]`: a value of the collection (`dict[K, V]` -> V, `list[T]` -> T).
        if let Some(inner) = h.strip_prefix("V:") {
            return match self.eval_val(fi, from, line, inner, d) {
                Val::Text {
                    fi,
                    from,
                    line,
                    text,
                    unwrap: false,
                    awaited: false,
                    elem: false,
                } => match subscript_value(&text) {
                    Some(text) => Val::Text {
                        fi,
                        from,
                        line,
                        text,
                        unwrap: false,
                        awaited: false,
                        elem: false,
                    },
                    None => Val::Ty(Ty::Unknown),
                },
                _ => Val::Ty(Ty::Unknown),
            };
        }
        // Python `@deco def f`: `f` is what calling the decorator's value returns.
        if let Some(inner) = h.strip_prefix("D:") {
            return match self.eval_val(fi, from, line, inner, d) {
                Val::Text {
                    fi,
                    from,
                    line,
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
                            line,
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
            return match self.eval_val(fi, from, line, inner, d) {
                Val::Text {
                    fi,
                    from,
                    line,
                    text,
                    unwrap,
                    awaited,
                    elem,
                } => Val::Text {
                    fi,
                    from,
                    line,
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
        if let Some(g) = h.strip_prefix("G:") {
            return self.eval_global(fi, g, d);
        }
        // JS/TS `N:<guard>@<hint>`: narrowed by a type guard, else `<hint>`.
        if let Some(rest) = h.strip_prefix("N:") {
            let Some((guard, base)) = rest.split_once('@') else {
                return Val::Ty(Ty::Unknown);
            };
            return match self.guard_type(fi, from, guard) {
                Some(v) => v,
                None => self.eval_val(fi, from, line, base, d),
            };
        }
        let (tag, rest) = h.split_at(2.min(h.len()));
        let Some((name, base)) = rest.split_once('@') else {
            return Val::Ty(Ty::Unknown);
        };
        match tag {
            "M:" => {
                let lang = self.files[fi].lang;
                let bv = self.eval_val(fi, from, line, base, d);
                let bt = self.bind_val(&bv, d);
                // The receiver's own method wins over the name-based shortcuts below
                // (`Db::read`, `Bag.values` are not std's).
                if let Ty::Known(t) = bt {
                    if let Some((m, _)) = self.method_on(t, name, &mut HashSet::new(), d) {
                        // `-> Self` (Python typing.Self): the receiver's own type.
                        let is_self = self.syms[m].ret.as_deref().is_some_and(|r| {
                            let r = r.strip_prefix("T:").unwrap_or(r);
                            r.rsplit('.').next() == Some("Self") && lang == Lang::Python
                        });
                        if is_self {
                            return Val::Ty(Ty::Known(t));
                        }
                        return match self.syms[m].ret.clone() {
                            Some(r) => {
                                let mf = self.syms[m].file;
                                self.eval_val(mf, Some(self.syms[m].id), self.syms[m].line, &r, d)
                            }
                            None => Val::Ty(Ty::Unknown),
                        };
                    }
                    // Python `self.context_class(..)` with `context_class: type[Context]`.
                    if lang == Lang::Python {
                        if let Val::Text { fi, from, text, .. } =
                            self.field_of(t, name, &mut HashSet::new(), d)
                        {
                            let tt = strip_typing(&text);
                            let inner = tt
                                .strip_prefix("type[")
                                .or_else(|| tt.strip_prefix("Type["))
                                .and_then(|x| x.strip_suffix(']'));
                            if let Some(inner) = inner {
                                return Val::Text {
                                    fi,
                                    from,
                                    line,
                                    text: inner.to_string(),
                                    unwrap: false,
                                    awaited: false,
                                    elem: false,
                                };
                            }
                        }
                    }
                    // Python `with m as x` on a repo type without its own `__enter__`
                    // (inherited from outside the repo): context managers return self.
                    if name == "__enter__" && lang == Lang::Python {
                        return Val::Ty(Ty::Known(t));
                    }
                }
                // An in-repo trait's method on a std type (`vec.put(..)` with
                // `impl BufMut for Vec<u8>`).
                if let Ty::External(Some(id)) = bt {
                    if let Some(m) = self.extension_method(fi, id, name) {
                        return match self.syms[m].ret.clone() {
                            Some(r) => {
                                let mf = self.syms[m].file;
                                self.eval_val(mf, Some(self.syms[m].id), self.syms[m].line, &r, d)
                            }
                            None => Val::Ty(Ty::Unknown),
                        };
                    }
                }
                // Python `d.get(k)` on `dict[K, V]`: a `V`.
                if lang == Lang::Python && matches!(name, "get" | "pop" | "setdefault") {
                    if let Val::Text {
                        fi,
                        from,
                        line: vline,
                        text,
                        elem: false,
                        ..
                    } = &bv
                    {
                        if let Some(v) = subscript_value(text).filter(|_| {
                            strip_typing(text).split('[').next().is_some_and(|h| {
                                matches!(
                                    h,
                                    "dict"
                                        | "Dict"
                                        | "Mapping"
                                        | "MutableMapping"
                                        | "defaultdict"
                                        | "OrderedDict"
                                )
                            })
                        }) {
                            return Val::Text {
                                fi: *fi,
                                from: *from,
                                line: *vline,
                                text: v,
                                unwrap: false,
                                awaited: false,
                                elem: false,
                            };
                        }
                    }
                }
                if lang == Lang::Rust && bt != Ty::Unknown {
                    if let Some((_, shape)) = RUST_STD_SHAPES.iter().find(|(m, _)| *m == name) {
                        return Val::Text {
                            fi,
                            from,
                            line,
                            text: (*shape).to_string(),
                            unwrap: false,
                            awaited: false,
                            elem: false,
                        };
                    }
                }
                if lang == Lang::Rust && PASSTHROUGH.contains(&name) {
                    return bv;
                }
                // JS/TS `Map<K, V>.get(k)`: a `V`.
                if lang.is_js_family() && name == "get" {
                    if let Val::Text {
                        fi,
                        from,
                        line,
                        text,
                        unwrap: false,
                        awaited: false,
                        elem: false,
                    } = &bv
                    {
                        if let Some(v) = map_value(text) {
                            return Val::Text {
                                fi: *fi,
                                from: *from,
                                line: *line,
                                text: v.to_string(),
                                unwrap: false,
                                awaited: false,
                                elem: false,
                            };
                        }
                    }
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
                        line,
                        text,
                        awaited,
                        elem,
                        ..
                    } = bv
                    {
                        return Val::Text {
                            fi,
                            from,
                            line,
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
            "F:" if name.bytes().all(|b| b.is_ascii_digit())
                && self.files[fi].lang == Lang::Rust =>
            {
                // `t.0` / `let (a, b) = f()`: element of a tuple type, else a tuple struct's
                // positional field.
                let bv = self.eval_val(fi, from, line, base, d);
                if let Some(v) = tuple_elem(&bv, name) {
                    return v;
                }
                match self.bind_val(&bv, d) {
                    Ty::Known(t) => self.field_of(t, name, &mut HashSet::new(), d),
                    other => Val::Ty(other),
                }
            }
            "F:" => match self.bind_val(&self.eval_val(fi, from, line, base, d), d) {
                Ty::Known(t) => {
                    let v = self.field_of(t, name, &mut HashSet::new(), d);
                    self.rust_generic_field(fi, from, v)
                }
                other => Val::Ty(other),
            },
            _ => Val::Ty(Ty::Unknown),
        }
    }

    /// A Rust field typed with a bare generic (`formatter: F`, `read: R`) is bound by the
    /// generics in scope at the access (`impl<W, F: Formatter> Serializer<W, F>`): its
    /// type text is re-read in the caller's context.
    fn rust_generic_field(&self, fi: usize, from: Option<i64>, v: Val) -> Val {
        let Val::Text {
            fi: tfi,
            text,
            unwrap,
            awaited,
            elem,
            ..
        } = &v
        else {
            return v;
        };
        if self.files[*tfi].lang != Lang::Rust {
            return v;
        }
        let core = generic_core(text);
        if self.generic_bound(from, core).is_none() {
            return v;
        }
        Val::Text {
            fi,
            from,
            line: 0,
            text: text.clone(),
            unwrap: *unwrap,
            awaited: *awaited,
            elem: *elem,
        }
    }

    /// The bound hint of Rust generic parameter `name` in scope of symbol `from` (its
    /// innermost enclosing function, whose `fields` hold `<T>` -> `T:<bound>`), with the
    /// function's (file, id).
    fn generic_bound(&self, from: Option<i64>, name: &str) -> Option<(usize, i64, &str)> {
        if name.is_empty() || !name.chars().next().is_some_and(char::is_uppercase) {
            return None;
        }
        let mut cur = from.and_then(|id| self.by_id.get(&id).copied());
        for _ in 0..64 {
            let s = &self.syms[cur?];
            if matches!(s.kind, SymKind::Function | SymKind::Method) {
                let key = format!("<{name}>");
                return s
                    .fields
                    .iter()
                    .find(|(k, _)| *k == key)
                    .map(|(_, h)| (s.file, s.id, h.as_str()));
            }
            cur = s.parent.and_then(|p| self.by_id.get(&p).copied());
        }
        None
    }

    /// Whether type `t` or one of its bases declares a field (a class attribute, not a
    /// symbol) named `name`: the member is there, just not as a method.
    fn declares_field(&self, t: usize, name: &str, d: u8) -> bool {
        let s = &self.syms[t];
        if s.fields.iter().any(|(k, _)| k == name) {
            return true;
        }
        d < 4
            && self.bases.get(&s.name).into_iter().flatten().any(|b| {
                self.by_name.get(b).into_iter().flatten().any(|&bt| {
                    bt != t
                        && self.syms[bt].kind.is_type_like()
                        && self.declares_field(bt, name, d + 1)
                })
            })
    }

    fn field_of(&self, t: usize, name: &str, seen: &mut HashSet<usize>, d: u8) -> Val {
        if !seen.insert(t) || d > MAX_DEPTH {
            return Val::Ty(Ty::Unknown);
        }
        if let Some(target) = self.alias_target(t) {
            return self.field_of(target, name, seen, d + 1);
        }
        let s = &self.syms[t];
        // A named field, else a Go embedded field accessed by its type name
        // (`be.HTTPError` for an embedded `*HTTPError`, stored as `~HTTPError`).
        if let Some((_, h)) = s.fields.iter().find(|(k, _)| k == name).or_else(|| {
            s.fields
                .iter()
                .find(|(k, _)| k.strip_prefix('~') == Some(name))
        }) {
            return self.eval_val(s.file, Some(s.id), s.line, h, d + 1);
        }
        // Go embedded fields promote their fields.
        for (k, h) in &s.fields {
            if k.starts_with('~') {
                if let Ty::Known(b) =
                    self.bind_val(&self.eval_val(s.file, Some(s.id), s.line, h, d + 1), d + 1)
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

    /// Go package-level var/const `name` as seen from file `fi`: same package when
    /// unqualified, else in the package `qual` is imported as. `Err(())`: `qual` is a
    /// package outside the repo. `Ok(None)`: no such var (or several).
    fn go_global(&self, fi: usize, qual: Option<&str>, name: &str) -> Result<Option<usize>, ()> {
        let scope = &self.scopes[fi];
        let is_var = |s: usize| matches!(self.syms[s].kind, SymKind::Var | SymKind::Const);
        let cands: Vec<usize> = if let Some(q) = qual {
            match scope.modules.get(q) {
                Some(t) if t.is_empty() => return Err(()),
                Some(t) => self
                    .by_name
                    .get(name)
                    .into_iter()
                    .flatten()
                    .copied()
                    .filter(|&s| is_var(s) && t.contains(&self.syms[s].file))
                    .collect(),
                None => return Ok(None),
            }
        } else {
            // A package name bound in this file is not a var.
            if scope.modules.contains_key(name) {
                return Ok(None);
            }
            let f = &self.files[fi];
            self.by_name
                .get(name)
                .into_iter()
                .flatten()
                .copied()
                .filter(|&s| {
                    let sf = &self.files[self.syms[s].file];
                    is_var(s) && sf.lang == Lang::Go && sf.package == f.package
                })
                .collect()
        };
        Ok(match cands.as_slice() {
            [s] => Some(*s),
            _ => None,
        })
    }

    /// `G:` hint: a Go package-level var (`Name`, `pkg.Name`), or a field of one
    /// (`cfg.Field` where `cfg` is a var of this package declared in another file).
    fn eval_global(&self, fi: usize, g: &str, d: u8) -> Val {
        if self.files[fi].lang != Lang::Go {
            return Val::Ty(Ty::Unknown);
        }
        let (qual, name) = match g.split_once('.') {
            Some((q, n)) => (Some(q), n),
            None => (None, g),
        };
        if let Some(q) = qual {
            if !self.scopes[fi].modules.contains_key(q) {
                return match self.bind_val(&self.eval_global(fi, q, d + 1), d + 1) {
                    Ty::Known(t) => self.field_of(t, name, &mut HashSet::new(), d + 1),
                    other => Val::Ty(other),
                };
            }
        }
        match self.go_global(fi, qual, name) {
            Err(()) => Val::Ty(Ty::External(None)),
            Ok(Some(s)) => match self.syms[s].ret.clone() {
                Some(r) => self.eval_val(
                    self.syms[s].file,
                    Some(self.syms[s].id),
                    self.syms[s].line,
                    &r,
                    d,
                ),
                None => Val::Ty(Ty::Unknown),
            },
            Ok(None) => Val::Ty(Ty::Unknown),
        }
    }

    fn eval_call(&self, fi: usize, from: Option<i64>, callee: &str, d: u8) -> Val {
        let lang = self.files[fi].lang;
        let (callee, shape) = match callee.rsplit_once('#') {
            Some((c, sh)) => (c, sh.chars().next()),
            None => (callee, None),
        };
        if lang.is_js_family() {
            if let Some(v) = self.value_call(fi, callee, shape, d) {
                return v;
            }
            if callee.matches('.').count() > 1 {
                // `ns.value.m()` and `ns` is no namespace of ours: no guessing by name.
                return Val::Ty(Ty::Unknown);
            }
        }
        let (qual, name) = split_callee(callee);
        if let Some((s, _)) = self.resolve_callee(fi, from, callee) {
            let sym = &self.syms[s];
            if sym.kind.is_type_like() {
                return Val::Ty(Ty::Known(s));
            }
            return match sym.ret.clone() {
                Some(r) => self.eval_val(sym.file, Some(sym.id), sym.line, &r, d),
                None => Val::Ty(Ty::Unknown),
            };
        }
        // Go: calling a package-level var bound to a function (`var NewX = NewBase[T]`).
        if lang == Lang::Go {
            if let Ok(Some(s)) = self.go_global(fi, qual, name) {
                if let Some(r) = self.syms[s].ret.clone().filter(|r| r.starts_with("C:")) {
                    return self.eval_val(
                        self.syms[s].file,
                        Some(self.syms[s].id),
                        self.syms[s].line,
                        &r,
                        d,
                    );
                }
            }
        }
        // `Type::default()` / `Type::from(..)` from a derive or a std trait impl: no symbol,
        // but it returns the type itself.
        if let (Lang::Rust, Some(q)) = (lang, qual) {
            if SELF_CTORS.contains(&name) {
                let (tq, tn) = match q.rsplit_once("::") {
                    Some((a, b)) => (Some(a), b),
                    None => (None, q),
                };
                // `Self::default()`: the enclosing impl's type.
                let found = if tq.is_none() && tn == "Self" {
                    match self.self_type(from).0 {
                        Some(n) => self.find_type(fi, None, &n, from, 0),
                        None => Ty::Unknown,
                    }
                } else {
                    self.find_type(fi, tq, tn, from, 0)
                };
                if let Ty::Known(t) = found {
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

    /// The symbol a call of `callee` (`f`, `pkg.f`, `T::f`) from file `fi` lands on.
    fn resolve_callee(
        &self,
        fi: usize,
        from: Option<i64>,
        callee: &str,
    ) -> Option<(usize, Confidence)> {
        let (qual, name) = split_callee(callee);
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
            arg: None,
            line: 0,
        };
        self.resolve_edge(&fake, &self.scopes[fi])
    }

    /// (file, first line, last line) of the object literal a JS/TS hint stands for: `O:a-b`
    /// itself, or `C:f` where `f`'s return hint is one.
    fn object_at(&self, fi: usize, from: Option<i64>, hint: &str) -> Option<(usize, u32, u32)> {
        let parse = |h: &str| -> Option<(u32, u32)> {
            let (a, b) = h.strip_prefix("O:")?.split_once('-')?;
            Some((a.parse().ok()?, b.parse().ok()?))
        };
        if let Some((a, b)) = parse(hint) {
            return Some((fi, a, b));
        }
        let callee = hint.strip_prefix("C:")?;
        if !self.files[fi].lang.is_js_family() {
            return None;
        }
        let key = (fi, from, hint.to_string());
        if let Some(&r) = self
            .obj_memo
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&key)
        {
            return r;
        }
        let callee = callee.rsplit_once('#').map_or(callee, |(c, _)| c);
        let r = self
            .resolve_callee(fi, from, callee)
            .filter(|&(_, c)| c != Confidence::NameOnly)
            .and_then(|(s, _)| {
                let sym = &self.syms[s];
                let (a, b) = parse(sym.ret.as_deref()?)?;
                Some((sym.file, a, b))
            });
        self.obj_memo
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(key, r);
        r
    }

    /// JS/TS hint `G:<guard>@..`: the type a type guard asserts (`(x: any) => x is Foo`,
    /// `function isFoo(x): x is Foo`), `guard` being a function or a function-typed value
    /// of the repo. `None` when it is no type guard.
    fn guard_type(&self, fi: usize, from: Option<i64>, guard: &str) -> Option<Val> {
        let s = match self.value_sym(fi, guard) {
            Some(v) => v,
            None => match self.resolve_callee(fi, from, guard)? {
                (s, c) if c != Confidence::NameOnly && self.syms[s].kind.is_callable() => s,
                _ => return None,
            },
        };
        let sym = &self.syms[s];
        let text = sym.ret.as_deref()?.strip_prefix("T:")?;
        let ret = fn_type_parts(text).map_or_else(|| text.to_string(), |(_, r)| r);
        let (_, ty) = ret.split_once(" is ")?;
        let ty = ty.trim();
        if ty.is_empty() {
            return None;
        }
        Some(Val::Text {
            fi: sym.file,
            from: Some(sym.id),
            line: sym.line,
            text: ty.to_string(),
            unwrap: false,
            awaited: false,
            elem: false,
        })
    }

    /// A module-level JS/TS value's type (`const computed: Factory = ..`).
    fn value_val(&self, fi: usize, path: &str, d: u8) -> Option<Val> {
        let v = self.value_sym(fi, path)?;
        let s = &self.syms[v];
        Some(match s.ret.clone() {
            Some(r) => self.eval_val(s.file, Some(s.id), s.line, &r, d),
            None => Val::Ty(Ty::Unknown),
        })
    }

    /// `f(..)` / `ns.f(..)` where `f` is a module-level value (a factory object with call
    /// signatures, a function-typed const), or `v.m(..)` / `ns.v.m(..)`: member `m` of such
    /// a value (a method, or a function-typed / callable field). `None`: no value there.
    fn value_call(&self, fi: usize, callee: &str, shape: Option<char>, d: u8) -> Option<Val> {
        if let Some(v) = self.value_val(fi, callee, d) {
            return Some(self.call_val(&v, shape, d));
        }
        let (q, m) = callee.rsplit_once('.')?;
        let v = self.value_val(fi, q, d)?;
        let Ty::Known(t) = self.bind_val(&v, d) else {
            return Some(Val::Ty(Ty::Unknown));
        };
        if let Some((ms, _)) = self.method_on(t, m, &mut HashSet::new(), d) {
            let sym = &self.syms[ms];
            return Some(match sym.ret.clone() {
                Some(r) => self.eval_val(sym.file, Some(sym.id), sym.line, &r, d),
                None => Val::Ty(Ty::Unknown),
            });
        }
        let f = self.field_of(t, m, &mut HashSet::new(), d);
        Some(match f {
            Val::Ty(Ty::Unknown) => f,
            f => self.call_val(&f, shape, d),
        })
    }

    /// The result of calling a value of type `v`: a function type's return type, or the
    /// call signatures of an interface / type alias (see [`Self::call_sigs`]).
    fn call_val(&self, v: &Val, shape: Option<char>, d: u8) -> Val {
        if d > MAX_DEPTH {
            return Val::Ty(Ty::Unknown);
        }
        if let Val::Text {
            fi,
            from,
            line,
            text,
            unwrap: false,
            awaited: false,
            elem: false,
        } = v
        {
            if let Some((sh, ret)) = fn_type_parts(text) {
                if !shape_ok(shape, sh) {
                    return Val::Ty(Ty::Unknown);
                }
                return Val::Text {
                    fi: *fi,
                    from: *from,
                    line: *line,
                    text: ret,
                    unwrap: false,
                    awaited: false,
                    elem: false,
                };
            }
        }
        match self.bind_val(v, d) {
            Ty::Known(t) => self
                .call_sigs(t, shape, &mut HashSet::new(), d + 1)
                .unwrap_or(Val::Ty(Ty::Unknown)),
            _ => Val::Ty(Ty::Unknown),
        }
    }

    /// Calling a value of type `t` through its call signatures (fields `()<shape>`, see
    /// `infer::param_shape`): those whose first parameter fits the first argument's
    /// shape; the type's own before its alias target's and its bases' (TypeScript tries
    /// derived overloads first). A result only when every fitting signature agrees on
    /// the type; `None` when `t` has no fitting signature.
    fn call_sigs(
        &self,
        t: usize,
        shape: Option<char>,
        seen: &mut HashSet<usize>,
        d: u8,
    ) -> Option<Val> {
        if !seen.insert(t) || d > MAX_DEPTH {
            return None;
        }
        let s = &self.syms[t];
        let fits: Vec<Val> = s
            .fields
            .iter()
            .filter_map(|(k, h)| {
                let sh = k.strip_prefix("()")?.chars().next()?;
                shape_ok(shape, sh).then(|| self.eval_val(s.file, Some(s.id), s.line, h, d + 1))
            })
            .collect();
        if !fits.is_empty() {
            return Some(self.agree(fits, d));
        }
        if let Some(target) = self.alias_target(t) {
            if let Some(v) = self.call_sigs(target, shape, seen, d + 1) {
                return Some(v);
            }
        }
        let mut found: Vec<Val> = Vec::new();
        for base in self.bases.get(&s.name).into_iter().flatten() {
            if let Ty::Known(b) = self.find_type(s.file, None, base, Some(s.id), s.line) {
                found.extend(self.call_sigs(b, shape, seen, d + 1));
            }
        }
        (!found.is_empty()).then(|| self.agree(found, d))
    }

    /// The first of `vals` when all bind to one type, else unknown.
    fn agree(&self, vals: Vec<Val>, d: u8) -> Val {
        let mut it = vals.into_iter();
        let Some(first) = it.next() else {
            return Val::Ty(Ty::Unknown);
        };
        let ty = self.bind_val(&first, d);
        if ty == Ty::Unknown || it.any(|v| self.bind_val(&v, d) != ty) {
            return Val::Ty(Ty::Unknown);
        }
        first
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
                line,
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
                if lang == Lang::Rust && !*unwrap {
                    // `&mut [u8]`: std, or an in-repo trait's extension (`impl BufMut for
                    // &mut [u8]`).
                    if let Some(slice) = rust_slice_type(text) {
                        // An array receiver also gets its slice's methods (`[u8; 10]` ->
                        // `impl BufMut for &mut [u8]`).
                        let unsized_ = split_top(&slice[1..slice.len() - 1], ';')
                            .first()
                            .map(|el| format!("[{el}]"));
                        let id = self
                            .ext_id(lang, &slice)
                            .or_else(|| unsized_.and_then(|u| self.ext_id(lang, &u)));
                        return Ty::External(id);
                    }
                }
                match normalize(lang, text, *unwrap, *awaited, 0) {
                    Norm::External => Ty::External(None),
                    Norm::Unknown => Ty::Unknown,
                    Norm::SelfType => match self.self_sym(*from) {
                        Some(t) => Ty::Known(t),
                        None => match self.self_type(*from).0 {
                            Some(n) => self.find_type(*fi, None, &n, *from, *line),
                            None => Ty::Unknown,
                        },
                    },
                    Norm::Name(q, n) => {
                        let _ = d;
                        self.find_type(*fi, q.as_deref(), &n, *from, *line)
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

    /// Rust type symbols named `name` defined in module `files`, or re-exported there by a
    /// `pub use` (not the directory-wide barrel guess of [`Ctx::reexported`]: a `#[cfg]`
    /// twin module's items must stay unbound).
    fn rust_type_syms_via(&self, files: &[usize], name: &str) -> Vec<usize> {
        let direct = self.type_syms_in(files, name);
        if !direct.is_empty() {
            return direct;
        }
        self.lookup_in(files, name, EdgeKind::Ref, 2)
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
            // A Rust associated type (`type Item = u8;` in an impl) is no module item.
            .filter(|&s| {
                self.syms[s].container.is_none()
                    || self.files[self.syms[s].file].lang != Lang::Rust
            })
            .collect()
    }

    /// Bind a type name as seen from file `fi`.
    /// `line`: where the name is seen from (0 = unknown); in JS/TS several same-file types
    /// of that name (sibling test callbacks each declaring `class Store`) resolve to the
    /// closest one declared at or before it (classes are block-scoped, declared before use).
    fn find_type(
        &self,
        fi: usize,
        qual: Option<&str>,
        name: &str,
        from: Option<i64>,
        line: u32,
    ) -> Ty {
        let f = &self.files[fi];
        let lang = f.lang;
        let scope = &self.scopes[fi];
        if lang == Lang::Rust && qual.is_none() {
            // A generic parameter with a single trait bound: that trait.
            if let Some((gfi, gid, h)) = self.generic_bound(from, name) {
                let bound = h.to_string();
                return self.bind_val(&self.eval_val(gfi, Some(gid), 0, &bound, 0), 0);
            }
        }
        if let Some(q) = qual {
            if lang == Lang::Rust {
                return match self.rust_path(fi, &format!("{q}::{name}")) {
                    // `stash::Repository`, `super::Buf`: also through `pub use` re-exports.
                    Some((files, Some(item))) => {
                        self.pick_type(fi, &self.rust_type_syms_via(&files, &item))
                    }
                    Some((files, None)) => {
                        self.pick_type(fi, &self.rust_type_syms_via(&files, name))
                    }
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
        let mut local = self.type_syms_in(&[fi], name);
        if lang == Lang::Rust {
            // An item nested in a function (`type Value = MyVec<T>;` of an impl inside a
            // test fn) is visible only inside that function.
            local.retain(|&s| self.rust_item_visible(s, from));
        }
        if local.len() > 1 && line > 0 && lang.is_js_family() {
            if let Some(&s) = local
                .iter()
                .filter(|&&s| self.syms[s].line <= line)
                .max_by_key(|&&s| (self.syms[s].line, std::cmp::Reverse(s)))
            {
                return Ty::Known(s);
            }
        }
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
            // Rust associated types are not named bare.
            .filter(|&s| lang != Lang::Rust || self.syms[s].container.is_none())
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

    /// Whether Rust item `s` can be named from inside symbol `from`: a module-level item,
    /// or one nested in a function/type that encloses `from`.
    fn rust_item_visible(&self, s: usize, from: Option<i64>) -> bool {
        if self.syms[s].container.is_some() {
            // An associated type (`type Value = Value;` in an impl): only `Self::Value`.
            return false;
        }
        let Some(p) = self.syms[s].parent else {
            return true;
        };
        if self
            .by_id
            .get(&p)
            .is_some_and(|&pi| self.syms[pi].kind == SymKind::Module)
        {
            return true;
        }
        let mut cur = from;
        for _ in 0..64 {
            let Some(id) = cur else { break };
            if id == p {
                return true;
            }
            cur = self.by_id.get(&id).and_then(|&i| self.syms[i].parent);
        }
        false
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
        // `impl Extend<u8> for BytesMut` and `impl Extend<Bytes> for BytesMut`: which impl
        // a call picks depends on the argument type.
        if own.len() > 1 && tf.lang == Lang::Rust && super::TRAIT_OVERLOADS.contains(&name) {
            return None;
        }
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
                if let Ty::Known(b) = self.bind_val(
                    &self.eval_val(ts.file, Some(ts.id), ts.line, h, d + 1),
                    d + 1,
                ) {
                    if let Some((m, _)) = self.method_on(b, name, seen, d + 1) {
                        return Some((m, true));
                    }
                }
            }
        }
        // Traits / base classes / implemented interfaces.
        for (qual, base) in self.base_quals.get(&ts.name).into_iter().flatten() {
            let Ty::Known(b) = self.find_type(ts.file, qual.as_deref(), base, Some(ts.id), ts.line)
            else {
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
                        (Some(i), Some(t)) => t.fits(i),
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
            match self.bind_val(&self.eval_val(s.file, Some(s.id), s.line, h, 0), 0) {
                Ty::Known(j) if self.syms[j].kind == SymKind::Interface => {
                    set.extend(self.iface_methods(j, methods, seen)?);
                }
                _ => {
                    let std = h.strip_prefix("T:").and_then(std_iface)?;
                    set.extend(std.iter().map(|(n, sh)| (*n, Some(GoShape::counts(*sh)))));
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
                out.entry(k).or_insert_with(|| v.clone());
            }
        }
        for (k, h) in &s.fields {
            if k.starts_with('~') {
                if let Ty::Known(b) =
                    self.bind_val(&self.eval_val(s.file, Some(s.id), s.line, h, 0), 0)
                {
                    self.type_methods(b, methods, out, seen);
                } else {
                    // Embedding a stdlib interface promotes its methods.
                    let t = h.strip_prefix("T:").map(|t| t.trim_start_matches('*'));
                    for (n, sh) in t.and_then(std_iface).into_iter().flatten() {
                        out.entry(n).or_insert(Some(GoShape::counts(*sh)));
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
            line: s.line,
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

    /// `P:<i>@<method>` (see [`Self::eval_val`]).
    fn inherited_param(&self, from: Option<i64>, rest: &str, d: u8) -> Option<Val> {
        let (i, method) = rest.split_once('@')?;
        let i: usize = i.parse().ok()?;
        // The method itself: `from` or one of its parents (a nested function's use).
        let mut cur = from.and_then(|id| self.by_id.get(&id).copied());
        let m = loop {
            let k = cur?;
            let s = &self.syms[k];
            if s.kind == SymKind::Method && s.name == method {
                break k;
            }
            cur = s.parent.and_then(|p| self.by_id.get(&p).copied());
        };
        let ms = &self.syms[m];
        let class = ms.parent.and_then(|p| self.by_id.get(&p).copied())?;
        let cs = &self.syms[class];
        let mut frontier = vec![class];
        let mut seen: HashSet<usize> = HashSet::new();
        for _ in 0..4 {
            let mut next = Vec::new();
            for c in frontier {
                if !seen.insert(c) {
                    continue;
                }
                let c_sym = &self.syms[c];
                for (qual, base) in self.base_quals.get(&c_sym.name).into_iter().flatten() {
                    let Ty::Known(b) =
                        self.find_type(c_sym.file, qual.as_deref(), base, None, c_sym.line)
                    else {
                        continue;
                    };
                    let own = self
                        .by_member
                        .get(&(self.syms[b].name.clone(), method.to_string()))
                        .into_iter()
                        .flatten()
                        .copied()
                        .find(|&bm| self.syms[bm].parent == Some(self.syms[b].id));
                    if let Some(bm) = own {
                        let text = param_annotation(&self.syms[bm].sig, i)?;
                        return Some(Val::Text {
                            fi: self.syms[bm].file,
                            from: Some(self.syms[bm].id),
                            line: self.syms[bm].line,
                            text,
                            unwrap: false,
                            awaited: false,
                            elem: false,
                        });
                    }
                    next.push(b);
                }
            }
            frontier = next;
        }
        let _ = (cs, d);
        None
    }

    /// `super().m()` (Python): `m` found through the bases of the class enclosing the
    /// call, never the class itself.
    pub(super) fn resolve_py_super(&self, e: &E) -> Option<(usize, Confidence)> {
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
            if let Ty::Known(b) =
                self.find_type(cs.file, qual.as_deref(), base, Some(cs.id), cs.line)
            {
                if let Some((m, _)) = self.method_on(b, &e.name, &mut seen, 0) {
                    return Some((m, Confidence::Exact));
                }
            }
        }
        None
    }

    /// Method `name` defined by exactly one type deriving (transitively) from `t`, for a
    /// receiver narrowed by `isinstance`. Classes are matched by name, in the same
    /// language.
    fn subclass_method(&self, t: usize, name: &str) -> Option<usize> {
        let lang = self.files[self.syms[t].file].lang;
        let mut found: Vec<usize> = Vec::new();
        let mut seen: HashSet<&str> = HashSet::new();
        let mut frontier: Vec<&str> = vec![self.syms[t].name.as_str()];
        for _ in 0..4 {
            let mut next = Vec::new();
            for b in frontier {
                for sub in self.derived.get(b).into_iter().flatten() {
                    if !seen.insert(sub.as_str()) {
                        continue;
                    }
                    for &m in self
                        .by_member
                        .get(&(sub.clone(), name.to_string()))
                        .into_iter()
                        .flatten()
                    {
                        if self.files[self.syms[m].file].lang == lang && !found.contains(&m) {
                            found.push(m);
                        }
                    }
                    next.push(sub.as_str());
                }
            }
            if next.is_empty() {
                break;
            }
            frontier = next;
        }
        match found.as_slice() {
            [m] => Some(*m),
            _ => None,
        }
    }

    /// Types of the bases of the type enclosing `from` (declaration order: superclass
    /// before interfaces, Python MRO order), each bound where the type is declared.
    fn base_types(&self, fi: usize, from: Option<i64>, line: u32) -> Vec<Ty> {
        let t = match self.self_sym(from) {
            Some(t) => t,
            None => match self.self_type(from).0 {
                Some(n) => match self.find_type(fi, None, &n, from, line) {
                    Ty::Known(t) => t,
                    _ => return Vec::new(),
                },
                None => return Vec::new(),
            },
        };
        let ts = &self.syms[t];
        let mut seen: HashSet<&str> = HashSet::new();
        self.bases
            .get(&ts.name)
            .into_iter()
            .flatten()
            .filter(|b| seen.insert(b.as_str()) && *b != &ts.name)
            .map(|b| self.find_type(ts.file, None, b, Some(ts.id), ts.line))
            .collect()
    }

    /// `super.m()` (hint `B`): `m` looked up on the bases of the enclosing type, never on
    /// the type itself. The first base (in declaration order) that has it wins; exact when
    /// that base declares it, likely when inherited further up. Every base outside the
    /// repo: the call does not land in the repo.
    fn resolve_super(&self, e: &E) -> Typed {
        let bases = self.base_types(e.file, e.from_symbol, e.line);
        let mut all_external = !bases.is_empty();
        for b in bases {
            match b {
                Ty::Known(t) => {
                    all_external = false;
                    if let Some((m, via)) = self.method_on(t, &e.name, &mut HashSet::new(), 0) {
                        let c = if via {
                            Confidence::Likely
                        } else {
                            Confidence::Exact
                        };
                        return Typed::Resolved(Some((m, c)));
                    }
                }
                Ty::External(_) => {}
                Ty::Unknown => all_external = false,
            }
        }
        if all_external {
            Typed::Resolved(None)
        } else {
            Typed::NoOpinion
        }
    }

    /// JS/TS `v.m()` / `ns.v.m()` with no receiver hint, where `v` is a module-level value
    /// of the repo (`computed.make_(..)`, `mobx.observable.box(..)`): `m` on its type.
    pub(super) fn resolve_value_member(&self, e: &E, q: &str) -> Typed {
        if !matches!(e.kind, EdgeKind::Call | EdgeKind::Value) {
            return Typed::NoOpinion;
        }
        let Some(v) = self.value_val(e.file, q, 0) else {
            return Typed::NoOpinion;
        };
        match self.bind_val(&v, 0) {
            Ty::Known(t) => match self.method_on(t, &e.name, &mut HashSet::new(), 0) {
                Some((m, via_base)) => Typed::Resolved(Some((
                    m,
                    if via_base {
                        Confidence::Likely
                    } else {
                        Confidence::Exact
                    },
                ))),
                None => Typed::NoOpinion,
            },
            Ty::External(_) => Typed::Resolved(None),
            Ty::Unknown => Typed::NoOpinion,
        }
    }

    /// Among several impls of a std-trait method (`From<&[u8]>`, `From<Vec<u8>>`,
    /// `From<Bytes>`), the one whose first parameter fits the call's first argument
    /// (`E::arg`). `None` unless exactly one fits and every candidate's parameter is
    /// readable from its one-line signature and concrete (no generic, no `impl Trait`).
    pub(super) fn overload_pick(&self, e: &E, cands: &[usize]) -> Option<usize> {
        let arg = self.arg_shape(e)?;
        let mut hit = None;
        for &c in cands {
            let s = &self.syms[c];
            let p = Shape::parse(first_param_type(&s.sig, &s.name)?);
            if p.opaque() {
                return None;
            }
            if arg.fits(&p) {
                if hit.is_some() {
                    return None;
                }
                hit = Some(c);
            }
        }
        hit
    }

    /// Shape of a call's first argument from its `arg` hint.
    fn arg_shape(&self, e: &E) -> Option<Shape> {
        let h = e.arg.as_deref()?;
        let shape = match self.eval_val(e.file, e.from_symbol, e.line, h, 0) {
            Val::Ty(Ty::Known(s)) => Shape::Named(self.syms[s].name.clone(), Vec::new()),
            Val::Ty(Ty::External(Some(id))) => {
                Shape::parse(self.ext_names.get(id as usize)?.as_str())
            }
            Val::Ty(_) => return None,
            Val::Text {
                from,
                text,
                unwrap,
                elem,
                ..
            } => {
                let text = if unwrap {
                    unwrap_text(Lang::Rust, &text)
                } else {
                    text
                };
                let text = if elem {
                    element_of(Lang::Rust, &text)?
                } else {
                    text
                };
                match Shape::parse(&text) {
                    Shape::Named(h, _) if h == "Self" => {
                        Shape::Named(self.self_type(from).0?, Vec::new())
                    }
                    s => s,
                }
            }
        };
        match &shape {
            Shape::Named(h, _) if looks_generic(h) => None,
            Shape::Other | Shape::Wild => None,
            _ => Some(shape),
        }
    }

    /// `R::discard(..)` / `F::as_cast(..)` where `R` is a generic parameter of the
    /// enclosing function or impl with a single trait bound: the method on that trait
    /// (`NoOpinion` = the qualifier is no such generic).
    pub(super) fn rust_generic_path(&self, e: &E, q: &str) -> Typed {
        let Some((gfi, gid, h)) = self.generic_bound(e.from_symbol, q) else {
            return Typed::NoOpinion;
        };
        let h = h.to_string();
        match self.bind_val(&self.eval_val(gfi, Some(gid), 0, &h, 0), 0) {
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
            Ty::External(_) => Typed::Resolved(None),
            Ty::Unknown => Typed::NoOpinion,
        }
    }

    /// `Trait::m(x, ..)` / `a::Trait::m(x, ..)`: `x.m(..)` among the impls of `Trait` for
    /// `x`'s type (`Display::fmt(&self.code, f)`, `ser::SerializeSeq::end(self)`). Only
    /// when the qualifier is no in-repo type with a method `m`, `x`'s type is known and
    /// exactly one of its methods named `m` implements `Trait`.
    pub(super) fn rust_ufcs(&self, e: &E, q: &str) -> Option<(usize, Confidence)> {
        let tr = q.rsplit("::").next()?;
        if tr == "Self" || !tr.starts_with(|c: char| c.is_ascii_uppercase()) {
            return None;
        }
        if self
            .by_member
            .contains_key(&(tr.to_string(), e.name.clone()))
        {
            return None;
        }
        let h = e.arg.as_deref()?;
        let Ty::Known(t) = self.bind_val(&self.eval_val(e.file, e.from_symbol, e.line, h, 0), 0)
        else {
            return None;
        };
        let ts = &self.syms[t];
        let pkg = &self.files[ts.file].package;
        let m: Vec<usize> = self
            .by_member
            .get(&(ts.name.clone(), e.name.clone()))
            .into_iter()
            .flatten()
            .copied()
            .filter(|&m| {
                let ms = &self.syms[m];
                self.files[ms.file].package == *pkg
                    && ms
                        .fields
                        .iter()
                        .any(|(k, v)| k == crate::extract::IMPL_TRAIT && same_trait(v, q))
                    && takes_self(&ms.sig, &ms.name)
            })
            .collect();
        match m[..] {
            [one] => Some((one, Confidence::Exact)),
            _ => None,
        }
    }

    /// Resolve a receiver call through its type hint.
    pub(super) fn resolve_typed(&self, e: &E) -> Typed {
        let Some(hint) = e.hint.as_deref() else {
            return Typed::NoOpinion;
        };
        if hint == "L" || !matches!(e.kind, EdgeKind::Call | EdgeKind::Value) {
            return Typed::NoOpinion;
        }
        if hint == "B" {
            return self.resolve_super(e);
        }
        if let Some((f, a, b)) = self.object_at(e.file, e.from_symbol, hint) {
            let m = self.by_file[f]
                .iter()
                .copied()
                .filter(|&s| {
                    let sym = &self.syms[s];
                    sym.kind == SymKind::Method && sym.name == e.name && (a..=b).contains(&sym.line)
                })
                .min_by_key(|&s| (self.syms[s].line, s));
            return Typed::Resolved(m.map(|m| (m, Confidence::Exact)));
        }
        match self.eval_hint(e.file, e.from_symbol, e.line, hint) {
            Ty::Unknown => Typed::NoOpinion,
            Ty::External(Some(id)) => Typed::Resolved(
                self.extension_method(e.file, id, &e.name)
                    .map(|m| (m, Confidence::Exact)),
            ),
            Ty::External(None) => Typed::Resolved(None),
            Ty::Known(t) if e.arg.is_some() => {
                // `a.partial_cmp(b)` with several `PartialOrd<X> for A`: pick by argument.
                let ts = &self.syms[t];
                let pkg = &self.files[ts.file].package;
                let own: Vec<usize> = self
                    .by_member
                    .get(&(ts.name.clone(), e.name.clone()))
                    .into_iter()
                    .flatten()
                    .copied()
                    .filter(|&m| {
                        let mf = &self.files[self.syms[m].file];
                        mf.lang == Lang::Rust && mf.package == *pkg
                    })
                    .collect();
                match own.len() {
                    0 | 1 => Typed::Resolved(
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
                    _ => {
                        Typed::Resolved(self.overload_pick(e, &own).map(|m| (m, Confidence::Exact)))
                    }
                }
            }
            Ty::Known(t) => {
                let found =
                    self.method_on(t, &e.name, &mut HashSet::new(), 0)
                        .map(|(m, via_base)| {
                            let c = if via_base {
                                Confidence::Likely
                            } else {
                                Confidence::Exact
                            };
                            (m, c)
                        });
                if found.is_none()
                    && self.files[e.file].lang == Lang::Python
                    && !self.declares_field(t, &e.name, 0)
                {
                    // Not on the static type: an `isinstance` narrowing to a subclass.
                    return Typed::Resolved(
                        self.subclass_method(t, &e.name)
                            .map(|m| (m, Confidence::Likely)),
                    );
                }
                Typed::Resolved(found)
            }
        }
    }
}

/// Whether trait path `a` (an impl's, as written) and `b` (a call's qualifier) can name the
/// same trait: same last segment and, when both are qualified, the same module before it
/// (`fmt::Write` is no `io::Write`; a bare `Write` may be either).
fn same_trait(a: &str, b: &str) -> bool {
    let mut x = a.rsplit("::");
    let mut y = b.rsplit("::");
    if x.next() != y.next() {
        return false;
    }
    match (x.next(), y.next()) {
        (Some(p), Some(q)) => p == q,
        _ => true,
    }
}

/// Whether a Rust fn's first parameter is `self` (`&self`, `mut self`, `self: Box<Self>`),
/// read from its one-line signature. A signature whose parameters start on the next line
/// is taken as a method: rustfmt only breaks long parameter lists, and there `self` leads.
fn takes_self(sig: &str, name: &str) -> bool {
    let Some(at) = sig.find(&format!("fn {name}")) else {
        return false;
    };
    let rest = &sig[at..];
    let Some(open) = rest.find('(') else {
        return false;
    };
    let params = rest[open + 1..].trim_start();
    if params.is_empty() {
        return true;
    }
    let first = params.split([',', ')']).next().unwrap_or("").trim();
    let mut t = first.strip_prefix('&').unwrap_or(first).trim_start();
    if let Some(lt) = t.strip_prefix('\'') {
        t = lt
            .trim_start_matches(|c: char| c.is_alphanumeric() || c == '_')
            .trim_start();
    }
    let t = t.strip_prefix("mut ").unwrap_or(t).trim_start();
    t == "self" || t.starts_with("self:") || t.starts_with("self ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trait_paths_must_agree_on_their_module() {
        assert!(same_trait("fmt::Display", "Display"));
        assert!(same_trait("Display", "fmt::Display"));
        assert!(same_trait("fmt::Write", "std::fmt::Write"));
        assert!(!same_trait("fmt::Write", "io::Write"));
        assert!(!same_trait("Display", "Debug"));
    }

    #[test]
    fn takes_self_reads_the_first_parameter() {
        assert!(takes_self(
            "fn fmt(&self, f: &mut Formatter) -> Result {",
            "fmt"
        ));
        assert!(takes_self("fn end(self) -> Result<Value> {", "end"));
        assert!(takes_self("pub fn set(&'a mut self, v: u8) {", "set"));
        assert!(takes_self("fn put_u8(&mut self, n: u8) {", "put_u8"));
        assert!(takes_self("fn long(", "long"));
        assert!(!takes_self(
            "fn deserialize<D>(deserializer: D) -> Result<Value, D::Error>",
            "deserialize"
        ));
        assert!(!takes_self("fn new() -> Self {", "new"));
        assert!(!takes_self("fn from(n: i32) -> Self {", "from"));
        assert!(!takes_self("fn f(self_x: u8) {", "f"));
        assert!(takes_self("fn boxed(self: Box<Self>) {", "boxed"));
    }

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
        let counts = |sig: &str, name: &str| go_shape(sig, name).map(|s| (s.params, s.results));
        assert_eq!(
            counts(
                "func (n *Notifier) Send(ctx context.Context, m Message, sinks []string) error {",
                "Send"
            ),
            Some((3, 1))
        );
        assert_eq!(counts("Flush() error", "Flush"), Some((0, 1)));
        assert_eq!(
            counts("func (f *flushWriter) Flush() {", "Flush"),
            Some((0, 0))
        );
        assert_eq!(counts("Get(a, b int) (int, error)", "Get"), Some((2, 2)));
        assert_eq!(counts("func (s *S) Run(", "Run"), None);
        assert_eq!(
            counts("func (testPrintLogger) Print(v ...any) {}", "Print"),
            Some((1, 0))
        );
        assert_eq!(
            counts("func f() (map[string]interface{}, error) {", "f"),
            Some((0, 2))
        );
        assert_eq!(
            counts("func (x T) Get() interface{} { return nil }", "Get"),
            Some((0, 1))
        );
    }

    #[test]
    fn go_parameter_types_decide_implementation() {
        let shape = |sig: &str, name: &str| go_shape(sig, name).unwrap();
        let t = |v: &[Option<&str>]| v.iter().map(|x| x.map(str::to_string)).collect::<Vec<_>>();
        assert_eq!(
            shape("Get(a, b int, c *pkg.Conf) error", "Get").types,
            t(&[Some("int"), Some("int"), Some("*Conf")])
        );
        assert_eq!(
            shape("Create(T, *T, C) Value", "Create").types,
            t(&[None, None, None])
        );
        assert_eq!(
            shape(
                "func (c *Cfg) Watch(ch chan int, f func(int) error) {",
                "Watch"
            )
            .types,
            t(&[Some("chanint"), None])
        );
        // `setMultiValueParsingConfig(f Flag)` is no `setMultiValueParsingConfig(c multiValueParsingConfig)`.
        let iface = shape(
            "setMultiValueParsingConfig(c multiValueParsingConfig)",
            "setMultiValueParsingConfig",
        );
        let other = shape(
            "func (cmd *Command) setMultiValueParsingConfig(f Flag) {",
            "setMultiValueParsingConfig",
        );
        assert!(!other.fits(&iface));
        // Qualified in one package, bare in the other: the same type.
        let iface = shape("ToMiddleware() (MiddlewareFunc, error)", "ToMiddleware");
        let imp = shape(
            "func (c RecoverConfig) ToMiddleware() (echo.MiddlewareFunc, error) {",
            "ToMiddleware",
        );
        assert!(imp.fits(&iface));
        let iface = shape("Handle(ctx context.Context, v interface{})", "Handle");
        let imp = shape("func (h H) Handle(c context.Context, v any) {", "Handle");
        assert!(imp.fits(&iface));
    }

    #[test]
    fn ts_function_types() {
        assert_eq!(
            fn_type_parts("<T=any>(initialValues?:T[],options?:O)=>IObservableArray<T>"),
            Some(('n', "IObservableArray<T>".to_string()))
        );
        assert_eq!(
            fn_type_parts("<T>(fn:()=>T,o?:O)=>Box<T>"),
            Some(('f', "Box<T>".to_string()))
        );
        assert_eq!(fn_type_parts("<T>(v:T)=>T"), Some(('g', "T".to_string())));
        assert_eq!(fn_type_parts("()=>void"), Some(('0', "void".to_string())));
        assert_eq!(fn_type_parts("Map<K,V>"), None);
        assert!(shape_ok(Some('f'), 'g') && !shape_ok(Some('f'), 'n'));
        assert!(shape_ok(Some('o'), 'n') && !shape_ok(Some('o'), 'f'));
        assert_eq!(split_top("a:()=>T,b:X", ','), vec!["a:()=>T", "b:X"]);
        assert_eq!(map_value("Map<string,Box<T>>|undefined"), Some("Box<T>"));
        assert_eq!(map_value("WeakMap<object,()=>Foo>"), Some("()=>Foo"));
        assert_eq!(map_value("Set<X>"), None);
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

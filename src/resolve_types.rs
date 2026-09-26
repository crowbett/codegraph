//! Receiver types: evaluate the extraction-time hints of [`crate::infer`] against the
//! whole repo, so `x.method()` resolves through the type of `x` instead of by name.
//!
//! A hint evaluates to a type symbol in this repo (`Known`), a type provably outside the
//! repo (`External`: stdlib, a dependency, a primitive; the call then resolves to nothing
//! instead of a same-named repo method), or `Unknown` (fall back to name rules).

use std::collections::HashSet;

use super::{Confidence, Ctx, EdgeKind, Lang, SymKind, E};

/// Evaluated receiver type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Ty {
    Known(usize),
    External,
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
        if let Some(&t) = self.memo.borrow().get(&key) {
            return t;
        }
        let v = self.eval_val(fi, from, hint, 0);
        let t = self.bind_val(&v, 0);
        self.memo.borrow_mut().insert(key, t);
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
                } => Val::Text {
                    fi,
                    from,
                    text,
                    unwrap: unwrap || is_q,
                    awaited: awaited || !is_q,
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
                if lang == Lang::Rust && PASSTHROUGH.contains(&name) {
                    return self.eval_val(fi, from, base, d);
                }
                if lang == Lang::Rust && UNWRAP.contains(&name) {
                    return self.eval_val(fi, from, &format!("Q:{base}"), d);
                }
                match self.bind_val(&self.eval_val(fi, from, base, d), d) {
                    Ty::Known(t) => match self.method_on(t, name, &mut HashSet::new(), d) {
                        Some((m, _)) => match self.syms[m].ret.clone() {
                            Some(r) => {
                                let mf = self.syms[m].file;
                                self.eval_val(mf, Some(self.syms[m].id), &r, d)
                            }
                            None => Val::Ty(Ty::Unknown),
                        },
                        None => Val::Ty(Ty::External),
                    },
                    other => Val::Ty(other),
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
        Val::Ty(if external { Ty::External } else { Ty::Unknown })
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
            } => {
                let lang = self.files[*fi].lang;
                match normalize(lang, text, *unwrap, *awaited, 0) {
                    Norm::External => Ty::External,
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
                            Some(t) if t.is_empty() => Ty::External,
                            _ if matches!(head, "std" | "core" | "alloc") => Ty::External,
                            _ => Ty::Unknown,
                        }
                    }
                };
            }
            return match scope.modules.get(q) {
                Some(t) if t.is_empty() => Ty::External,
                Some(t) => self.pick_type(fi, &self.type_syms_in(t, name)),
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
                return Ty::External;
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
            Lang::Rust if !scope.globs.is_empty() => {
                let g = self.type_syms_in(&scope.globs, name);
                if g.is_empty() {
                    Ty::Unknown
                } else {
                    self.pick_type(fi, &g)
                }
            }
            _ if all.is_empty() && builtin_type(lang, name) => Ty::External,
            // A lowercase Go name that is not a type of this package is a builtin or a
            // package we can't see; TS/Python types that exist nowhere are globals.
            _ if all.is_empty() && lang != Lang::Rust => Ty::External,
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
            .by_name
            .get(name)
            .into_iter()
            .flatten()
            .copied()
            .filter(|&s| {
                let m = &self.syms[s];
                let mf = &self.files[m.file];
                m.kind == SymKind::Method
                    && fam(mf.lang)
                    && m.container.as_deref() == Some(ts.name.as_str())
                    && match tf.lang {
                        Lang::Go | Lang::Rust => mf.package == tf.package,
                        _ => m.parent == Some(ts.id),
                    }
            })
            .collect();
        if let Some(b) = self.best(ts.file, &own) {
            return Some((b, false));
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
        for base in self.bases.get(&ts.name).into_iter().flatten() {
            let Ty::Known(b) = self.find_type(ts.file, None, base, Some(ts.id)) else {
                continue;
            };
            if let Some((m, _)) = self.method_on(b, name, seen, d + 1) {
                return Some((m, true));
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
            Ty::External => Typed::Resolved(None),
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

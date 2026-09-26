//! Per-file extraction: run the language's tag query over a tree-sitter parse and
//! turn captures into symbols (with nesting, containers, docs, signatures),
//! references (calls, type refs, impl/inherit) and imports.
//!
//! Everything here is file-local; cross-file resolution happens in [`crate::resolve`].

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use streaming_iterator::StreamingIterator;
use tree_sitter::{Node, QueryCursor};

use crate::error::Result;
use crate::infer::Infer;
use crate::lang::Lang;
use crate::model::{EdgeKind, SymKind};

/// Longest signature line kept.
const MAX_SIG: usize = 200;
/// Longest doc line kept.
const MAX_DOC: usize = 160;
/// Longest qualifier text kept (longer expressions are treated as opaque receivers).
const MAX_QUALIFIER: usize = 80;

/// A definition found in a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawSymbol {
    /// Simple name.
    pub name: String,
    /// Kind.
    pub kind: SymKind,
    /// Owning type name for methods (Go receiver, Rust impl type, class, C++ scope).
    pub container: Option<String>,
    /// Index of the enclosing definition in the same file.
    pub parent: Option<usize>,
    /// Qualified name within the file (`Type.method`, `outer::inner`).
    pub local_qname: String,
    /// 1-based start line.
    pub start_line: u32,
    /// 1-based start column.
    pub start_col: u32,
    /// 1-based end line.
    pub end_line: u32,
    /// 1-based end column.
    pub end_col: u32,
    /// First source line of the definition, trimmed.
    pub signature: String,
    /// First line of the doc comment / docstring.
    pub doc: Option<String>,
    /// Visible outside its module/package (heuristic per language).
    pub exported: bool,
    /// `export default` (JS/TS).
    pub default_export: bool,
    /// Receiver variable name (Go `func (s *Store) ...` -> `s`).
    pub recv: Option<String>,
    /// Return type hint (functions/methods), see [`crate::infer`].
    pub ret: Option<String>,
    /// Field name -> type hint (structs/classes); Go embedded fields are `~Type`.
    pub fields: Vec<(String, String)>,
    start_byte: usize,
    end_byte: usize,
}

/// A reference (call, type use, impl, inherit) found in a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawRef {
    /// Referenced simple name.
    pub name: String,
    /// Qualifier text: `pkg` in `pkg.Fn()`, `self`, `a::b`, receiver expression.
    pub qualifier: Option<String>,
    /// Edge kind.
    pub kind: EdgeKind,
    /// 1-based line.
    pub line: u32,
    /// 1-based column.
    pub col: u32,
    /// Index of the innermost enclosing definition (None = file level).
    pub from: Option<usize>,
    /// For `impl Trait for Type`: the implementing type's name.
    pub from_name: Option<String>,
    /// Receiver type hint for calls/values (see [`crate::infer`]); `L` on a bare name
    /// means it is a local variable.
    pub hint: Option<String>,
    /// Method-call syntax (`x.f()`), as opposed to a path (`x::f()`); Rust only.
    pub dot: bool,
}

/// How an import binds a local name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BindKind {
    /// The whole module under a local name (`import x "p"`, `import * as x`, `import a.b as x`).
    Module,
    /// One named item (`from m import x`, `use a::x`, `import {x}`).
    Name,
    /// The default export (`import x from 'm'`).
    Default,
    /// Everything (`use a::*`, `from m import *`, Go dot import).
    Glob,
}

/// A local name introduced by an import.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Binding {
    /// Binding kind.
    pub kind: BindKind,
    /// Original item name (for `Name`), empty otherwise.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// Local name in this file (empty for Go imports without alias: filled by the resolver).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub local: String,
}

/// One import / use / include.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawImport {
    /// Module path as written (`"./util"`, `crate::a::b`, `..pkg.mod`, `stdio.h`).
    pub path: String,
    /// 1-based line.
    pub line: u32,
    /// Bound names.
    pub bindings: Vec<Binding>,
    /// `#include <...>` (never resolved inside the repo first).
    pub system: bool,
}

/// Everything extracted from one file.
#[derive(Debug, Clone, Default)]
pub struct FileFacts {
    /// Declared package (Go).
    pub package: Option<String>,
    /// Definitions in source order.
    pub symbols: Vec<RawSymbol>,
    /// References.
    pub refs: Vec<RawRef>,
    /// Imports.
    pub imports: Vec<RawImport>,
    /// Line count.
    pub lines: u32,
    /// tree-sitter reported syntax errors (extraction still ran).
    pub has_errors: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    Def(SymKind),
    Ref(EdgeKind),
    Import,
    Export,
    Package,
}

struct DefCand<'t> {
    kind: SymKind,
    node: Node<'t>,
    name: Node<'t>,
    scope: Option<Node<'t>>,
    receiver: Option<Node<'t>>,
}

struct RefCand<'t> {
    kind: EdgeKind,
    name: Node<'t>,
    qualifier: Option<Node<'t>>,
    impl_for: Option<Node<'t>>,
}

fn text<'a>(node: Node<'_>, src: &'a [u8]) -> &'a str {
    node.utf8_text(src).unwrap_or("")
}

fn line_of(node: Node<'_>) -> u32 {
    node.start_position().row as u32 + 1
}

/// Truncate on a char boundary.
pub(crate) fn clip(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

/// Parse `src` as `lang` and extract symbols, references and imports.
pub fn extract(lang: Lang, src: &[u8]) -> Result<FileFacts> {
    // TS: retry with signature separators when the grammar trips (see tsfix).
    if matches!(lang, Lang::TypeScript | Lang::Tsx) {
        if let Some(patched) = crate::tsfix::separate_signatures(src) {
            let errs = |s: &[u8]| -> Result<Option<usize>> {
                Ok(lang
                    .with_parser(|p| p.parse(s, None))?
                    .map(|t| count_errors(t.root_node())))
            };
            if let (Some(before), Some(after)) = (errs(src)?, errs(&patched)?) {
                if after < before {
                    return extract_inner(lang, &patched);
                }
            }
        }
    }
    extract_inner(lang, src)
}

/// ERROR / MISSING nodes in a tree.
fn count_errors(root: Node<'_>) -> usize {
    if !root.has_error() {
        return 0;
    }
    let mut n = 0;
    let mut cur = root.walk();
    loop {
        let node = cur.node();
        if node.is_error() || node.is_missing() {
            n += 1;
        }
        if node.has_error() && cur.goto_first_child() {
            continue;
        }
        loop {
            if cur.goto_next_sibling() {
                break;
            }
            if !cur.goto_parent() {
                return n;
            }
        }
    }
}

fn extract_inner(lang: Lang, src: &[u8]) -> Result<FileFacts> {
    let query = lang.query()?;
    let Some(tree) = lang.with_parser(|p| p.parse(src, None))? else {
        return Ok(FileFacts::default());
    };
    let root = tree.root_node();
    let names = query.capture_names();

    let mut defs: Vec<DefCand<'_>> = Vec::new();
    let mut def_seen: std::collections::HashSet<usize> = std::collections::HashSet::new();
    let mut refs: HashMap<usize, RefCand<'_>> = HashMap::new();
    let mut import_nodes: Vec<Node<'_>> = Vec::new();
    let mut export_nodes: Vec<Node<'_>> = Vec::new();
    let mut package = None;

    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, root, src);
    while let Some(m) = matches.next() {
        let mut role: Option<(Role, Node<'_>)> = None;
        let (mut name, mut qualifier, mut receiver, mut decl, mut impl_for) =
            (None, None, None, None, None);
        for cap in m.captures() {
            let cname = names[cap.index as usize];
            let n = cap.node;
            match cname {
                "name" => name = Some(n),
                "qualifier" => qualifier = Some(n),
                "receiver" => receiver = Some(n),
                "decl" => decl = Some(n),
                "impl_for" => impl_for = Some(n),
                "import" => role = Some((Role::Import, n)),
                "export" => role = Some((Role::Export, n)),
                "package" => role = Some((Role::Package, n)),
                c => {
                    if let Some(k) = c.strip_prefix("definition.") {
                        role = def_kind(k).map(|k| (Role::Def(k), n));
                    } else if let Some(k) = c.strip_prefix("reference.") {
                        role = ref_kind(k).map(|k| (Role::Ref(k), n));
                    }
                }
            }
        }
        let Some((role, node)) = role else { continue };
        match role {
            Role::Package => package = name.map(|n| text(n, src).to_string()),
            Role::Import => import_nodes.push(node),
            Role::Export => export_nodes.push(node),
            Role::Def(kind) => {
                if def_seen.contains(&node.id()) {
                    continue;
                }
                let (name, scope) = match (name, decl) {
                    (Some(n), _) => (n, None),
                    (None, Some(d)) => match declarator_name(d) {
                        Some(x) => x,
                        None => continue,
                    },
                    (None, None) => continue,
                };
                if text(name, src).is_empty() {
                    continue;
                }
                def_seen.insert(node.id());
                defs.push(DefCand {
                    kind,
                    node,
                    name,
                    scope,
                    receiver,
                });
            }
            Role::Ref(kind) => {
                let Some(name) = name else { continue };
                if lang == Lang::Rust && in_attribute_or_macro_def(name) {
                    continue;
                }
                let cand = RefCand {
                    kind,
                    name,
                    qualifier,
                    impl_for,
                };
                let key = name.start_byte();
                let better = match refs.get(&key) {
                    None => true,
                    Some(old) => ref_priority(&cand) > ref_priority(old),
                };
                if better {
                    refs.insert(key, cand);
                }
            }
        }
    }

    let mut facts = FileFacts {
        package,
        lines: count_lines(src),
        has_errors: root.has_error(),
        ..FileFacts::default()
    };

    build_symbols(lang, src, &mut defs, &mut facts);
    apply_exports(lang, src, &export_nodes, &defs, &mut facts.symbols);

    let mut infer = Infer::new(lang, root, src);
    let def_nodes: HashMap<(usize, usize), Node<'_>> = defs
        .iter()
        .map(|d| ((d.node.start_byte(), d.node.end_byte()), d.node))
        .collect();
    for sym in &mut facts.symbols {
        let Some(&node) = def_nodes.get(&(sym.start_byte, sym.end_byte)) else {
            continue;
        };
        if matches!(sym.kind, SymKind::Function | SymKind::Method) {
            sym.ret = infer.ret_of(node);
        } else if matches!(
            sym.kind,
            SymKind::Struct | SymKind::Class | SymKind::Interface
        ) {
            sym.fields = infer.fields_of(node);
        }
    }

    for node in import_nodes {
        parse_import(lang, node, src, &mut facts.imports);
    }
    facts.imports.sort_by_key(|i| i.line);

    // Bare identifiers passed as values are kept only when they name something
    // defined or imported in this file (everything else is a local variable).
    let known: std::collections::HashSet<String> = facts
        .symbols
        .iter()
        .map(|s| s.name.clone())
        .chain(
            facts
                .imports
                .iter()
                .flat_map(|i| i.bindings.iter())
                .filter(|b| matches!(b.kind, BindKind::Name | BindKind::Default))
                .map(|b| {
                    if b.local.is_empty() {
                        b.name.clone()
                    } else {
                        b.local.clone()
                    }
                }),
        )
        .collect();

    // References: drop the ones that sit on a definition's own name.
    let def_names: std::collections::HashSet<usize> =
        defs.iter().map(|d| d.name.start_byte()).collect();
    let mut ref_list: Vec<RefCand<'_>> = refs
        .into_values()
        .filter(|r| !def_names.contains(&r.name.start_byte()))
        .collect();
    ref_list.sort_by_key(|r| r.name.start_byte());
    for r in ref_list {
        let name = text(r.name, src).to_string();
        if name.is_empty()
            || name == "_"
            || (r.qualifier.is_none() && is_builtin(lang, r.kind, &name))
        {
            continue;
        }
        if r.kind == EdgeKind::Ref
            && r.qualifier.is_none()
            && name.len() == 1
            && name.chars().all(|c| c.is_ascii_uppercase())
        {
            // Generic type parameters (`T`, `K`, `V`).
            continue;
        }
        if r.kind == EdgeKind::Value && r.qualifier.is_none() && !known.contains(&name) {
            continue;
        }
        let pos = r.name.start_position();
        let from = enclosing(&facts.symbols, r.name.start_byte());
        let from_name = r.impl_for.map(|n| base_type_name(text(n, src)));
        let (hint, dot) = if matches!(r.kind, EdgeKind::Call | EdgeKind::Value) {
            match r.qualifier {
                // A Rust path qualifier (`m::f`, `Type::f`) is never a value.
                Some(q) if lang == Lang::Rust && !is_dot_receiver(q) => (None, false),
                Some(q) if q.parent().is_some_and(|p| p.kind() == "token_tree") => {
                    (infer.token_receiver_hint(q), true)
                }
                Some(q) => (
                    infer.qualifier_hint(q),
                    lang == Lang::Rust && is_dot_receiver(q),
                ),
                None => (infer.bare_is_local(r.name).then(|| "L".to_string()), false),
            }
        } else {
            (None, false)
        };
        facts.refs.push(RawRef {
            name,
            qualifier: r.qualifier.map(|q| normalize_qualifier(text(q, src))),
            kind: r.kind,
            line: pos.row as u32 + 1,
            col: pos.column as u32 + 1,
            from: if from_name.is_some() { None } else { from },
            from_name,
            hint,
            dot,
        });
    }

    Ok(facts)
}

/// `x` in `x.f()` (a field expression or a `.` token in a macro token tree).
fn is_dot_receiver(q: Node<'_>) -> bool {
    match q.parent() {
        Some(p) if p.kind() == "field_expression" => true,
        Some(p) if p.kind() == "token_tree" => q.next_sibling().is_some_and(|n| n.kind() == "."),
        _ => false,
    }
}

/// Rust tokens inside `#[attr(...)]` or a `macro_rules!` body are not calls even when they
/// look like `f(..)` (only token-tree matches can land there).
fn in_attribute_or_macro_def(name: Node<'_>) -> bool {
    if name.parent().is_none_or(|p| p.kind() != "token_tree") {
        return false;
    }
    let mut cur = name.parent();
    while let Some(n) = cur {
        match n.kind() {
            "attribute_item"
            | "inner_attribute_item"
            | "attribute"
            | "macro_definition"
            | "macro_rule" => {
                return true;
            }
            "token_tree" | "macro_invocation" => cur = n.parent(),
            _ => return false,
        }
    }
    false
}

/// Language builtins that can never resolve inside a repo; dropped at extraction to
/// keep the graph lean (only when unqualified).
fn is_builtin(lang: Lang, kind: EdgeKind, name: &str) -> bool {
    let calls: &[&str] = match lang {
        Lang::Go => &[
            "len", "cap", "append", "make", "new", "panic", "recover", "print", "println", "copy",
            "delete", "close", "complex", "real", "imag", "min", "max", "clear",
        ],
        Lang::Rust => &[
            "println",
            "print",
            "eprintln",
            "eprint",
            "format",
            "write",
            "writeln",
            "vec",
            "assert",
            "assert_eq",
            "assert_ne",
            "debug_assert",
            "debug_assert_eq",
            "debug_assert_ne",
            "panic",
            "todo",
            "unimplemented",
            "unreachable",
            "matches",
            "format_args",
            "concat",
            "stringify",
            "include_str",
            "include_bytes",
            "env",
            "option_env",
            "cfg",
            "dbg",
            "Ok",
            "Err",
            "Some",
            "None",
            "line",
            "file",
            "column",
        ],
        Lang::Python => &[
            "print",
            "len",
            "range",
            "isinstance",
            "issubclass",
            "str",
            "int",
            "float",
            "bool",
            "dict",
            "list",
            "set",
            "tuple",
            "frozenset",
            "super",
            "open",
            "getattr",
            "setattr",
            "hasattr",
            "delattr",
            "enumerate",
            "zip",
            "sorted",
            "reversed",
            "min",
            "max",
            "sum",
            "any",
            "all",
            "type",
            "repr",
            "iter",
            "next",
            "map",
            "filter",
            "abs",
            "round",
            "id",
            "vars",
            "dir",
            "hash",
            "callable",
            "format",
            "ord",
            "chr",
            "bytes",
            "bytearray",
            "divmod",
            "pow",
            "input",
            "globals",
            "locals",
            "object",
            "Exception",
            "ValueError",
            "TypeError",
            "KeyError",
            "RuntimeError",
            "NotImplementedError",
            "OSError",
        ],
        Lang::TypeScript | Lang::Tsx | Lang::JavaScript => &[
            "require",
            "parseInt",
            "parseFloat",
            "String",
            "Number",
            "Boolean",
            "Array",
            "Object",
            "Symbol",
            "Promise",
            "Error",
            "TypeError",
            "setTimeout",
            "setInterval",
            "clearTimeout",
            "clearInterval",
            "isNaN",
            "encodeURIComponent",
            "decodeURIComponent",
            "structuredClone",
            "BigInt",
            "Date",
            "Map",
            "Set",
            "RegExp",
            "URL",
            "URLSearchParams",
        ],
        Lang::C | Lang::Cpp => &[
            "printf",
            "fprintf",
            "sprintf",
            "snprintf",
            "puts",
            "malloc",
            "calloc",
            "realloc",
            "free",
            "memcpy",
            "memmove",
            "memset",
            "memcmp",
            "strlen",
            "strcmp",
            "strncmp",
            "strcpy",
            "strncpy",
            "strcat",
            "exit",
            "abort",
            "assert",
            "static_cast",
            "dynamic_cast",
            "reinterpret_cast",
            "const_cast",
        ],
    };
    let types: &[&str] = match lang {
        Lang::Go => &[
            "string",
            "error",
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
            "any",
            "comparable",
        ],
        Lang::Rust => &["Self"],
        Lang::Python => &[
            "str", "int", "float", "bool", "bytes", "object", "dict", "list", "set", "tuple",
        ],
        Lang::TypeScript | Lang::Tsx | Lang::JavaScript => &[
            "Record",
            "Partial",
            "Required",
            "Readonly",
            "Pick",
            "Omit",
            "Array",
            "Promise",
            "Map",
            "Set",
            "Date",
            "Error",
            "RegExp",
            "ReturnType",
            "Parameters",
            "Awaited",
            "NonNullable",
            "Exclude",
            "Extract",
            "ReadonlyArray",
        ],
        Lang::C | Lang::Cpp => &[
            "size_t",
            "ssize_t",
            "uint8_t",
            "uint16_t",
            "uint32_t",
            "uint64_t",
            "int8_t",
            "int16_t",
            "int32_t",
            "int64_t",
            "uintptr_t",
            "intptr_t",
            "FILE",
            "bool",
        ],
    };
    match kind {
        EdgeKind::Call => calls.contains(&name),
        EdgeKind::Ref => types.contains(&name),
        _ => false,
    }
}

#[allow(clippy::naive_bytecount)]
fn count_lines(src: &[u8]) -> u32 {
    let n = src.iter().filter(|&&b| b == b'\n').count() as u32;
    if src.last().is_some_and(|&b| b != b'\n') {
        n + 1
    } else {
        n
    }
}

fn def_kind(s: &str) -> Option<SymKind> {
    Some(match s {
        "function" => SymKind::Function,
        "method" => SymKind::Method,
        "class" => SymKind::Class,
        "struct" => SymKind::Struct,
        "interface" => SymKind::Interface,
        "trait" => SymKind::Trait,
        "enum" => SymKind::Enum,
        "type" => SymKind::Type,
        "const" | "constant" => SymKind::Const,
        "var" => SymKind::Var,
        "module" => SymKind::Module,
        "macro" => SymKind::Macro,
        _ => return None,
    })
}

fn ref_kind(s: &str) -> Option<EdgeKind> {
    Some(match s {
        "call" => EdgeKind::Call,
        "type" | "class" => EdgeKind::Ref,
        "impl" | "implementation" => EdgeKind::Impl,
        "inherit" => EdgeKind::Inherit,
        "value" => EdgeKind::Value,
        _ => return None,
    })
}

fn ref_priority(r: &RefCand<'_>) -> u8 {
    let base = match r.kind {
        EdgeKind::Impl | EdgeKind::Inherit => 6,
        EdgeKind::Call => 4,
        EdgeKind::Ref | EdgeKind::Import => 2,
        EdgeKind::Value => 1,
    };
    base + u8::from(r.qualifier.is_some())
}

/// Dig the name out of a C/C++ declarator chain. Returns (name, scope).
fn declarator_name(mut node: Node<'_>) -> Option<(Node<'_>, Option<Node<'_>>)> {
    let mut saw_fn = false;
    for _ in 0..16 {
        match node.kind() {
            "function_declarator" => {
                saw_fn = true;
                node = node.child_by_field_name("declarator")?;
            }
            "pointer_declarator"
            | "reference_declarator"
            | "parenthesized_declarator"
            | "attributed_declarator" => {
                node = node
                    .child_by_field_name("declarator")
                    .or_else(|| node.named_child(0))?;
            }
            "qualified_identifier" => {
                let scope = node.child_by_field_name("scope");
                let mut name = node.child_by_field_name("name")?;
                // Nested scopes: a::b::c -> descend to the innermost name.
                while name.kind() == "qualified_identifier" {
                    name = name.child_by_field_name("name")?;
                }
                if name.kind() == "template_function" {
                    name = name.child_by_field_name("name")?;
                }
                return saw_fn.then_some((name, scope.map(|_| node)));
            }
            "identifier" | "field_identifier" | "destructor_name" | "operator_name"
            | "type_identifier" => {
                return saw_fn.then_some((node, None));
            }
            "template_function" => {
                node = node.child_by_field_name("name")?;
            }
            _ => return None,
        }
    }
    None
}

/// `*Store[T]` / `&mut Vec<T>` / `Foo<'a>` -> `Store` / `Vec` / `Foo`.
pub(crate) fn base_type_name(s: &str) -> String {
    let s = s.trim();
    let cut = s.find(['<', '[', '(']).unwrap_or(s.len());
    let s = &s[..cut];
    let s = s.trim_start_matches(['*', '&', ' ']);
    let s = s.strip_prefix("mut ").unwrap_or(s);
    let s = s.strip_prefix("dyn ").unwrap_or(s);
    let last = s.rsplit("::").next().unwrap_or(s);
    let last = last.rsplit('.').next().unwrap_or(last);
    last.trim().to_string()
}

/// Remove whitespace and generic arguments; long/multiline expressions collapse.
fn normalize_qualifier(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut depth = 0i32;
    for ch in s.chars() {
        match ch {
            '<' => depth += 1,
            '>' if depth > 0 => depth -= 1,
            c if c.is_whitespace() => {}
            c if depth == 0 => out.push(c),
            _ => {}
        }
    }
    let out = out.trim_end_matches("::").to_string();
    if out.len() > MAX_QUALIFIER {
        "…".to_string()
    } else {
        out
    }
}

/// Innermost symbol whose byte range contains `pos`.
fn enclosing(symbols: &[RawSymbol], pos: usize) -> Option<usize> {
    // symbols are sorted by start_byte; find the last one starting at or before pos.
    let idx = symbols.partition_point(|s| s.start_byte <= pos);
    let mut cur = idx.checked_sub(1);
    while let Some(i) = cur {
        let s = &symbols[i];
        if s.start_byte <= pos && pos < s.end_byte {
            return Some(i);
        }
        cur = s.parent;
    }
    None
}

fn is_comment(kind: &str) -> bool {
    matches!(kind, "comment" | "line_comment" | "block_comment")
}

const WRAPPERS: [&str; 10] = [
    "type_declaration",
    "lexical_declaration",
    "variable_declaration",
    "export_statement",
    "template_declaration",
    "decorated_definition",
    "const_declaration",
    "var_declaration",
    "ambient_declaration",
    "expression_statement",
];

fn clean_comment_line(line: &str) -> &str {
    let l = line.trim();
    let l = l
        .trim_start_matches("///")
        .trim_start_matches("//!")
        .trim_start_matches("//")
        .trim_start_matches("/**")
        .trim_start_matches("/*!")
        .trim_start_matches("/*");
    let l = l.trim_end_matches("*/");
    let l = l.trim_start_matches('*').trim_start_matches('#');
    l.trim()
}

fn first_doc_line(block: &str) -> Option<String> {
    block
        .lines()
        .map(clean_comment_line)
        .find(|l| !l.is_empty())
        .map(|l| clip(l, MAX_DOC))
}

fn doc_for(lang: Lang, node: Node<'_>, src: &[u8]) -> Option<String> {
    if lang == Lang::Python {
        let body = node.child_by_field_name("body")?;
        let first = body.named_child(0)?;
        if first.kind() == "expression_statement" {
            let s = first.named_child(0)?;
            if s.kind() == "string" {
                let t = text(s, src)
                    .trim_start_matches(['r', 'R', 'u', 'U', 'b', 'B'])
                    .trim_matches(|c| c == '"' || c == '\'');
                return t
                    .lines()
                    .map(str::trim)
                    .find(|l| !l.is_empty())
                    .map(|l| clip(l, MAX_DOC));
            }
        }
        return None;
    }
    let mut anchor = node;
    while let Some(p) = anchor.parent() {
        if WRAPPERS.contains(&p.kind()) {
            anchor = p;
        } else {
            break;
        }
    }
    let mut sib = anchor.prev_sibling();
    // Skip attributes / decorators between the doc and the item.
    while let Some(s) = sib {
        if matches!(s.kind(), "attribute_item" | "decorator" | "attribute") {
            sib = s.prev_sibling();
        } else {
            break;
        }
    }
    let mut top: Option<Node<'_>> = None;
    let mut next_row = anchor.start_position().row;
    while let Some(s) = sib {
        if !is_comment(s.kind()) || s.end_position().row + 1 < next_row {
            break;
        }
        // Skip Rust inner docs (`//!`) and trailing comments of previous code.
        let t = text(s, src);
        if t.starts_with("//!") {
            break;
        }
        top = Some(s);
        next_row = s.start_position().row;
        sib = s.prev_sibling();
    }
    let top = top?;
    // Collect the block from `top` down to the anchor and return its first line.
    let start = top.start_byte();
    let end = anchor.start_byte();
    let block = std::str::from_utf8(src.get(start..end)?).ok()?;
    first_doc_line(block)
}

fn signature_line(node: Node<'_>, src: &[u8]) -> String {
    let start = node.start_byte();
    let line_start = src[..start]
        .iter()
        .rposition(|&b| b == b'\n')
        .map_or(0, |i| i + 1);
    let line_end = src[start..]
        .iter()
        .position(|&b| b == b'\n')
        .map_or(src.len(), |i| start + i);
    let line = String::from_utf8_lossy(&src[line_start..line_end]);
    let t = line.trim().trim_end_matches('{').trim_end();
    clip(t, MAX_SIG)
}

fn has_child_kind(node: Node<'_>, kind: &str) -> bool {
    let mut c = node.walk();
    let found = node.children(&mut c).any(|ch| ch.kind() == kind);
    found
}

fn is_exported(lang: Lang, node: Node<'_>, name: &str, src: &[u8]) -> bool {
    match lang {
        Lang::Go => name.chars().next().is_some_and(char::is_uppercase),
        Lang::Rust => has_child_kind(node, "visibility_modifier"),
        Lang::Python => !name.starts_with('_') || (name.starts_with("__") && name.ends_with("__")),
        Lang::C | Lang::Cpp => {
            let mut c = node.walk();
            let is_static = node
                .children(&mut c)
                .any(|ch| ch.kind() == "storage_class_specifier" && text(ch, src) == "static");
            !is_static
        }
        // JS/TS: decided by export statements (apply_exports).
        Lang::TypeScript | Lang::Tsx | Lang::JavaScript => false,
    }
}

/// Go receiver `(s *Store[T])` -> (`Store`, Some(`s`)).
fn go_receiver(recv: &str) -> (String, Option<String>) {
    let inner = recv
        .trim()
        .trim_start_matches('(')
        .trim_end_matches(')')
        .trim();
    let parts: Vec<&str> = inner.split_whitespace().collect();
    match parts.as_slice() {
        [ty] => (base_type_name(ty), None),
        [var, rest @ ..] => (base_type_name(&rest.join(" ")), Some((*var).to_string())),
        [] => (String::new(), None),
    }
}

/// Rust: the `impl` block a function sits directly in -> (type name, is trait impl).
fn rust_impl_of(node: Node<'_>, src: &[u8]) -> Option<(String, bool)> {
    let list = node.parent()?;
    if list.kind() != "declaration_list" {
        return None;
    }
    let imp = list.parent()?;
    if imp.kind() != "impl_item" {
        return None;
    }
    let ty = imp.child_by_field_name("type")?;
    Some((
        base_type_name(text(ty, src)),
        imp.child_by_field_name("trait").is_some(),
    ))
}

fn join(prefix: &str, sep: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_string()
    } else {
        format!("{prefix}{sep}{name}")
    }
}

fn build_symbols(lang: Lang, src: &[u8], defs: &mut [DefCand<'_>], facts: &mut FileFacts) {
    defs.sort_by(|a, b| {
        a.node
            .start_byte()
            .cmp(&b.node.start_byte())
            .then(b.node.end_byte().cmp(&a.node.end_byte()))
    });
    let sep = lang.sep();
    let mut stack: Vec<usize> = Vec::new();
    for (i, d) in defs.iter().enumerate() {
        let (sb, eb) = (d.node.start_byte(), d.node.end_byte());
        while let Some(&top) = stack.last() {
            let t: &RawSymbol = &facts.symbols[top];
            if t.start_byte <= sb && eb <= t.end_byte {
                break;
            }
            stack.pop();
        }
        let parent = stack.last().copied();
        let name = text(d.name, src).to_string();
        let mut kind = d.kind;
        let mut container: Option<String> = None;
        let mut recv = None;
        let mut exported = is_exported(lang, d.node, &name, src);
        let mut explicit_scope: Option<String> = None;

        let parent_sym = parent.map(|p| &facts.symbols[p]);
        if let Some(r) = d.receiver {
            let (ty, var) = go_receiver(text(r, src));
            if !ty.is_empty() {
                explicit_scope = Some(ty.clone());
                container = Some(ty);
            }
            recv = var;
        }
        if lang == Lang::Rust && kind == SymKind::Function {
            if let Some((ty, is_trait)) = rust_impl_of(d.node, src) {
                kind = SymKind::Method;
                exported |= is_trait;
                explicit_scope = Some(ty.clone());
                container = Some(ty);
            }
        }
        if let Some(scope) = d.scope {
            // C++ `void a::Foo::bar()`: scope is the qualified_identifier.
            if let Some(s) = scope.child_by_field_name("scope") {
                let s = normalize_qualifier(text(s, src));
                let last = base_type_name(&s);
                let class_like = last.chars().next().is_some_and(char::is_uppercase)
                    || facts
                        .symbols
                        .iter()
                        .any(|x| x.name == last && x.kind.is_type_like());
                if class_like {
                    kind = SymKind::Method;
                    container = Some(last);
                }
                explicit_scope = Some(s);
            }
        }
        if let Some(p) = parent_sym {
            if p.kind.is_type_like() && matches!(kind, SymKind::Function | SymKind::Method) {
                kind = SymKind::Method;
                container.get_or_insert_with(|| p.name.clone());
                if lang.is_js_family() || lang == Lang::Python {
                    exported = p.exported && !name.starts_with('#');
                }
            }
            if lang == Lang::Rust && p.kind == SymKind::Trait {
                exported = true;
            }
        }
        if lang.is_c_family()
            && d.node.kind() == "preproc_def"
            && d.node.child_by_field_name("value").is_none()
        {
            // Include guards and feature flags (`#define FOO_H`) are not API.
            continue;
        }
        if lang == Lang::Python && kind == SymKind::Const {
            // Only UPPER_CASE module-level assignments are treated as constants.
            if !name
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
            {
                continue;
            }
        }
        if lang.is_js_family() && kind == SymKind::Const && !name.chars().any(char::is_uppercase) {
            // Plain lowercase top-level values are too noisy; keep CONSTS and exported ones.
            let exported_decl = d
                .node
                .parent()
                .and_then(|p| p.parent())
                .is_some_and(|gp| gp.kind() == "export_statement");
            if !exported_decl {
                continue;
            }
        }
        let prefix = parent_sym.map_or("", |p| p.local_qname.as_str());
        let prefix = match &explicit_scope {
            Some(s) => join(prefix, sep, &s.replace("::", sep)),
            None => prefix.to_string(),
        };
        let local_qname = join(&prefix, sep, &name);
        let sp = d.node.start_position();
        let ep = d.node.end_position();
        let doc = doc_for(lang, d.node, src);
        let signature = signature_line(d.node, src);
        facts.symbols.push(RawSymbol {
            name,
            kind,
            container,
            parent,
            local_qname,
            start_line: sp.row as u32 + 1,
            start_col: sp.column as u32 + 1,
            end_line: ep.row as u32 + 1,
            end_col: ep.column as u32 + 1,
            signature,
            doc,
            exported,
            default_export: false,
            recv,
            ret: None,
            fields: Vec::new(),
            start_byte: sb,
            end_byte: eb,
        });
        let _ = i;
        stack.push(facts.symbols.len() - 1);
    }
}

fn apply_exports(
    lang: Lang,
    src: &[u8],
    exports: &[Node<'_>],
    _defs: &[DefCand<'_>],
    symbols: &mut [RawSymbol],
) {
    if !lang.is_js_family() {
        return;
    }
    for &ex in exports {
        let is_default = {
            let mut c = ex.walk();
            let d = ex.children(&mut c).any(|ch| ch.kind() == "default");
            d
        };
        if let Some(decl) = ex.child_by_field_name("declaration") {
            let (sb, eb) = (decl.start_byte(), decl.end_byte());
            for s in symbols.iter_mut() {
                if s.parent.is_none() && s.start_byte >= sb && s.end_byte <= eb {
                    s.exported = true;
                    if is_default {
                        s.default_export = true;
                    }
                }
            }
            continue;
        }
        if let Some(value) = ex.child_by_field_name("value") {
            if is_default {
                if value.kind() == "identifier" {
                    let name = text(value, src);
                    for s in symbols.iter_mut() {
                        if s.parent.is_none() && s.name == name {
                            s.exported = true;
                            s.default_export = true;
                        }
                    }
                } else {
                    let (sb, eb) = (value.start_byte(), value.end_byte());
                    for s in symbols.iter_mut() {
                        if s.parent.is_none() && s.start_byte >= sb && s.end_byte <= eb {
                            s.exported = true;
                            s.default_export = true;
                        }
                    }
                }
            }
            continue;
        }
        // export { a, b as c }
        let mut c = ex.walk();
        for clause in ex.named_children(&mut c) {
            if clause.kind() != "export_clause" {
                continue;
            }
            let mut c2 = clause.walk();
            for spec in clause.named_children(&mut c2) {
                let Some(n) = spec.child_by_field_name("name") else {
                    continue;
                };
                let alias = spec.child_by_field_name("alias").map(|a| text(a, src));
                let name = text(n, src);
                for s in symbols.iter_mut() {
                    if s.parent.is_none() && s.name == name {
                        s.exported = true;
                        if alias == Some("default") {
                            s.default_export = true;
                        }
                    }
                }
            }
        }
    }
    // Class members of exported classes are reachable too.
    for i in 0..symbols.len() {
        if let Some(p) = symbols[i].parent {
            if symbols[p].exported && !symbols[i].name.starts_with('#') {
                symbols[i].exported = true;
            }
        }
    }
}

fn unquote(s: &str) -> String {
    s.trim()
        .trim_matches(|c| c == '"' || c == '\'' || c == '`' || c == '<' || c == '>')
        .to_string()
}

fn parse_import(lang: Lang, node: Node<'_>, src: &[u8], out: &mut Vec<RawImport>) {
    match lang {
        Lang::Go => go_imports(node, src, out),
        Lang::Rust => {
            if let Some(arg) = node.child_by_field_name("argument") {
                rust_use(arg, "", line_of(node), src, out);
            }
        }
        Lang::Python => python_import(node, src, out),
        Lang::TypeScript | Lang::Tsx | Lang::JavaScript => js_import(node, src, out),
        Lang::C | Lang::Cpp => {
            if let Some(p) = node.child_by_field_name("path") {
                let system = p.kind() == "system_lib_string";
                out.push(RawImport {
                    path: unquote(text(p, src)),
                    line: line_of(node),
                    bindings: Vec::new(),
                    system,
                });
            }
        }
    }
}

fn go_imports(node: Node<'_>, src: &[u8], out: &mut Vec<RawImport>) {
    let mut specs = Vec::new();
    let mut c = node.walk();
    for ch in node.named_children(&mut c) {
        match ch.kind() {
            "import_spec" => specs.push(ch),
            "import_spec_list" => {
                let mut c2 = ch.walk();
                specs.extend(
                    ch.named_children(&mut c2)
                        .filter(|n| n.kind() == "import_spec"),
                );
            }
            _ => {}
        }
    }
    for spec in specs {
        let Some(path) = spec.child_by_field_name("path") else {
            continue;
        };
        let path = unquote(text(path, src));
        let alias = spec.child_by_field_name("name").map(|n| text(n, src));
        let bindings = match alias {
            Some("_") => Vec::new(),
            Some(".") => vec![Binding {
                kind: BindKind::Glob,
                name: String::new(),
                local: String::new(),
            }],
            Some(a) => vec![Binding {
                kind: BindKind::Module,
                name: String::new(),
                local: a.to_string(),
            }],
            None => vec![Binding {
                kind: BindKind::Module,
                name: String::new(),
                local: String::new(),
            }],
        };
        out.push(RawImport {
            path,
            line: line_of(spec),
            bindings,
            system: false,
        });
    }
}

fn join_path(prefix: &str, rest: &str) -> String {
    if prefix.is_empty() {
        rest.to_string()
    } else if rest.is_empty() {
        prefix.to_string()
    } else {
        format!("{prefix}::{rest}")
    }
}

fn last_seg(p: &str) -> &str {
    p.rsplit("::").next().unwrap_or(p)
}

fn rust_use(node: Node<'_>, prefix: &str, line: u32, src: &[u8], out: &mut Vec<RawImport>) {
    match node.kind() {
        "scoped_use_list" => {
            let p = node
                .child_by_field_name("path")
                .map_or(String::new(), |p| text(p, src).to_string());
            let prefix = join_path(prefix, &p);
            if let Some(list) = node.child_by_field_name("list") {
                rust_use(list, &prefix, line, src, out);
            }
        }
        "use_list" => {
            let mut c = node.walk();
            for ch in node.named_children(&mut c) {
                rust_use(ch, prefix, line, src, out);
            }
        }
        "use_as_clause" => {
            let Some(path) = node.child_by_field_name("path") else {
                return;
            };
            let full = join_path(prefix, text(path, src));
            let alias = node
                .child_by_field_name("alias")
                .map_or_else(|| last_seg(&full).to_string(), |a| text(a, src).to_string());
            out.push(RawImport {
                bindings: vec![Binding {
                    kind: BindKind::Name,
                    name: last_seg(&full).to_string(),
                    local: alias,
                }],
                path: full,
                line,
                system: false,
            });
        }
        "use_wildcard" => {
            let inner = node
                .named_child(0)
                .map_or(String::new(), |p| text(p, src).to_string());
            out.push(RawImport {
                path: join_path(prefix, &inner),
                line,
                bindings: vec![Binding {
                    kind: BindKind::Glob,
                    name: String::new(),
                    local: String::new(),
                }],
                system: false,
            });
        }
        "identifier" | "scoped_identifier" | "crate" | "self" | "super" | "metavariable" => {
            let t = text(node, src);
            let (full, leaf) = if t == "self" && !prefix.is_empty() {
                (prefix.to_string(), last_seg(prefix).to_string())
            } else {
                let full = join_path(prefix, t);
                let leaf = last_seg(&full).to_string();
                (full, leaf)
            };
            out.push(RawImport {
                bindings: vec![Binding {
                    kind: BindKind::Name,
                    name: leaf.clone(),
                    local: leaf,
                }],
                path: full,
                line,
                system: false,
            });
        }
        _ => {}
    }
}

fn python_import(node: Node<'_>, src: &[u8], out: &mut Vec<RawImport>) {
    let line = line_of(node);
    if node.kind() == "import_statement" {
        let mut c = node.walk();
        for n in node.children_by_field_name("name", &mut c) {
            let (path, local) = if n.kind() == "aliased_import" {
                let p = n
                    .child_by_field_name("name")
                    .map_or("", |x| text(x, src))
                    .to_string();
                let a = n
                    .child_by_field_name("alias")
                    .map_or_else(|| p.clone(), |x| text(x, src).to_string());
                (p, a)
            } else {
                let p = text(n, src).to_string();
                (p.clone(), p)
            };
            out.push(RawImport {
                path,
                line,
                bindings: vec![Binding {
                    kind: BindKind::Module,
                    name: String::new(),
                    local,
                }],
                system: false,
            });
        }
        return;
    }
    // import_from_statement
    let Some(module) = node.child_by_field_name("module_name") else {
        return;
    };
    let path: String = text(module, src).split_whitespace().collect();
    let mut bindings = Vec::new();
    let mut c = node.walk();
    for n in node.children_by_field_name("name", &mut c) {
        if n.kind() == "aliased_import" {
            let orig = n.child_by_field_name("name").map_or("", |x| text(x, src));
            let alias = n
                .child_by_field_name("alias")
                .map_or(orig, |x| text(x, src));
            bindings.push(Binding {
                kind: BindKind::Name,
                name: orig.to_string(),
                local: alias.to_string(),
            });
        } else {
            let t = text(n, src);
            bindings.push(Binding {
                kind: BindKind::Name,
                name: t.to_string(),
                local: t.to_string(),
            });
        }
    }
    let mut c = node.walk();
    if node
        .named_children(&mut c)
        .any(|n| n.kind() == "wildcard_import")
    {
        bindings.push(Binding {
            kind: BindKind::Glob,
            name: String::new(),
            local: String::new(),
        });
    }
    out.push(RawImport {
        path,
        line,
        bindings,
        system: false,
    });
}

fn js_string(node: Node<'_>, src: &[u8]) -> String {
    unquote(text(node, src))
}

fn js_import(node: Node<'_>, src: &[u8], out: &mut Vec<RawImport>) {
    let line = line_of(node);
    if node.kind() == "variable_declarator" {
        // const x = require('m') / const {a, b: c} = require('m')
        let Some(value) = node.child_by_field_name("value") else {
            return;
        };
        let Some(args) = value.child_by_field_name("arguments") else {
            return;
        };
        let Some(s) = args.named_child(0) else { return };
        let path = js_string(s, src);
        let mut bindings = Vec::new();
        if let Some(lhs) = node.child_by_field_name("name") {
            match lhs.kind() {
                "identifier" => bindings.push(Binding {
                    kind: BindKind::Module,
                    name: String::new(),
                    local: text(lhs, src).to_string(),
                }),
                "object_pattern" => {
                    let mut c = lhs.walk();
                    for p in lhs.named_children(&mut c) {
                        match p.kind() {
                            "shorthand_property_identifier_pattern" => {
                                let t = text(p, src).to_string();
                                bindings.push(Binding {
                                    kind: BindKind::Name,
                                    name: t.clone(),
                                    local: t,
                                });
                            }
                            "pair_pattern" => {
                                let k = p.child_by_field_name("key").map(|k| text(k, src));
                                let v = p.child_by_field_name("value").map(|v| text(v, src));
                                if let (Some(k), Some(v)) = (k, v) {
                                    bindings.push(Binding {
                                        kind: BindKind::Name,
                                        name: k.to_string(),
                                        local: v.to_string(),
                                    });
                                }
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
        out.push(RawImport {
            path,
            line,
            bindings,
            system: false,
        });
        return;
    }
    let Some(source) = node.child_by_field_name("source") else {
        // TS `import x = require('m')`
        let mut c = node.walk();
        for ch in node.named_children(&mut c) {
            if ch.kind() == "import_require_clause" {
                let local = ch.named_child(0).map(|n| text(n, src).to_string());
                let path = ch
                    .child_by_field_name("source")
                    .map(|s| js_string(s, src))
                    .unwrap_or_default();
                if let Some(local) = local {
                    out.push(RawImport {
                        path,
                        line,
                        bindings: vec![Binding {
                            kind: BindKind::Module,
                            name: String::new(),
                            local,
                        }],
                        system: false,
                    });
                }
            }
        }
        return;
    };
    let path = js_string(source, src);
    let mut bindings = Vec::new();
    let mut c = node.walk();
    for clause in node.named_children(&mut c) {
        if clause.kind() != "import_clause" {
            continue;
        }
        let mut c2 = clause.walk();
        for part in clause.named_children(&mut c2) {
            match part.kind() {
                "identifier" => bindings.push(Binding {
                    kind: BindKind::Default,
                    name: String::new(),
                    local: text(part, src).to_string(),
                }),
                "namespace_import" => {
                    if let Some(id) = part.named_child(0) {
                        bindings.push(Binding {
                            kind: BindKind::Module,
                            name: String::new(),
                            local: text(id, src).to_string(),
                        });
                    }
                }
                "named_imports" => {
                    let mut c3 = part.walk();
                    for spec in part.named_children(&mut c3) {
                        if spec.kind() != "import_specifier" {
                            continue;
                        }
                        let Some(n) = spec.child_by_field_name("name") else {
                            continue;
                        };
                        let name = text(n, src).to_string();
                        let local = spec
                            .child_by_field_name("alias")
                            .map_or_else(|| name.clone(), |a| text(a, src).to_string());
                        let kind = if name == "default" {
                            BindKind::Default
                        } else {
                            BindKind::Name
                        };
                        bindings.push(Binding { kind, name, local });
                    }
                }
                _ => {}
            }
        }
    }
    out.push(RawImport {
        path,
        line,
        bindings,
        system: false,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(f: &FileFacts) -> Vec<String> {
        f.symbols
            .iter()
            .map(|s| format!("{}:{}", s.kind.as_str(), s.local_qname))
            .collect()
    }

    #[test]
    fn go_basics() {
        let src = br#"package store

import (
    "fmt"
    x "example.com/app/util"
)

// Store keeps things.
type Store struct{}

// Get returns a value.
func (s *Store) Get(k string) string { return s.load(k) }

func (s *Store) load(k string) string { fmt.Println(k); return x.Helper() }

const Max = 3
"#;
        let f = extract(Lang::Go, src).unwrap();
        assert_eq!(f.package.as_deref(), Some("store"));
        assert_eq!(
            names(&f),
            [
                "struct:Store",
                "method:Store.Get",
                "method:Store.load",
                "const:Max"
            ]
        );
        assert_eq!(f.symbols[0].doc.as_deref(), Some("Store keeps things."));
        assert_eq!(f.symbols[1].doc.as_deref(), Some("Get returns a value."));
        assert_eq!(f.symbols[1].recv.as_deref(), Some("s"));
        assert!(f.symbols[1].exported && !f.symbols[2].exported);
        let calls: Vec<String> = f
            .refs
            .iter()
            .filter(|r| r.kind == EdgeKind::Call)
            .map(|r| format!("{}.{}", r.qualifier.clone().unwrap_or_default(), r.name))
            .collect();
        assert_eq!(calls, ["s.load", "fmt.Println", "x.Helper"]);
        assert_eq!(f.imports.len(), 2);
        assert_eq!(f.imports[1].bindings[0].local, "x");
    }

    #[test]
    fn rust_impl_and_use() {
        let src = br"use crate::a::{b::C, d as e, f::*};

/// A thing.
#[derive(Debug)]
pub struct Thing;

impl Thing {
    pub fn new() -> Self { helper(); Self }
}

impl Default for Thing {
    fn default() -> Self { Self::new() }
}

fn helper() {}
";
        let f = extract(Lang::Rust, src).unwrap();
        assert_eq!(
            names(&f),
            [
                "struct:Thing",
                "method:Thing::new",
                "method:Thing::default",
                "function:helper"
            ]
        );
        assert_eq!(f.symbols[0].doc.as_deref(), Some("A thing."));
        let paths: Vec<&str> = f.imports.iter().map(|i| i.path.as_str()).collect();
        assert_eq!(paths, ["crate::a::b::C", "crate::a::d", "crate::a::f"]);
        assert_eq!(f.imports[1].bindings[0].local, "e");
        assert_eq!(f.imports[2].bindings[0].kind, BindKind::Glob);
        let imp = f.refs.iter().find(|r| r.kind == EdgeKind::Impl).unwrap();
        assert_eq!(imp.name, "Default");
        assert_eq!(imp.from_name.as_deref(), Some("Thing"));
        let q = f.refs.iter().find(|r| r.name == "new").unwrap();
        assert_eq!(q.qualifier.as_deref(), Some("Self"));
    }

    #[test]
    fn signature_and_clip() {
        assert_eq!(clip("abcdef", 3), "abc…");
        assert_eq!(base_type_name("*Store[T]"), "Store");
        assert_eq!(base_type_name("&mut Vec<u8>"), "Vec");
        assert_eq!(normalize_qualifier("Vec::<u8>"), "Vec");
        assert_eq!(
            go_receiver("(s *Store)"),
            ("Store".into(), Some("s".into()))
        );
        assert_eq!(go_receiver("(Store)"), ("Store".into(), None));
    }
}

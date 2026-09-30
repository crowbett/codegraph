//! SCIP index loading: definitions, reference occurrences, implementation relationships.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::{Context, Result};
use protobuf::Message;

/// A definition site.
#[derive(Debug, Clone)]
pub struct Def {
    pub path: String,
    /// 0-based line.
    pub line: u32,
    pub start: u32,
}

/// One occurrence.
#[derive(Debug, Clone)]
pub struct Occ {
    pub line: u32,
    pub start: u32,
    pub end: u32,
    pub symbol: String,
    pub is_def: bool,
}

/// What we need from one SCIP index.
#[derive(Default)]
pub struct Scip {
    /// symbol -> definition site (locals are keyed `local N@path`).
    pub defs: HashMap<String, Def>,
    /// Further definition sites of a symbol that has several: Python/TS `@overload`
    /// stubs plus the implementation, Rust `#[cfg]` twins. Each is the same symbol.
    pub extra_defs: HashMap<String, Vec<Def>>,
    /// path -> occurrences sorted by (line, start).
    pub occs: HashMap<String, Vec<Occ>>,
    /// symbol -> symbols it implements (is_implementation).
    pub implements: HashMap<String, HashSet<String>>,
    /// SymbolInformation kind names, when the indexer provides them.
    pub kinds: HashMap<String, String>,
    /// A symbol given another's definition -> that symbol (see `alias_python_constants`).
    pub alias_of: HashMap<String, String>,
}

impl Scip {
    pub fn load(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
        let idx = scip::types::Index::parse_from_bytes(&bytes)
            .with_context(|| format!("decode {}", path.display()))?;
        let mut out = Scip::default();
        for doc in &idx.documents {
            let p = doc.relative_path.replace('\\', "/");
            let v = out.occs.entry(p.clone()).or_default();
            for o in &doc.occurrences {
                let Some((line, start, end)) = range_of(o) else {
                    continue;
                };
                if o.symbol.is_empty() {
                    continue;
                }
                let symbol = canonical(&p, &o.symbol);
                let is_def = o.symbol_roles & 1 != 0;
                // A constructor folded into its class keeps the class's own definition.
                let folded = strip_constructor(&o.symbol).is_some();
                if is_def && !folded {
                    let d = Def {
                        path: p.clone(),
                        line,
                        start,
                    };
                    match out.defs.get(&symbol) {
                        None => {
                            out.defs.insert(symbol.clone(), d);
                        }
                        Some(first) if first.path != d.path || first.line != d.line => {
                            out.extra_defs.entry(symbol.clone()).or_default().push(d);
                        }
                        Some(_) => {}
                    }
                }
                v.push(Occ {
                    line,
                    start,
                    end,
                    symbol,
                    is_def,
                });
            }
            v.sort_by_key(|o| (o.line, o.start));
            for si in &doc.symbols {
                record_info(&mut out, si);
            }
        }
        for si in &idx.external_symbols {
            record_info(&mut out, si);
        }
        out.alias_python_constants();
        Ok(out)
    }

    /// scip-python defines an annotated module constant (`INT: t.Final[IntParamType] = ..`)
    /// as the term `mod/INT.INT.` (sic) but spells references to it as the type `mod/INT#`,
    /// which then has no definition and reads as external. Such a type symbol gets the
    /// term's definition.
    fn alias_python_constants(&mut self) {
        let mut add = Vec::new();
        for occs in self.occs.values() {
            for o in occs {
                let s = &o.symbol;
                if !s.starts_with("scip-python ") || self.defs.contains_key(s) {
                    continue;
                }
                let Some(stem) = s.strip_suffix('#') else {
                    continue;
                };
                let name = stem.rsplit(['/', '#', '.', '`']).next().unwrap_or("");
                let term = [format!("{stem}.{name}."), format!("{stem}.")]
                    .into_iter()
                    .find_map(|t| self.defs.get(&t).map(|d| (t, d)));
                if let Some((t, d)) = term {
                    add.push((s.clone(), t, d.clone()));
                }
            }
        }
        for (s, t, d) in add {
            self.defs.entry(s.clone()).or_insert(d);
            self.alias_of.insert(s, t);
        }
    }

    /// Occurrences on one (0-based) line of a file.
    pub fn on_line(&self, path: &str, line: u32) -> &[Occ] {
        let Some(v) = self.occs.get(path) else {
            return &[];
        };
        let lo = v.partition_point(|o| o.line < line);
        let hi = v.partition_point(|o| o.line <= line);
        &v[lo..hi]
    }

    /// Whether `a` and `b` are related by implementation (either direction, one hop,
    /// or siblings implementing the same abstract method).
    pub fn dispatch_related(&self, a: &str, b: &str) -> bool {
        let ia = self.implements.get(a);
        let ib = self.implements.get(b);
        ia.is_some_and(|s| s.contains(b))
            || ib.is_some_and(|s| s.contains(a))
            || matches!((ia, ib), (Some(x), Some(y)) if x.intersection(y).next().is_some())
    }
}

fn record_info(out: &mut Scip, si: &scip::types::SymbolInformation) {
    for r in &si.relationships {
        if r.is_implementation {
            out.implements
                .entry(si.symbol.clone())
                .or_default()
                .insert(r.symbol.clone());
        }
    }
    let k = si.kind.value();
    if k != 0 {
        out.kinds
            .insert(si.symbol.clone(), format!("{:?}", si.kind));
    }
}

fn range_of(o: &scip::types::Occurrence) -> Option<(u32, u32, u32)> {
    let r = &o.range;
    match r.len() {
        3 => Some((r[0] as u32, r[1] as u32, r[2] as u32)),
        4 if r[0] == r[2] => Some((r[0] as u32, r[1] as u32, r[3] as u32)),
        4 => Some((r[0] as u32, r[1] as u32, u32::MAX)),
        _ => {
            use scip::types::occurrence::Typed_range;
            match &o.typed_range {
                Some(Typed_range::SingleLineRange(s)) => Some((
                    s.line as u32,
                    s.start_character as u32,
                    s.end_character as u32,
                )),
                Some(Typed_range::MultiLineRange(m)) => {
                    Some((m.start_line as u32, m.start_character as u32, u32::MAX))
                }
                _ => None,
            }
        }
    }
}

pub fn is_local(sym: &str) -> bool {
    sym.starts_with("local ")
}

/// Document-local symbols (`local 12`) are made unique per file (`local 12@path`) so
/// nested functions resolve like any other definition; a TS constructor
/// (`C#<constructor>().`) is folded into its class (`C#`), which is what codegraph links
/// `new C()` to.
/// A class declared inside a function body is a local too: its constructor is spelled
/// `local 7`<constructor>`().` and folds into `local 7@path`.
fn canonical(path: &str, sym: &str) -> String {
    let base = strip_constructor(sym).unwrap_or(sym);
    if is_local(base) {
        format!("{base}@{path}")
    } else {
        base.to_string()
    }
}

/// The class part of a TS constructor symbol, if `sym` is one.
fn strip_constructor(sym: &str) -> Option<&str> {
    sym.strip_suffix("`<constructor>`().")
        .or_else(|| sym.strip_suffix("<constructor>()."))
}

/// Descriptor class of a symbol string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    Callable,
    Type,
    Other,
}

pub fn class_of(sym: &str) -> Class {
    if sym.ends_with(").") {
        // `name().` (method/function); `(param)` ends with `)` alone.
        Class::Callable
    } else if sym.ends_with('#') {
        // rust-analyzer spells enum variants `Enum#Variant#`; like struct fields they are
        // not codegraph symbols (Rust has no other nested types).
        if sym.starts_with("rust-analyzer ") && is_nested_type(sym) {
            return Class::Other;
        }
        Class::Type
    } else {
        Class::Other
    }
}

/// `...Outer#Inner#`: a type descriptor directly under another type descriptor.
fn is_nested_type(sym: &str) -> bool {
    let body = &sym[..sym.len() - 1];
    let Some(i) = body.rfind(['#', '/', '.', ')', ']', ' ']) else {
        return false;
    };
    body.as_bytes()[i] == b'#' && !body[i + 1..].is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classes() {
        assert_eq!(class_of("scip-go gomod m v `m/p`/T#M()."), Class::Callable);
        assert_eq!(class_of("scip-go gomod m v `m/p`/T#"), Class::Type);
        assert_eq!(
            class_of("rust-analyzer cargo fd 1 exit_codes/ExitCode#GeneralError#"),
            Class::Other
        );
        assert_eq!(
            class_of("rust-analyzer cargo fd 1 exit_codes/ExitCode#"),
            Class::Type
        );
        assert_eq!(class_of("rust-analyzer cargo fd 1 f/g(x)"), Class::Other);
        assert_eq!(canonical("a.ts", "local 3"), "local 3@a.ts");
        assert_eq!(
            canonical("a.ts", "local 3`<constructor>`()."),
            "local 3@a.ts"
        );
        assert_eq!(
            canonical("a.ts", "npm p 1 a/C#<constructor>()."),
            "npm p 1 a/C#"
        );
    }
}

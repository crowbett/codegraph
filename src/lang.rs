//! Supported languages: file detection, tree-sitter grammars, and compiled tag queries.

use std::cell::RefCell;
use std::path::Path;
use std::sync::OnceLock;

use serde::Serialize;
use tree_sitter::{Language, Parser, Query};

use crate::error::{Error, Result};

/// A language codegraph can parse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Lang {
    /// Go.
    Go,
    /// Rust.
    Rust,
    /// C (and `.h` headers next to C sources).
    C,
    /// C++.
    Cpp,
    /// TypeScript.
    TypeScript,
    /// TypeScript + JSX.
    Tsx,
    /// JavaScript (incl. JSX, ESM and CommonJS).
    JavaScript,
    /// Python.
    Python,
}

impl Lang {
    /// All languages.
    pub const ALL: [Lang; 8] = [
        Self::Go,
        Self::Rust,
        Self::C,
        Self::Cpp,
        Self::TypeScript,
        Self::Tsx,
        Self::JavaScript,
        Self::Python,
    ];

    /// Detect from a file extension. `.h` maps to C; the indexer upgrades it to
    /// C++ when the directory holds C++ sources.
    pub fn from_path(path: &Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        Some(match ext.as_str() {
            "go" => Self::Go,
            "rs" => Self::Rust,
            "c" | "h" => Self::C,
            "cc" | "cpp" | "cxx" | "c++" | "hpp" | "hh" | "hxx" | "h++" | "ipp" | "inl" => {
                Self::Cpp
            }
            "ts" | "mts" | "cts" => Self::TypeScript,
            "tsx" => Self::Tsx,
            "js" | "jsx" | "mjs" | "cjs" => Self::JavaScript,
            "py" | "pyi" => Self::Python,
            _ => return None,
        })
    }

    /// Stable lowercase name (stored in the database).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Go => "go",
            Self::Rust => "rust",
            Self::C => "c",
            Self::Cpp => "cpp",
            Self::TypeScript => "typescript",
            Self::Tsx => "tsx",
            Self::JavaScript => "javascript",
            Self::Python => "python",
        }
    }

    /// Parse a stored name.
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|l| l.as_str() == s)
    }

    /// Separator used in qualified names (`a::b` for Rust/C++, `a.b` elsewhere).
    pub fn sep(self) -> &'static str {
        match self {
            Self::Rust | Self::Cpp | Self::C => "::",
            _ => ".",
        }
    }

    /// Languages resolved with the TS/JS module rules.
    pub fn is_js_family(self) -> bool {
        matches!(self, Self::TypeScript | Self::Tsx | Self::JavaScript)
    }

    /// Languages resolved with the include-graph rules.
    pub fn is_c_family(self) -> bool {
        matches!(self, Self::C | Self::Cpp)
    }

    fn index(self) -> usize {
        self as usize
    }

    /// The tree-sitter grammar.
    pub fn language(self) -> Language {
        match self {
            Self::Go => tree_sitter_go::LANGUAGE.into(),
            Self::Rust => tree_sitter_rust::LANGUAGE.into(),
            Self::C => tree_sitter_c::LANGUAGE.into(),
            Self::Cpp => tree_sitter_cpp::LANGUAGE.into(),
            Self::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Self::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            Self::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            Self::Python => tree_sitter_python::LANGUAGE.into(),
        }
    }

    /// Source of the tag query (`queries/<lang>/tags.scm`).
    pub fn query_source(self) -> String {
        match self {
            Self::Go => include_str!("../queries/go/tags.scm").to_string(),
            Self::Rust => include_str!("../queries/rust/tags.scm").to_string(),
            Self::C => include_str!("../queries/c/tags.scm").to_string(),
            Self::Cpp => include_str!("../queries/cpp/tags.scm").to_string(),
            Self::TypeScript => include_str!("../queries/typescript/tags.scm").to_string(),
            Self::Tsx => format!(
                "{}\n{}",
                include_str!("../queries/typescript/tags.scm"),
                include_str!("../queries/typescript/jsx.scm")
            ),
            Self::JavaScript => include_str!("../queries/javascript/tags.scm").to_string(),
            Self::Python => include_str!("../queries/python/tags.scm").to_string(),
        }
    }

    /// The compiled tag query (compiled once per process; `Query` is `Sync`).
    pub fn query(self) -> Result<&'static Query> {
        static QUERIES: [OnceLock<std::result::Result<Query, String>>; 8] =
            [const { OnceLock::new() }; 8];
        let slot = QUERIES[self.index()].get_or_init(|| {
            Query::new(&self.language(), &self.query_source()).map_err(|e| e.to_string())
        });
        slot.as_ref().map_err(|m| Error::Query {
            lang: self.as_str(),
            message: m.clone(),
        })
    }

    /// Run `f` with this thread's parser for the language.
    pub fn with_parser<T>(self, f: impl FnOnce(&mut Parser) -> T) -> Result<T> {
        thread_local! {
            static PARSERS: RefCell<Vec<Option<Parser>>> = RefCell::new((0..8).map(|_| None).collect());
        }
        PARSERS.with(|cell| {
            let mut parsers = cell.borrow_mut();
            let slot = &mut parsers[self.index()];
            if slot.is_none() {
                let mut p = Parser::new();
                p.set_language(&self.language())
                    .map_err(|e| Error::Internal(format!("grammar {}: {e}", self.as_str())))?;
                *slot = Some(p);
            }
            match slot.as_mut() {
                Some(p) => Ok(f(p)),
                None => Err(Error::Internal("parser slot empty".into())),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_queries_compile() {
        let errs: Vec<String> = Lang::ALL
            .into_iter()
            .filter_map(|l| l.query().err().map(|e| e.to_string()))
            .collect();
        assert!(
            errs.is_empty(),
            "{}",
            errs.join(
                "
"
            )
        );
    }

    #[test]
    fn detection() {
        assert_eq!(Lang::from_path(Path::new("a/b.go")), Some(Lang::Go));
        assert_eq!(Lang::from_path(Path::new("x.TSX")), Some(Lang::Tsx));
        assert_eq!(Lang::from_path(Path::new("x.hpp")), Some(Lang::Cpp));
        assert_eq!(Lang::from_path(Path::new("x.h")), Some(Lang::C));
        assert_eq!(Lang::from_path(Path::new("x.md")), None);
        for l in Lang::ALL {
            assert_eq!(Lang::parse(l.as_str()), Some(l));
        }
    }
}

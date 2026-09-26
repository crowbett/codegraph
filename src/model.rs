//! Core vocabulary: symbol kinds, edge kinds, confidence levels.

use serde::{Deserialize, Serialize};

/// What a symbol is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SymKind {
    /// Free function (or arrow function bound to a name).
    Function,
    /// Method / associated function (has a receiver, impl, or class container).
    Method,
    /// Class.
    Class,
    /// Struct / union.
    Struct,
    /// Interface.
    Interface,
    /// Rust trait.
    Trait,
    /// Enum.
    Enum,
    /// Type alias / typedef / named type.
    Type,
    /// Constant (or `#define`).
    Const,
    /// Package-level variable.
    Var,
    /// Module / namespace.
    Module,
    /// Macro.
    Macro,
}

impl SymKind {
    /// Every kind, for `--kind` help.
    pub const ALL: [SymKind; 12] = [
        Self::Function,
        Self::Method,
        Self::Class,
        Self::Struct,
        Self::Interface,
        Self::Trait,
        Self::Enum,
        Self::Type,
        Self::Const,
        Self::Var,
        Self::Module,
        Self::Macro,
    ];

    /// Stable lowercase name (stored in the database).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Function => "function",
            Self::Method => "method",
            Self::Class => "class",
            Self::Struct => "struct",
            Self::Interface => "interface",
            Self::Trait => "trait",
            Self::Enum => "enum",
            Self::Type => "type",
            Self::Const => "const",
            Self::Var => "var",
            Self::Module => "module",
            Self::Macro => "macro",
        }
    }

    /// Parse a stored name.
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }

    /// Parse a user-facing `--kind` filter, accepting common aliases. `fn` means
    /// functions and methods.
    pub fn parse_filter(s: &str) -> Option<Vec<Self>> {
        let k = s.trim().to_ascii_lowercase();
        let v = match k.as_str() {
            "fn" | "func" | "callable" => vec![Self::Function, Self::Method],
            "function" | "function_" => vec![Self::Function],
            "method" | "meth" => vec![Self::Method],
            "class" | "cls" => vec![Self::Class],
            "struct" | "union" => vec![Self::Struct],
            "interface" | "iface" => vec![Self::Interface],
            "trait" => vec![Self::Trait],
            "enum" => vec![Self::Enum],
            "type" | "typedef" | "alias" => vec![Self::Type],
            "types" => vec![
                Self::Class,
                Self::Struct,
                Self::Interface,
                Self::Trait,
                Self::Enum,
                Self::Type,
            ],
            "const" | "constant" | "define" => vec![Self::Const],
            "var" | "variable" => vec![Self::Var],
            "module" | "mod" | "namespace" | "package" => vec![Self::Module],
            "macro" => vec![Self::Macro],
            _ => return None,
        };
        Some(v)
    }

    /// Callable (function-like).
    pub fn is_callable(self) -> bool {
        matches!(self, Self::Function | Self::Method | Self::Macro)
    }

    /// Type-like container that owns methods.
    pub fn is_type_like(self) -> bool {
        matches!(
            self,
            Self::Class | Self::Struct | Self::Interface | Self::Trait | Self::Enum | Self::Type
        )
    }
}

/// Relationship recorded by an edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EdgeKind {
    /// A call (or constructor / JSX usage).
    Call,
    /// A type reference.
    Ref,
    /// `impl Trait for Type`, `implements I`.
    Impl,
    /// `extends`, base classes.
    Inherit,
    /// File-level import / include / use.
    Import,
    /// A function or method used as a value (callback, handler registration).
    Value,
}

impl EdgeKind {
    /// Stable lowercase name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Call => "call",
            Self::Ref => "ref",
            Self::Impl => "impl",
            Self::Inherit => "inherit",
            Self::Import => "import",
            Self::Value => "value",
        }
    }

    /// Parse a stored name.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "call" => Some(Self::Call),
            "ref" => Some(Self::Ref),
            "impl" => Some(Self::Impl),
            "inherit" => Some(Self::Inherit),
            "import" => Some(Self::Import),
            "value" => Some(Self::Value),
            _ => None,
        }
    }
}

/// How sure the resolver is that an edge points at the right symbol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    /// Lexical scope, same Go package, or an explicit import binding.
    Exact,
    /// Unique match in the repo, a same-crate/module match, or a type-qualified call.
    Likely,
    /// Picked by name among several candidates (closest file wins).
    NameOnly,
}

impl Confidence {
    /// Stable name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Likely => "likely",
            Self::NameOnly => "name_only",
        }
    }

    /// Parse a stored name.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "exact" => Some(Self::Exact),
            "likely" => Some(Self::Likely),
            "name_only" => Some(Self::NameOnly),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        for k in SymKind::ALL {
            assert_eq!(SymKind::parse(k.as_str()), Some(k));
        }
        for k in [
            EdgeKind::Call,
            EdgeKind::Ref,
            EdgeKind::Impl,
            EdgeKind::Inherit,
            EdgeKind::Import,
            EdgeKind::Value,
        ] {
            assert_eq!(EdgeKind::parse(k.as_str()), Some(k));
        }
        for c in [Confidence::Exact, Confidence::Likely, Confidence::NameOnly] {
            assert_eq!(Confidence::parse(c.as_str()), Some(c));
        }
    }

    #[test]
    fn kind_filters() {
        assert_eq!(
            SymKind::parse_filter("fn"),
            Some(vec![SymKind::Function, SymKind::Method])
        );
        assert_eq!(SymKind::parse_filter("Struct"), Some(vec![SymKind::Struct]));
        assert!(SymKind::parse_filter("nope").is_none());
    }
}

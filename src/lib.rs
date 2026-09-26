//! codegraph: a tree-sitter symbol + call graph of every repo, served to agents.
//!
//! Pipeline: [`index`] walks the configured roots, [`extract`] parses changed files
//! with per-language tag queries, [`store`] writes symbols and unresolved edges to
//! SQLite, [`resolve`] links edges to symbols per repo, and [`query`] answers
//! find / def / refs / callers / callees / outline / path / impact / hotspots.

pub mod cli;
pub mod config;
pub mod error;
pub mod extract;
pub mod index;
pub mod lang;
pub mod layout;
pub mod mcp;
pub mod model;
pub mod output;
pub mod query;
pub mod render;
pub mod resolve;
pub mod store;
pub mod util;

/// Crate version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// Short git commit baked in at build time.
pub const COMMIT: &str = env!("CODEGRAPH_COMMIT");

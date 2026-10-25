//! codegraph: a tree-sitter symbol + call graph of every repo, served to agents.
//!
//! Pipeline: [`index`] walks the configured roots, [`extract`] parses changed files
//! with per-language tag queries, [`store`] writes symbols and unresolved edges to
//! SQLite, [`resolve`] links edges to symbols per repo, and [`query`] answers
//! find / def / refs / callers / callees / outline / path / impact / hotspots.
//! [`webdemo`] runs that pipeline on a bundled sample in memory for the wasm32 build.

#[cfg(feature = "native")]
pub mod cli;
pub mod clock;
#[cfg(feature = "graph")]
pub mod config;
pub mod error;
pub mod extract;
#[cfg(feature = "graph")]
pub mod index;
pub(crate) mod infer;
pub mod lang;
pub mod layout;
mod macros;
#[cfg(feature = "native")]
pub mod mcp;
pub mod model;
#[cfg(feature = "native")]
pub mod output;
#[cfg(feature = "graph")]
pub mod query;
#[cfg(feature = "native")]
pub mod render;
#[cfg(feature = "graph")]
pub mod resolve;
#[cfg(feature = "graph")]
pub mod store;
pub(crate) mod tsfix;
pub mod util;
#[cfg(feature = "native")]
pub mod watch;
#[cfg(feature = "graph")]
pub mod webdemo;

/// Crate version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// Short git commit baked in at build time.
pub const COMMIT: &str = env!("CODEGRAPH_COMMIT");

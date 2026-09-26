//! Command-line definition (clap derive).

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

const AFTER_HELP: &str = "\
Symbols can be named by simple name (`Evaluate`), qualified name (`rules.Engine.Evaluate`,
`store::Store::open`), or location (`internal/rules/engine.go:42`).

Examples:
  codegraph index                       index every repo under the configured roots (~/dev)
  codegraph find Evaluate --kind fn     fuzzy + exact symbol search
  codegraph def Engine.Evaluate         definition with source
  codegraph callers Store.Get --depth 3 who calls this, as a tree
  codegraph impact internal/rules/engine.go
                                        what transitively depends on this file
  codegraph path main Store.Get         shortest call path
  codegraph dot Evaluate --format mermaid | clip
  codegraph mcp                         stdio MCP server for Claude Code

Exit codes: 0 ok, 1 error, 2 usage, 3 not found, 4 permission denied.";

/// codegraph — a symbol + call graph of every repo, served to agents.
#[derive(Debug, Parser)]
#[command(
    name = "codegraph",
    version = concat!(env!("CARGO_PKG_VERSION"), " (", env!("CODEGRAPH_COMMIT"), ")"),
    about = "A tree-sitter symbol + call graph of every repo you own, served to agents over MCP.",
    after_help = AFTER_HELP,
    propagate_version = true
)]
pub struct Cli {
    /// Global flags.
    #[command(flatten)]
    pub global: GlobalArgs,
    /// Subcommand.
    #[command(subcommand)]
    pub command: Command,
}

/// Flags accepted before or after any subcommand.
#[derive(Debug, Args, Clone, Default)]
pub struct GlobalArgs {
    /// Emit exactly one JSON document ({"ok":...}).
    #[arg(long, global = true)]
    pub json: bool,
    /// Minimal output (locations only).
    #[arg(short, long, global = true)]
    pub quiet: bool,
    /// More detail (signatures, per-repo stats).
    #[arg(short, long, global = true)]
    pub verbose: bool,
    /// Disable ANSI colors (NO_COLOR is honoured too).
    #[arg(long, global = true)]
    pub no_color: bool,
}

/// Graph output format for `dot`.
#[derive(Debug, Clone, Copy, ValueEnum, Default)]
pub enum GraphFormatArg {
    /// Graphviz DOT.
    #[default]
    Dot,
    /// Mermaid flowchart.
    Mermaid,
}

/// Subcommands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Index (incrementally) every repo under the configured roots, or the given paths.
    Index(IndexArgs),
    /// Find symbols by name: exact, qualified, prefix, substring, doc text, fuzzy.
    Find(FindArgs),
    /// Show a symbol's definition with source.
    Def(DefArgs),
    /// List references to a symbol (calls, type uses, impls, inheritance).
    Refs(RefsArgs),
    /// Who calls this function, as a tree.
    Callers(TreeArgs),
    /// What this function calls, as a tree.
    Callees(CalleesArgs),
    /// Nested symbol outline of a file with line numbers.
    Outline(OutlineArgs),
    /// Shortest call path between two functions.
    Path(PathArgs),
    /// What transitively depends on a file or symbol ("what breaks if I change X").
    Impact(ImpactArgs),
    /// Most-referenced symbols, largest fan-out, dead-code candidates.
    Hotspots(HotspotsArgs),
    /// Call graph around a symbol as Graphviz DOT or Mermaid.
    Dot(DotArgs),
    /// Graph size, resolution rate, per-repo freshness.
    Status(RepoArg),
    /// Show config location or effective config.
    Config(ConfigArgs),
    /// Print version.
    Version,
    /// Run the MCP server on stdio (read-only tools, auto-refreshing).
    Mcp,
}

/// `--repo` filter.
#[derive(Debug, Args, Clone, Default)]
pub struct RepoArg {
    /// Restrict to a repo (name like `go/pulse`, last segment `pulse`, or root path).
    #[arg(long, value_name = "REPO")]
    pub repo: Option<String>,
}

/// `index`.
#[derive(Debug, Args)]
pub struct IndexArgs {
    /// Re-parse every file, ignoring content hashes.
    #[arg(long)]
    pub full: bool,
    /// Index these directories instead of the configured roots.
    #[arg(value_name = "PATH")]
    pub paths: Vec<PathBuf>,
}

/// `find`.
#[derive(Debug, Args)]
pub struct FindArgs {
    /// Name, qualified name, or fragment.
    pub query: String,
    /// Kind filter: fn, method, class, struct, interface, trait, enum, type, types, const, module, macro.
    #[arg(long, short = 'k', value_name = "KIND")]
    pub kind: Option<String>,
    /// Repo filter.
    #[command(flatten)]
    pub repo: RepoArg,
    /// Max results.
    #[arg(long, short = 'n', default_value_t = 30)]
    pub limit: usize,
}

/// `def`.
#[derive(Debug, Args)]
pub struct DefArgs {
    /// Symbol (name, qualified name, or file:line).
    pub symbol: String,
    /// Repo filter.
    #[command(flatten)]
    pub repo: RepoArg,
    /// Max source lines per definition.
    #[arg(long, default_value_t = 120)]
    pub max_lines: u32,
    /// Max definitions shown when the name is ambiguous.
    #[arg(long, default_value_t = 5)]
    pub max_defs: usize,
}

/// `refs`.
#[derive(Debug, Args)]
pub struct RefsArgs {
    /// Symbol (name, qualified name, or file:line).
    pub symbol: String,
    /// Only this edge kind: call, ref, impl, inherit.
    #[arg(long, value_name = "KIND")]
    pub kind: Option<String>,
    /// Repo filter.
    #[command(flatten)]
    pub repo: RepoArg,
    /// Max sites.
    #[arg(long, short = 'n', default_value_t = 200)]
    pub limit: usize,
}

/// `callers`.
#[derive(Debug, Args)]
pub struct TreeArgs {
    /// Function (name, qualified name, or file:line).
    pub symbol: String,
    /// Levels to walk.
    #[arg(long, short = 'd', default_value_t = 2)]
    pub depth: u32,
    /// Repo filter.
    #[command(flatten)]
    pub repo: RepoArg,
}

/// `callees`.
#[derive(Debug, Args)]
pub struct CalleesArgs {
    /// Function (name, qualified name, or file:line).
    pub symbol: String,
    /// Levels to walk.
    #[arg(long, short = 'd', default_value_t = 1)]
    pub depth: u32,
    /// Hide calls that resolve outside the graph (stdlib, dependencies).
    #[arg(long)]
    pub no_external: bool,
    /// Repo filter.
    #[command(flatten)]
    pub repo: RepoArg,
}

/// `outline`.
#[derive(Debug, Args)]
pub struct OutlineArgs {
    /// File path (absolute, relative, or repo-relative suffix).
    pub file: String,
    /// Show signature lines.
    #[arg(long, short = 's')]
    pub signatures: bool,
}

/// `path`.
#[derive(Debug, Args)]
pub struct PathArgs {
    /// Start function.
    pub from: String,
    /// Target function.
    pub to: String,
    /// Max hops.
    #[arg(long, default_value_t = 12)]
    pub max_depth: u32,
    /// Repo filter.
    #[command(flatten)]
    pub repo: RepoArg,
}

/// `impact`.
#[derive(Debug, Args)]
pub struct ImpactArgs {
    /// File path or symbol.
    pub target: String,
    /// Levels of reverse dependencies.
    #[arg(long, short = 'd', default_value_t = 3)]
    pub depth: u32,
    /// Max dependents listed.
    #[arg(long, short = 'n', default_value_t = 200)]
    pub limit: usize,
    /// Repo filter.
    #[command(flatten)]
    pub repo: RepoArg,
}

/// `hotspots`.
#[derive(Debug, Args)]
pub struct HotspotsArgs {
    /// Rows per list.
    #[arg(long, short = 'n', default_value_t = 15)]
    pub limit: usize,
    /// Repo filter.
    #[command(flatten)]
    pub repo: RepoArg,
}

/// `dot`.
#[derive(Debug, Args)]
pub struct DotArgs {
    /// Center symbol.
    pub symbol: String,
    /// Levels in each direction.
    #[arg(long, short = 'd', default_value_t = 2)]
    pub depth: u32,
    /// Output format.
    #[arg(long, value_enum, default_value_t = GraphFormatArg::Dot)]
    pub format: GraphFormatArg,
    /// Repo filter.
    #[command(flatten)]
    pub repo: RepoArg,
}

/// `config`.
#[derive(Debug, Args)]
pub struct ConfigArgs {
    /// Action.
    #[command(subcommand)]
    pub action: ConfigAction,
}

/// `config` actions.
#[derive(Debug, Subcommand)]
pub enum ConfigAction {
    /// Print the config file path.
    Path,
    /// Print the effective configuration.
    Show,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn globals_after_subcommand() {
        let c = Cli::try_parse_from(["codegraph", "find", "x", "--json", "-q"]).unwrap();
        assert!(c.global.json && c.global.quiet);
        let c = Cli::try_parse_from(["codegraph", "--no-color", "status"]).unwrap();
        assert!(c.global.no_color);
    }
}

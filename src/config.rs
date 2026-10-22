//! `codegraph.toml` loading per the kit contract.
//!
//! Location: `%USERPROFILE%\.config\kit\codegraph.toml`, or `$KIT_CONFIG_DIR\codegraph.toml`.
//! Data (the SQLite graph): `<KIT_DATA_DIR or config dir>\codegraph\graph.db`.
//! A missing file means defaults; unknown keys produce warnings, not errors.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::error::{Error, Result};
use crate::lang::Lang;

/// Tool name used for config and data paths.
pub const TOOL: &str = "codegraph";

/// Largest accepted `max_file_kb` (4 GiB: tree-sitter addresses a file with 32-bit
/// offsets, and the byte cap must not overflow).
pub const MAX_FILE_KB_LIMIT: u64 = 4 * 1024 * 1024;
/// Largest accepted `threads` (rayon would try to allocate a pool that size).
pub const THREADS_LIMIT: u64 = 1024;

const KNOWN_KEYS: [&str; 7] = [
    "roots",
    "skip_dirs",
    "max_file_kb",
    "languages",
    "color",
    "refresh_budget_ms",
    "threads",
];

/// Directory names skipped by default (on top of `.gitignore`).
pub const DEFAULT_SKIP_DIRS: [&str; 16] = [
    "vendor",
    "node_modules",
    "target",
    "build",
    "dist",
    "out",
    ".git",
    ".venv",
    "venv",
    "__pycache__",
    ".next",
    ".cache",
    "third_party",
    "bower_components",
    "coverage",
    "testdata",
];

/// When to colorize human output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ColorMode {
    /// Color when stdout is a terminal and `NO_COLOR` is unset.
    #[default]
    Auto,
    /// Always color (still off with `--no-color`/`NO_COLOR`).
    Always,
    /// Never color.
    Never,
}

/// Effective configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Directories to index (each git repo below becomes a graph "repo").
    pub roots: Vec<PathBuf>,
    /// Directory names never descended into.
    pub skip_dirs: Vec<String>,
    /// Files larger than this are skipped (generated code, bundles).
    pub max_file_kb: u64,
    /// Enabled languages.
    pub languages: Vec<Lang>,
    /// Color mode.
    pub color: ColorMode,
    /// MCP auto-refresh time budget per call.
    pub refresh_budget_ms: u64,
    /// Parser threads (0 = one per core).
    pub threads: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            roots: vec![home_dir().join("dev")],
            skip_dirs: DEFAULT_SKIP_DIRS.iter().map(|s| (*s).to_string()).collect(),
            max_file_kb: 1024,
            languages: Lang::ALL.to_vec(),
            color: ColorMode::Auto,
            refresh_budget_ms: 1500,
            threads: 0,
        }
    }
}

/// JSON view of [`Config`] used by `config show`.
#[derive(Debug, Serialize)]
pub struct ConfigView {
    /// Resolved config path.
    pub path: String,
    /// Whether the file exists.
    pub exists: bool,
    /// Graph database path.
    pub db: String,
    /// Roots.
    pub roots: Vec<String>,
    /// Skipped directory names.
    pub skip_dirs: Vec<String>,
    /// Size cap.
    pub max_file_kb: u64,
    /// Enabled languages.
    pub languages: Vec<Lang>,
    /// Color mode.
    pub color: ColorMode,
    /// MCP refresh budget.
    pub refresh_budget_ms: u64,
    /// Parser threads.
    pub threads: usize,
}

impl Config {
    /// `max_file_kb` in bytes.
    pub fn max_file_bytes(&self) -> u64 {
        self.max_file_kb.saturating_mul(1024)
    }

    /// View for `config show`.
    pub fn view(&self, path: &Path) -> ConfigView {
        ConfigView {
            path: path.display().to_string(),
            exists: path.exists(),
            db: db_path().display().to_string(),
            roots: self.roots.iter().map(|p| p.display().to_string()).collect(),
            skip_dirs: self.skip_dirs.clone(),
            max_file_kb: self.max_file_kb,
            languages: self.languages.clone(),
            color: self.color,
            refresh_budget_ms: self.refresh_budget_ms,
            threads: self.threads,
        }
    }
}

/// The user's home directory.
pub fn home_dir() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .filter(|v| !v.is_empty())
        .map_or_else(|| PathBuf::from("."), PathBuf::from)
}

/// Kit config directory (`KIT_CONFIG_DIR` or `~/.config/kit`).
pub fn config_dir() -> PathBuf {
    match std::env::var_os("KIT_CONFIG_DIR") {
        Some(d) if !d.is_empty() => PathBuf::from(d),
        _ => home_dir().join(".config").join("kit"),
    }
}

/// Path of `codegraph.toml`.
pub fn config_path() -> PathBuf {
    config_dir().join(format!("{TOOL}.toml"))
}

/// Per-tool data directory (`<KIT_DATA_DIR>/codegraph` or `<config dir>/codegraph`).
pub fn data_dir() -> PathBuf {
    match std::env::var_os("KIT_DATA_DIR") {
        Some(d) if !d.is_empty() => PathBuf::from(d).join(TOOL),
        _ => config_dir().join(TOOL),
    }
}

/// Path of the SQLite graph.
pub fn db_path() -> PathBuf {
    data_dir().join("graph.db")
}

/// Expand a leading `~` and make absolute (without requiring existence).
pub fn expand(p: &str) -> PathBuf {
    let p = p.trim();
    let path = if p == "~" {
        home_dir()
    } else if let Some(rest) = p.strip_prefix("~/").or_else(|| p.strip_prefix("~\\")) {
        home_dir().join(rest)
    } else {
        PathBuf::from(p)
    };
    crate::util::absolute(&path)
}

/// Load config from the default path. Returns the config plus warnings.
pub fn load() -> Result<(Config, Vec<String>)> {
    let path = config_path();
    match std::fs::read_to_string(&path) {
        Ok(text) => parse(&text).map_err(|e| match e {
            Error::Usage(m) => Error::Usage(format!("{}: {m}", path.display())),
            other => other,
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok((Config::default(), Vec::new())),
        Err(e) => Err(Error::io(format!("read {}", path.display()), e)),
    }
}

fn bad(key: &str, why: &str) -> Error {
    Error::Usage(format!("config key `{key}`: {why}"))
}

fn str_array(key: &str, value: &toml::Value) -> Result<Vec<String>> {
    value
        .as_array()
        .ok_or_else(|| bad(key, "expected an array of strings"))?
        .iter()
        .map(|v| v.as_str().map(str::to_string))
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| bad(key, "expected an array of strings"))
}

/// An integer in `min..=max` (`min` is 0 or 1).
fn bounded(key: &str, value: &toml::Value, min: u64, max: u64) -> Result<u64> {
    value
        .as_integer()
        .and_then(|n| u64::try_from(n).ok())
        .filter(|n| (min..=max).contains(n))
        .ok_or_else(|| bad(key, &format!("expected an integer in {min}..={max}")))
}

/// Parse config text. Unknown keys become warnings.
pub fn parse(text: &str) -> Result<(Config, Vec<String>)> {
    let table: toml::Table = text
        .parse()
        .map_err(|e: toml::de::Error| Error::Usage(format!("invalid TOML: {}", e.message())))?;
    let mut cfg = Config::default();
    let mut warnings = Vec::new();
    for (key, value) in &table {
        match key.as_str() {
            "roots" => {
                let roots = str_array(key, value)?;
                if roots.is_empty() {
                    return Err(bad(key, "needs at least one directory"));
                }
                cfg.roots = roots.iter().map(|r| expand(r)).collect();
            }
            "skip_dirs" => cfg.skip_dirs = str_array(key, value)?,
            "max_file_kb" => cfg.max_file_kb = bounded(key, value, 1, MAX_FILE_KB_LIMIT)?,
            "refresh_budget_ms" => {
                cfg.refresh_budget_ms = bounded(key, value, 0, u64::from(u32::MAX))?;
            }
            "threads" => cfg.threads = bounded(key, value, 0, THREADS_LIMIT)? as usize,
            "languages" => {
                let names = str_array(key, value)?;
                let mut langs = Vec::new();
                for n in &names {
                    let l = Lang::parse(&n.to_ascii_lowercase()).ok_or_else(|| {
                        bad(
                            key,
                            &format!(
                                "unknown language `{n}` (known: {})",
                                Lang::ALL.map(Lang::as_str).join(", ")
                            ),
                        )
                    })?;
                    langs.push(l);
                }
                cfg.languages = langs;
            }
            "color" => {
                cfg.color = match value.as_str() {
                    Some("auto") => ColorMode::Auto,
                    Some("always") => ColorMode::Always,
                    Some("never") => ColorMode::Never,
                    _ => return Err(bad(key, "expected \"auto\", \"always\" or \"never\"")),
                }
            }
            other => warnings.push(format!(
                "unknown config key `{other}` (known: {})",
                KNOWN_KEYS.join(", ")
            )),
        }
    }
    Ok((cfg, warnings))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_is_default() {
        let (cfg, warn) = parse("").unwrap();
        assert_eq!(cfg, Config::default());
        assert_eq!(warn, [] as [String; 0]);
    }

    #[test]
    fn full_config() {
        let text = r#"
            roots = ["C:/code", "~/work"]
            skip_dirs = ["gen"]
            max_file_kb = 64
            languages = ["go", "Rust"]
            color = "never"
            refresh_budget_ms = 0
            threads = 4
            bogus = true
        "#;
        let (cfg, warn) = parse(text).unwrap();
        assert_eq!(cfg.roots.len(), 2);
        assert!(cfg.roots[1].ends_with("work"));
        assert_eq!(cfg.skip_dirs, ["gen"]);
        assert_eq!(cfg.max_file_kb, 64);
        assert_eq!(cfg.languages, [Lang::Go, Lang::Rust]);
        assert_eq!(cfg.color, ColorMode::Never);
        assert_eq!(cfg.refresh_budget_ms, 0);
        assert_eq!(cfg.threads, 4);
        assert_eq!(warn.len(), 1);
        assert!(warn[0].contains("bogus"));
    }

    #[test]
    fn invalid_values_are_usage_errors() {
        for text in [
            "roots = []",
            "roots = [1]",
            "max_file_kb = 0",
            "max_file_kb = -1",
            "max_file_kb = 9223372036854775807",
            "threads = 4096",
            "refresh_budget_ms = -5",
            "languages = [\"cobol\"]",
            "color = 1",
            "not toml",
        ] {
            let err = parse(text).unwrap_err();
            assert_eq!(err.code(), "usage", "{text}");
        }
    }
}

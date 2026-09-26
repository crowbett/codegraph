//! Where a file sits in its language's module system: Go package import path,
//! Rust crate + module path, Python dotted module, JS/TS module path.
//!
//! These "units" drive cross-file resolution and the qualified-name prefix.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::lang::Lang;

/// A file's position in its module system.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FileUnit {
    /// Resolution unit inside the package: Go dir, Rust module path, Python module,
    /// JS/TS path without extension, C/C++ path.
    pub module: String,
    /// Package: Go import path, Rust crate name, Python parent package, JS/C dir.
    pub package: String,
    /// Prefix for qualified names (`rules`, `store`, `pkg.models`, `date`).
    pub prefix: String,
}

/// Per-repo cache of go.mod / Cargo.toml lookups.
#[derive(Debug)]
pub struct Layout {
    root: PathBuf,
    gomod: HashMap<String, Option<(String, String)>>,
    cargo: HashMap<String, Option<(String, String)>>,
}

fn parent_rel(rel: &str) -> &str {
    rel.rfind('/').map_or("", |i| &rel[..i])
}

fn file_name(rel: &str) -> &str {
    rel.rfind('/').map_or(rel, |i| &rel[i + 1..])
}

fn strip_ext(rel: &str) -> &str {
    let name = file_name(rel);
    match name.rfind('.') {
        Some(i) if i > 0 => &rel[..rel.len() - (name.len() - i)],
        _ => rel,
    }
}

/// Read `module <path>` from a go.mod.
pub fn go_module_path(text: &str) -> Option<String> {
    text.lines().find_map(|l| {
        let l = l.trim();
        l.strip_prefix("module")
            .filter(|r| r.starts_with([' ', '\t']))
            .map(|r| r.trim().trim_matches('"').to_string())
    })
}

fn cargo_package_name(text: &str) -> Option<String> {
    let t: toml::Table = text.parse().ok()?;
    let name = t.get("package")?.get("name")?.as_str()?;
    Some(name.replace('-', "_"))
}

impl Layout {
    /// New layout for a repo root.
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            gomod: HashMap::new(),
            cargo: HashMap::new(),
        }
    }

    /// Find the nearest ancestor dir (repo-relative, inclusive) holding `marker`
    /// whose contents `parse` accepts. Returns (dir, parsed).
    fn find_up(
        root: &Path,
        cache: &mut HashMap<String, Option<(String, String)>>,
        dir: &str,
        marker: &str,
        parse: fn(&str) -> Option<String>,
    ) -> Option<(String, String)> {
        if let Some(hit) = cache.get(dir) {
            return hit.clone();
        }
        let path = if dir.is_empty() {
            root.join(marker)
        } else {
            root.join(dir).join(marker)
        };
        let here = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| parse(&t))
            .map(|v| (dir.to_string(), v));
        let found = match here {
            Some(h) => Some(h),
            None if dir.is_empty() => None,
            None => Self::find_up(root, cache, parent_rel(dir), marker, parse),
        };
        cache.insert(dir.to_string(), found.clone());
        found
    }

    /// Compute the unit of a repo-relative file.
    pub fn unit(&mut self, rel: &str, lang: Lang, declared_pkg: Option<&str>) -> FileUnit {
        let dir = parent_rel(rel);
        match lang {
            Lang::Go => {
                let found = Self::find_up(&self.root, &mut self.gomod, dir, "go.mod", |t| {
                    go_module_path(t)
                });
                let package = match found {
                    Some((moddir, modpath)) => {
                        let sub = if moddir.is_empty() {
                            dir
                        } else {
                            dir.strip_prefix(moddir.as_str())
                                .map_or(dir, |s| s.trim_start_matches('/'))
                        };
                        if sub.is_empty() {
                            modpath
                        } else {
                            format!("{modpath}/{sub}")
                        }
                    }
                    None => dir.to_string(),
                };
                FileUnit {
                    module: dir.to_string(),
                    package,
                    prefix: declared_pkg.unwrap_or_default().to_string(),
                }
            }
            Lang::Rust => {
                let found = Self::find_up(&self.root, &mut self.cargo, dir, "Cargo.toml", |t| {
                    cargo_package_name(t)
                });
                let (crate_dir, crate_name) =
                    found.unwrap_or_else(|| (String::new(), "crate".to_string()));
                let in_crate = if crate_dir.is_empty() {
                    rel
                } else {
                    rel.strip_prefix(crate_dir.as_str())
                        .map_or(rel, |s| s.trim_start_matches('/'))
                };
                let (package, module) = match in_crate.strip_prefix("src/") {
                    Some(rest) if !rest.starts_with("bin/") => {
                        let no_ext = strip_ext(rest);
                        let mut segs: Vec<&str> = no_ext.split('/').collect();
                        if segs.len() > 1 && segs.last() == Some(&"mod") {
                            segs.pop();
                        }
                        // `src/lib.rs` / `src/main.rs` are the crate root.
                        let module = if no_ext == "lib" || no_ext == "main" {
                            String::new()
                        } else {
                            segs.join("::")
                        };
                        (crate_name, module)
                    }
                    _ => (
                        format!("{crate_name}@{}", strip_ext(in_crate)),
                        String::new(),
                    ),
                };
                FileUnit {
                    prefix: module.clone(),
                    module,
                    package,
                }
            }
            Lang::Python => {
                let no_ext = strip_ext(rel);
                let no_ext = no_ext.strip_prefix("src/").unwrap_or(no_ext);
                let mut segs: Vec<&str> = no_ext.split('/').collect();
                let is_init = segs.last() == Some(&"__init__");
                if is_init {
                    segs.pop();
                }
                let module = segs.join(".");
                let package = if is_init {
                    module.clone()
                } else {
                    segs[..segs.len().saturating_sub(1)].join(".")
                };
                FileUnit {
                    prefix: module.clone(),
                    module,
                    package,
                }
            }
            Lang::TypeScript | Lang::Tsx | Lang::JavaScript => {
                let module = strip_ext(rel).to_string();
                let stem = file_name(&module);
                let prefix = if stem == "index" {
                    file_name(dir).to_string()
                } else {
                    stem.to_string()
                };
                FileUnit {
                    module,
                    package: dir.to_string(),
                    prefix,
                }
            }
            Lang::C | Lang::Cpp => FileUnit {
                module: rel.to_string(),
                package: dir.to_string(),
                prefix: String::new(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn go_and_rust_units() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("go.mod"), "module example.com/app\n\ngo 1.22\n").unwrap();
        std::fs::create_dir_all(root.join("svc/src/a")).unwrap();
        std::fs::write(
            root.join("svc/Cargo.toml"),
            "[package]\nname = \"my-svc\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        let mut l = Layout::new(root);
        let u = l.unit("internal/x/y.go", Lang::Go, Some("x"));
        assert_eq!(u.package, "example.com/app/internal/x");
        assert_eq!(u.module, "internal/x");
        assert_eq!(u.prefix, "x");
        assert_eq!(
            l.unit("main.go", Lang::Go, Some("main")).package,
            "example.com/app"
        );

        let u = l.unit("svc/src/a/b.rs", Lang::Rust, None);
        assert_eq!((u.package.as_str(), u.module.as_str()), ("my_svc", "a::b"));
        assert_eq!(l.unit("svc/src/a/mod.rs", Lang::Rust, None).module, "a");
        assert_eq!(l.unit("svc/src/lib.rs", Lang::Rust, None).module, "");
        assert_eq!(
            l.unit("svc/tests/it.rs", Lang::Rust, None).package,
            "my_svc@tests/it"
        );
    }

    #[test]
    fn python_and_js_units() {
        let mut l = Layout::new(Path::new("."));
        let u = l.unit("src/pkg/sub/mod.py", Lang::Python, None);
        assert_eq!(
            (u.module.as_str(), u.package.as_str()),
            ("pkg.sub.mod", "pkg.sub")
        );
        let u = l.unit("pkg/__init__.py", Lang::Python, None);
        assert_eq!((u.module.as_str(), u.package.as_str()), ("pkg", "pkg"));
        let u = l.unit("src/lib/index.ts", Lang::TypeScript, None);
        assert_eq!(
            (u.module.as_str(), u.prefix.as_str()),
            ("src/lib/index", "lib")
        );
        assert_eq!(
            go_module_path("// c\nmodule   a/b\n").as_deref(),
            Some("a/b")
        );
    }
}

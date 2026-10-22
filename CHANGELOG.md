# Changelog

All notable changes to codegraph. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/);
versions follow [SemVer](https://semver.org/).

## [Unreleased]

### Security
- Path arguments never follow a link inside a root. `confine` used `canonicalize`, which
  follows symlinks and junctions before the root check, so a link planted in a cloned repo
  could point an MCP `file`/`target` argument at a UNC share (SMB authentication, NTLM hash
  leak). Paths are now resolved component by component: a link under a root is refused, a
  link outside the roots (a symlinked `~/dev`, macOS `/var`) is followed one hop at a time
  with its target checked lexically for a remote path first. `find_files` and `impact` no
  longer probe the filesystem (`exists()`) for untrusted arguments at all.
- `is_remote_path` also rejects the NT object-namespace spelling `\??\UNC\host\share`.
- On-the-fly `outline` (CLI and `codegraph_outline`) parsed any supported-language file under
  a root, including gitignored, hidden and `skip_dirs` files, and returned their first lines
  as signatures (a gitignored `local_settings.py`, a `.venv` file). It now only parses a file
  inside an indexed repo that an index pass would include, applying the walker's own rules
  (`.gitignore`, hidden entries, `skip_dirs`, no links, no secret files), through the
  bounded repo-file reader.
- The indexer read each source file with an unbounded `fs::read` after checking the size
  the directory walk reported; a file that grew or was swapped in between could exhaust
  memory. Reads now go through `util::read_bounded`: a regular file checked on the open
  handle, read with `take(max + 1)`, refused over `max_file_kb`.

### Fixed
- Overlapping index roots (`~/dev` and `~/dev/go`) indexed every nested repo twice per run
  and flipped its name between `go/pulse` and `pulse`. A root inside another root is now
  skipped with one warning on stderr, repos are deduplicated by canonical path, and a repo
  is always named relative to its outermost root.
- `codegraph mcp` exited 1 when the client closed the pipe mid-session (its own exit or
  crash while a response was pending). A closed pipe on either side is now an ordinary end
  of session: one stderr line and exit 0. Other I/O errors still exit non-zero.
- `tools/call` with an unknown tool name ran the per-call auto-refresh before failing; the
  name is now validated first and the -32602 error is returned without touching the index.
- An index run that died between a repo's write commit and its resolve (Ctrl-C, a killed
  `watch` or MCP server) left that repo with unresolved or missing edges until
  `index --full`. The write transaction now resets the repo's `indexed_at`, and the next
  run repairs it: a whole-repo resolve when edges are stored, a cold reload when a cold
  load never got to insert them.
- Config bounds: `max_file_kb` is capped at 4,194,304 (4 GiB) so the byte cap cannot
  overflow, `threads` at 1024 (rayon would try to allocate a pool that size), and
  `refresh_budget_ms` at 2^32-1. Out-of-range values are `usage` errors like other bad
  config values.

### Changed
- README: reviewer-facing opening, a verified "try it" walkthrough, an architecture diagram,
  design trade-offs and a what-next list; docs/PACKAGES.md covers every source and test file;
  docs/COMPARISON.md numbers match docs/ACCURACY.md and docs/AGENT-EVAL.md.

## [0.1.2] - 2026-10-20

### Added
- `codegraph watch` (notify-based, per-repo debounce, NDJSON with `--json`) and
  `codegraph mcp --watch`; a resident resolver keeps a big repo's state in memory between
  re-indexes (a one-line edit in Linux re-resolves in 3.5-3.7 s).
- Typed-receiver resolution (`src/infer.rs` + `src/resolve_types.rs`): receiver calls resolve
  through an inferred type; Go implicit interface satisfaction compares parameter types;
  interface/trait/base-class dispatch annotated `[via Trait::method]` in `callers`/`impact`.
- Same-file Rust `macro_rules!` expansion, TS/JS/Python/Rust names used as values, Rust
  types in path-qualifier position, Python attribute reads of imported modules.
- Accuracy benchmark against SCIP ground truth (`task bench:accuracy`, docs/ACCURACY.md) and
  the grep-vs-codegraph agent eval (`task eval:agent`, docs/AGENT-EVAL.md).
- Multi-platform release workflow (five archives, `SHA256SUMS`, SLSA build provenance),
  `packaging/install.sh` and `install.ps1`, scoop/winget/Homebrew/npm manifests.
- Hardening: per-file extraction time budgets and match limits (hostile nesting), bounded
  MCP request lines (4 MiB), 256 KB response cap, UNC/device paths rejected lexically, path
  arguments confined to the configured roots, credential files never indexed or read,
  `def`/`refs` only show the exact file content that was indexed (symlink swaps and edits
  are reported stale).

### Changed
- Callers recall 77.0% -> 99.5%, false call edges 13.8% -> 0.6%, refs recall 97.7% -> 99.2%
  on the 16-repo corpus; confidence tiers calibrated (`exact` 99.9% correct).
- Cold index of a 60k-file repo traced to SQLite I/O and fixed with bulk loads and deferred
  index/FTS builds (Linux v6.12: 114-129 s -> 91.8 s); the whole `~/dev` cold index
  7.0 s -> 4.7 s. Schema version 19.
- CI: Linux + Windows on every push, macOS weekly; Rust 1.99 clippy clean.

## [0.1.1] - 2026-09-25

### Added
- Tag-triggered release workflow (Windows zip).

## [0.1.0] - 2026-09-25

### Added
- Initial release: tree-sitter symbol + call graph for Go, Rust, C, C++, TypeScript, TSX,
  JavaScript and Python in SQLite; `index`, `find`, `def`, `refs`, `callers`, `callees`,
  `outline`, `path`, `impact`, `hotspots`, `dot`, `status`, `config`; stdio MCP server with
  read-only tools and per-call auto-refresh; kit `--json` envelope and exit codes.

[Unreleased]: https://github.com/crowbett/codegraph/compare/v0.1.2...HEAD
[0.1.2]: https://github.com/crowbett/codegraph/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/crowbett/codegraph/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/crowbett/codegraph/releases/tag/v0.1.0

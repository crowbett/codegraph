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
- `find` accepted a query of any length, and every 2+ character query without `.`/`:`
  loaded every distinct symbol name of the searched repos for fuzzy matching. Queries over
  256 bytes are now rejected (`-32602` over MCP, a usage error on the CLI), and the fuzzy
  tier is skipped when the searched repos hold more than 300,000 symbols (from the stored
  per-repo counts; the exact, qualified, prefix, substring and doc tiers still run).
- Constant, variable and `#define` signatures no longer carry their initializer:
  `API_KEY = "sk_live_..."` is stored and shown as `API_KEY = …`, `#define TOKEN "x"` as
  `#define TOKEN …`, so a secret literal in source never reaches `find`, `outline`, `impact`
  or the MCP tools. Schema version 21.

### Fixed
- A Go receiver with `]` before `[` (`func (s /*]*/ T /*[*/) F()`, or plain broken
  input) sliced out of bounds in the receiver type-parameter scan and panicked the parser
  thread: with `panic = "abort"` one such file killed `codegraph index` and the MCP server
  on every refresh, and debug builds silently dropped the file. The scan is bounds-checked,
  the release profile unwinds again, a parser panic on one file is caught in the indexer
  (logged, file skipped), and a parser thread that dies for any other reason fails the run
  instead of committing a partial repo.
- The "last run died before its resolve" repair keyed on `indexed_at = 0` alone, which is
  also the state of a run in progress on another connection (an MCP refresh during a CLI
  `index`): the refresh wiped the repo's file rows and the other run then attached its
  edges to reused ids. The write transaction now stamps `repos.write_started`; a repo is
  repaired only when its owner process (`pid:nonce`, also stored) is gone and the stamp,
  re-set right before the commit so a long cold parse does not count, is older than 15
  minutes; the check runs inside the (IMMEDIATE) write transaction.
- The MCP refresh budget did not cover waiting for SQLite's write lock: while a CLI
  `index --full` ran, every tool call stalled for the connection's full 10 s busy timeout,
  and the failed refresh was retried on the very next call. The busy timeout is now
  lowered to the remaining budget for the duration of the refresh, and a failed refresh
  counts as the repo's last attempt (next try after the usual 3 s).
- A refresh that found nothing to do still wrote three rows (`repos` upsert, `indexed_at`
  reset, `indexed_at` set) and committed, bumping SQLite's `data_version` and making the
  `watch`/`mcp --watch` resolver cache drop its state on every tick. A no-op refresh now
  rolls back and skips `set_indexed`; the repo upsert only writes when the name changed.
- A file that could not be read during a refresh (held open without sharing by an
  editor or Defender on Windows, permission denied) was deleted from the graph as if it
  had vanished. Such files now keep their last indexed state; the run counts them
  (`failed` in the summary and `--json`) and says so on stderr. Missing and oversize files
  are still dropped.
- Edits to `go.mod`, `Cargo.toml`, `package.json` and `tsconfig.json` were never noticed:
  the watcher ignored those names and unchanged source files were never re-unitized, so
  module paths went stale until `index --full`. Each repo now stores a fingerprint of its
  marker files; when it changes every stored file of the repo is invalidated and re-parsed
  by the normal incremental path (so a budgeted MCP refresh finishes the job over several
  passes), and the watcher treats those files as repo changes.
- `mcp --watch` (which never adds or drops repos) discarded dropped-event and watcher-error
  notices because it may not rescan a root, so the graph went stale silently after a burst
  of changes. It now queues every known repo under the watched roots for a refresh instead.
- The resident resolver (`watch`, `mcp --watch`) tagged its state with `PRAGMA data_version`
  read after its own commit; a commit from another connection landing in that window was
  absorbed into the tag and the stale state kept. The version is now read inside the write
  transaction and returned with the resolve result.
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

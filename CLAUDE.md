# codegraph — CLAUDE.md

Tree-sitter symbol + call graph of every repo under the configured roots, served to agents
over MCP (Rust, `codegraph.exe`). Part of **kit**: follow `C:\Users\corbe\dev\KIT.md`
(config paths, `--json` envelope, exit codes, MCP rules). Do not edit KIT.md.

## Layout

```
queries/<lang>/tags.scm  tree-sitter tag queries (tags.scm conventions + @qualifier/@receiver/
                         @decl/@impl_for/@import/@export/@package). typescript/jsx.scm is appended for .tsx
src/main.rs      binary: clap parse, output mode, dispatch (anyhow; everything else is thiserror)
src/cli.rs       clap derive definitions
src/lib.rs       crate root (`unsafe_code = "forbid"`)
src/lang.rs      Lang enum, grammars, compiled queries (OnceLock), thread-local parsers
src/extract.rs   one file -> symbols (nesting, containers, docs, signatures, exported),
                 refs (call/ref/impl/inherit/value), imports (per-language binding parsers)
src/layout.rs    module-system units: Go import path (go.mod), Rust crate+module path,
                 Python dotted module, JS/TS path; qualified-name prefixes
src/store.rs     SQLite schema (repos/files/symbols/edges + FTS5 trigram), file replace
src/index.rs     walk roots (ignore crate), repo detection (.git), mtime/size -> xxh3 change
                 detection, rayon parse, per-repo transaction, then resolve
src/resolve.rs   per-repo resolution: import scopes, qualifier rules, confidence
src/query.rs     find/def/refs/callers/callees/outline/path/impact/hotspots/dot/status
src/render.rs    human text (also the MCP `content` text, uncolored)
src/output.rs    --json envelope, ANSI Style, Table; src/config.rs codegraph.toml
src/mcp.rs       hand-rolled stdio MCP server (read-only tools, auto-refresh of the asked repo)
tests/fixtures/<lang>/  mini projects; tests/lang.rs snapshots them (insta) + asserts resolution
tests/graph.rs incremental.rs cli.rs   graph algorithms, re-index, binary + MCP stdio + self-index
```

## Commands

```powershell
cargo fmt --check ; cargo clippy --all-targets -- -D warnings ; cargo test
cargo build --release
task snapshots   # re-record fixture snapshots (INSTA_UPDATE=always), then review the diff
task release     # dist\codegraph-0.1.1-windows-amd64.zip
```

Smoke against a throwaway data dir (never the real `~/.config/kit`):

```powershell
$env:KIT_DATA_DIR = "$env:TEMP\cg" ; .\target\release\codegraph.exe index C:\Users\corbe\dev -v
```

## Rules

- Library code uses `crate::error::Error` (thiserror); only `main.rs` uses anyhow. No
  `unwrap`/`expect` outside tests (clippy `unwrap_used`/`expect_used` are deny).
- Adding a language: grammar crate in Cargo.toml, `Lang` variant + extension, a
  `queries/<lang>/tags.scm`, binding parser in `extract::parse_import`, a unit rule in
  `layout.rs`, a fixture + snapshot + resolution assertions in `tests/lang.rs`.
- Changing extraction output or the schema: bump `store::SCHEMA_VERSION` (old graphs are
  wiped and rebuilt on next `index`) and re-record snapshots.
- Resolution must stay honest: when a qualifier is bound to an import that points outside
  the repo, resolve to nothing rather than guessing by name. Guesses are `name_only`.
- MCP tools are read-only; stdout is protocol only, logs go to stderr. Keep tool output
  compact (repo-relative paths under a repo header, `file:line` everywhere).

## Definition of done

`cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` and `cargo test` pass,
`cargo build --release` succeeds, and a smoke run of the release binary works: `index` of
`~/dev` into a temp `KIT_DATA_DIR`, `find` / `callers` / `outline` / `impact` on real symbols,
and an MCP stdio exchange.

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
                 refs (call/ref/impl/inherit/value, incl. calls inside macro token trees),
                 imports (per-language binding parsers); wires src/infer.rs hints onto
                 receiver-expression call refs
src/infer.rs     extraction-time type hints for receiver calls (`x.foo()`): a small grammar
                 (`S`, `T:<type>`, `C:<callee>`, `M:<name>@<hint>`, `F:<name>@<hint>`,
                 `Q:`/`A:` unwrap/await, `E:` element, `L` = unknown local) evaluated later
                 by resolve_types.rs — module doc has the full table, read it before touching
                 either file
src/layout.rs    module-system units: Go import path (go.mod), Rust crate+module path,
                 Python dotted module, JS/TS path (incl. workspace packages); qualified-name
                 prefixes
src/store.rs     SQLite schema (repos/files/symbols/edges + FTS5 trigram), file replace,
                 SCHEMA_VERSION
src/index.rs     walk roots (ignore crate), repo detection (.git), mtime/size -> xxh3 change
                 detection, rayon parse, per-repo transaction, then resolve
src/resolve.rs   per-repo resolution: import scopes, qualifier rules, confidence, Go implicit
                 interface satisfaction; `#[path = "resolve_types.rs"] mod types` for typed
                 receivers
src/resolve_types.rs   evaluates infer.rs hints against the whole repo: Known(symbol) /
                 External / Unknown, passthrough + Self-constructor method tables
src/query.rs     find/def/refs/callers/callees/outline/path/impact/hotspots/dot/status;
                 dispatch-aware callers/impact (`[via Trait::method]`)
src/render.rs    human text (also the MCP `content` text, uncolored); `via`/confidence marks
src/output.rs    --json envelope, ANSI Style, Table; src/config.rs codegraph.toml
src/mcp.rs       hand-rolled stdio MCP server (read-only tools, auto-refresh of the asked repo)
src/watch.rs     `watch` / `mcp --watch`: notify events -> path filter -> per-repo debounce ->
                 index::refresh_repo (root rescan when a repo appears/vanishes); busy retry
tests/fixtures/<lang>/  mini projects; tests/lang.rs snapshots them (insta) + asserts resolution
tests/fixtures/{go,rust,ts}_infer/  typed-receiver resolution fixtures, asserted in tests/resolve.rs
tests/graph.rs incremental.rs cli.rs resolve.rs   graph algorithms, re-index, binary + MCP
                 stdio + self-index, typed-receiver resolution
tests/watch.rs   watcher: edit/create/delete/new repo reach the DB, concurrent writers, NDJSON
bench/           standalone crate (`codegraph-bench`, own [workspace], not shipped): clones
                 the pinned corpus, builds SCIP ground truth, scores codegraph against it
                 (docs/ACCURACY.md has the method). bench/corpus.toml, bench/baseline/ (frozen
                 pre-resolver-work numbers), bench/results/ (latest recorded run)
evals/agent/     grep-vs-codegraph agent eval: cases.py (questions + SCIP/source-verified
                 answers), gen.py (renders suite.yaml + fake-rules.json), proxy.py (the agent
                 loop, OpenAI-compatible), run.py (one-command runner), truth.py (SCIP
                 verification). See docs/AGENT-EVAL.md
packaging/       install.sh / install.ps1 (checksum-verified installers), npm/ (per-platform
                 packages), homebrew/codegraph.rb, scoop/, winget/. See docs/INSTALL.md
docs/            ACCURACY.md (benchmark method + results), AGENT-EVAL.md (agent token/pass-rate
                 eval), INSTALL.md (every install path), OVERNIGHT.md (running log of the 1.0
                 push — read it for *why*, not just *what*), COMPARISON.md (vs grep/ctags/LSP/
                 SCIP/GitHub search)
```

## Commands

```powershell
cargo fmt --check ; cargo clippy --all-targets -- -D warnings ; cargo test
cargo build --release
task snapshots   # re-record fixture snapshots (INSTA_UPDATE=always), then review the diff
task release     # dist\codegraph-<ver>-windows-amd64.zip
```

Resolver/accuracy work — **measure before keeping a change**:

```powershell
task bench:accuracy   # score against frozen SCIP ground truth, delta vs bench/baseline/; miss
                       # reports in .cache/bench/report/*.tsv (clones the corpus + builds 4
                       # SCIP indexers on first run: network + time)
task bench:record     # once you're keeping the change: writes bench/results/ + docs/ACCURACY.md
```

Agent eval (grep vs codegraph tool cost/correctness):

```powershell
task eval:agent        # fake LLM, no API key, no spend, rewrites docs/AGENT-EVAL.md
task eval:agent:real   # needs ANTHROPIC_API_KEY; estimate then a $5-capped real run
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
- Changing extraction output or the schema: bump `store::SCHEMA_VERSION` (currently **2**;
  old graphs are wiped and rebuilt on next `index`) and re-record snapshots.
- **Any resolver change (`src/resolve.rs`, `src/resolve_types.rs`, `src/infer.rs`) must be
  measured with `task bench:accuracy` before it's kept.** Compare against
  `bench/baseline/accuracy.json` and the last recorded `bench/results/accuracy.json`; a
  change that regresses recall/precision on any language, or calibration (`exact`/`likely`/
  `name_only`), gets reverted or fixed, not shipped on vibes. Read the miss reports in
  `.cache/bench/report/*.tsv` before assuming a regression is a harness bug.
- Adding or changing a type hint: the grammar lives in `src/infer.rs`'s module doc (`S`,
  `T:`, `C:`, `M:`, `F:`, `Q:`/`A:`, `E:`, `L`) — read it, keep new hints inside `MAX_DEPTH`/
  `MAX_HINT`, and add fixture coverage under `tests/fixtures/{go,rust,ts}_infer/` plus an
  assertion in `tests/resolve.rs`.
- Resolution must stay honest: when a qualifier is bound to an import that points outside
  the repo, resolve to nothing rather than guessing by name. Guesses are `name_only`. This
  also holds across repos in the same graph: cross-repo edges are never linked by name.
- MCP tools are read-only; stdout is protocol only, logs go to stderr. Keep tool output
  compact (repo-relative paths under a repo header, `file:line` everywhere).

## Definition of done

`cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` and `cargo test` pass,
`cargo build --release` succeeds, and a smoke run of the release binary works: `index` of
`~/dev` into a temp `KIT_DATA_DIR`, `find` / `callers` / `outline` / `impact` on real symbols,
and an MCP stdio exchange.

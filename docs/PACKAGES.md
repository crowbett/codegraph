# Source layout

Moved verbatim from CLAUDE.md.


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
                 index::refresh_repo_cached (root rescan when a repo appears/vanishes); busy retry.
                 The watcher and the MCP server keep a big repo's resolver state in memory
                 (resolve::ResolveCache): an edit splices in the re-parsed files and
                 re-resolves without reloading the repo; `CODEGRAPH_VERIFY_RESIDENT=1` checks
                 each such resolve against a whole-repo one
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
                 eval), INSTALL.md (every install path), COMPARISON.md (vs grep/ctags/LSP/
                 SCIP/GitHub search)
```


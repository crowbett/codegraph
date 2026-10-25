# Source layout

What every file in the repo is for. Keep it current when files are added.

```
queries/<lang>/tags.scm  tree-sitter tag queries (tags.scm conventions + @qualifier/@receiver/
                         @decl/@impl_for/@import/@export/@package). typescript/jsx.scm is appended for .tsx
src/main.rs      binary: clap parse, output mode, dispatch (anyhow; everything else is thiserror)
src/cli.rs       clap derive definitions
src/lib.rs       crate root (`unsafe_code = "forbid"`). Features: `graph` (store, index, resolve,
                 query; what wasm32 builds) and `native` (default: graph + CLI, MCP, watcher)
src/clock.rs     Instant / Unix time that also work on wasm32 (zero elapsed, fixed time)
src/error.rs     thiserror `Error` with the kit error codes and exit codes
src/model.rs     shared enums/records: SymKind, EdgeKind, Confidence (exact/likely/name_only)
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
src/macros.rs    Rust `macro_rules!` expansion: same-file invocations are expanded textually
                 (bounded, deduped) and extracted at the invocation site
src/tsfix.rs     TS/TSX parse retry: separates signatures the grammar trips on, kept only
                 when it lowers the ERROR count
src/layout.rs    module-system units: Go import path (go.mod), Rust crate+module path,
                 Python dotted module, JS/TS path (incl. workspace packages); qualified-name
                 prefixes
src/store.rs     SQLite schema (repos/files/symbols/edges + FTS5 trigram), file replace,
                 SCHEMA_VERSION
src/index.rs     walk roots (ignore crate), repo detection (.git), mtime/size -> xxh3 change
                 detection, rayon parse, per-repo transaction, then resolve; index_sources
                 runs the same cold path on in-memory files (no filesystem)
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
src/webdemo.rs   the browser demo's logic: index the bundled sample in memory
                 (index::index_sources) and answer sample/callers/callees as JSON; its tests
                 check it against a disk index of the same files and sample/answers.json
crates/codegraph-web/  wasm32-unknown-unknown cdylib for the site's demo (own workspace and
                 lockfile): demo_alloc/demo_call/demo_free ABI over webdemo, src/sys.rs (the
                 libc bits and no-file VFS bundled SQLite needs), libc/include headers,
                 vendored tree-sitter-language (libc shims emptied), sample/ (the indexed
                 crate + answers.json), check.mjs (0 imports, answers match native)
tests/fixtures/<lang>/  mini projects; tests/lang.rs snapshots them (insta) + asserts resolution
tests/fixtures/{go,rust,ts}_infer/  typed-receiver resolution fixtures, asserted in tests/resolve.rs
tests/graph.rs incremental.rs cli.rs resolve.rs   graph algorithms, re-index, binary + MCP
                 stdio + self-index, typed-receiver resolution
tests/watch.rs   watcher: edit/create/delete/new repo reach the DB, concurrent writers, NDJSON
tests/limits.rs  hostile input (thousand-call chains, 10k-deep generics) extracts in bounded time
tests/security.rs  def/refs only show the file that was indexed: symlink swaps and edits are
                 reported stale, never printed
tests/common/mod.rs  index a fixture into a temp DB, stable graph dump for snapshots
tests/snapshots/  insta snapshots of tests/lang.rs
build.rs         bakes the short git commit into `codegraph version`
Taskfile.yml     task runner: lint/test/release zip, bench:*, eval:*
.github/workflows/  ci.yml (fmt, clippy, tests, release build + npm shim smoke on Linux/Windows,
                 macOS weekly), release.yml (tag -> five archives + SHA256SUMS + attestation,
                 draft release)
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


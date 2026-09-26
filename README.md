# codegraph

A symbol + call graph of every repo you own, built with tree-sitter, stored in SQLite, and
served to AI agents over MCP. It answers the structural questions an agent otherwise
answers with a dozen greps: *where is this defined, who calls it, what does it call, what
breaks if I change this file?*, with `file:line` on every answer.

- **7 languages, one engine**: Go, Rust, C, C++, TypeScript/TSX, JavaScript/JSX, Python, via
  per-language tree-sitter tag queries (`queries/<lang>/tags.scm`).
- **Cross-file resolution** with a confidence on every edge (`exact` / `likely` / `name_only`).
- **Incremental**: only files whose content hash changed are re-parsed (in parallel).
- **MCP server** that re-indexes the repo you're asking about before it answers.

Part of **kit**, a personal developer toolkit. It follows the shared contract (config paths,
`--json` envelope, exit codes, MCP conventions).

## Numbers (real, from `~/dev`)

Release build, Windows 11, 16 cores, on a machine shared with other build jobs:

| run | files | symbols | edges | time |
|---|---:|---:|---:|---:|
| cold index, 41 repos | 2,596 | 32,447 | 298,870 | **6.6 s** (parse 1.1 s · write 4.1 s · resolve 1.2 s) |
| no-op re-index | 2,597 | | | **0.16 s** |
| `index --full` | 2,597 | | | 8.5 s |

Files by language: Go 1,708 · Rust 262 · TypeScript 246 · C++ 141 · TSX 112 · Python 89 ·
JavaScript 34 · C 5. The database is 65 MB.

Resolved edges split 77.8k `exact` / 14.6k `likely` / 9.8k `name_only`. About a third of all
edges resolve. The rest point at the standard library or dependencies (`fmt.Errorf`, `Vec`,
`assert.Equal`), which are outside the graph by design. Queries take milliseconds.

## Install

```powershell
scoop install codegraph            # packaging/scoop/codegraph.json
winget install crowbett.Codegraph
# or from source
cargo install --path .
```

Register with Claude Code:

```powershell
claude mcp add codegraph -- codegraph mcp
```

## Usage

```text
codegraph index [--full] [PATH...]      index the configured roots (default ~/dev), incrementally
codegraph find <name> [--kind fn] [--repo x]
codegraph def <symbol>                  definition + source (with its doc comment)
codegraph refs <symbol> [--kind call|ref|impl|inherit]
codegraph callers <fn> [--depth N]      call hierarchy up (tree)
codegraph callees <fn> [--depth N]      call hierarchy down (tree; externals listed)
codegraph outline <file>                nested symbols with line ranges (-s for signatures)
codegraph path <from> <to>              shortest call path
codegraph impact <file|symbol> [-d N]   what transitively depends on this
codegraph hotspots [--repo x]           most referenced, largest fan-out, dead-code candidates
codegraph dot <symbol> [--format dot|mermaid]
codegraph watch [PATH...]               keep the graph fresh as files change (Ctrl-C stops)
codegraph status | config path|show | version | mcp [--watch]
```

A `<symbol>` can be a simple name (`Evaluate`), a qualified name (`rules.Engine.Evaluate`,
`store::Store::open`), or a location (`internal/engine/engine.go:294`, which picks the
innermost symbol enclosing that line). Global flags: `--json`, `-q/--quiet`, `-v/--verbose`,
`--no-color` (and `NO_COLOR`).

```text
$ codegraph callers FsBackend::check_name --depth 1
backend::FsBackend::check_name (method)  C:/Users/corbe/dev/rust/stash/src/backend.rs:140
├─ backend::FsBackend::read  src/backend.rs:229
├─ backend::FsBackend::read_range  src/backend.rs:235
├─ backend::FsBackend::write  src/backend.rs:247
├─ backend::FsBackend::remove  src/backend.rs:295
└─ backend::FsBackend::exists  src/backend.rs:338

$ codegraph impact Engine.Submit --depth 2
impact of Engine.Submit: 15 dependent symbol(s) in 5 file(s) within depth 2
depth 1 (direct)
  method engine.Engine.Trigger  internal/engine/engine.go:274  via call
  method engine.Engine.chain  internal/engine/engine.go:306  via call
  ...
depth 2
  method api.Server.trigger  internal/api/server.go:230  via call
  method engine.Engine.apply  internal/engine/engine.go:190  via value
```

In trees, `~` marks a `likely` edge and `?` marks `name_only`. `[as value]` means the
function is passed as a callback or handler, not called. `[cycle]` and `[seen above]` mark
recursion and repeated subtrees.

### `watch`

`codegraph watch` indexes the configured roots once (incrementally), then watches them and
re-indexes a repo shortly after its files change: events are batched per repo and flushed
after 300 ms of quiet (`--debounce-ms`), at most 2 s after the first change. It applies the
same filters as `index` (`.gitignore`, `skip_dirs`, languages, `max_file_kb`, credential
files) and ignores `.git/`, `target/`, `node_modules/` and other skipped or hidden
directories, so builds do not trigger re-index storms. A new repo under a root (a `.git`
directory appears) is picked up, a deleted one is dropped. Symlinks and junctions are never
followed. Human lines by default; with `--json` one NDJSON object per line with a `type`:
`ready`, `reindex` (`repo`, `files`, `changed`, `ms`, `stats`), `rescan` (a whole root after
a repo appeared or vanished) and `error`. Ctrl-C exits 0.

```text
$ codegraph watch C:/tmp/root      # smoke run: one repo, edits, then a second repo appears
indexed 1 repos · 18 files (18 parsed, 0 unchanged, 0 removed) · 496 symbols · 5,387 edges (25% resolved) · 0.14s
05:51:11 watching C:/tmp/root (1 repos) — Ctrl-C to stop
05:51:15 reindex demo: 2 parsed, 0 removed · 497 symbols · 30 ms (src/lib.rs, src/util.rs)
05:51:18 rescan C:/tmp/root: 1 parsed, 1 removed · 2 repos · 34 ms (repo boundary changed: second/.git)
```

`codegraph mcp --watch` runs the same watcher in a background thread of the MCP server,
for repos already in the graph (the per-call refresh stays on). The graph is SQLite in WAL
mode: readers never block, and concurrent writers (`watch`, `index`, MCP refresh) wait on a
busy timeout; a batch that still finds the database busy is retried. Re-indexing a repo
re-resolves all of its edges, which is fast for normal repos but takes minutes for a
kernel-sized one (see `docs/SCALE.md`).

### `--json`

Every command prints exactly one `{"ok": true, "data": ...}` document, or
`{"ok": false, "error": {"code", "message"}}` with exit code 2 (usage), 3 (not found) or 1.
Symbols carry `id, name, kind, qname, repo, file (absolute), line, end_line, signature, doc`.

## MCP tools

| tool | answers |
|---|---|
| `codegraph_find` | search definitions by name (exact > qualified > prefix > substring > doc text > fuzzy) |
| `codegraph_def` | the definition's source, line-numbered, with its doc comment |
| `codegraph_refs` | every use site: calls, type refs, impls, inheritance, with the source line |
| `codegraph_callers` / `codegraph_callees` | call hierarchy trees with call sites |
| `codegraph_outline` | nested outline of one file (works on unindexed files too) |
| `codegraph_impact` | transitive dependents of a file or symbol, by distance, plus affected files |
| `codegraph_status` | repos, sizes, resolution rates, freshness |

Each result has `structuredContent` (same shape as `--json` data) and a compact text
rendering. The text uses repo-relative paths under a `# repo root` header, so it stays
token-efficient. All tools are read-only.

**Auto-refresh:** before each call, the server re-indexes the relevant repo within
`refresh_budget_ms`. That is the repo named by `repo`, else the one containing a
`file`/`target` path, else the one containing the server's working directory. A repo is
refreshed at most once every 3 s. Only files whose mtime or size changed are re-hashed, and
only files whose content hash changed are re-parsed.

**Safety:** MCP never adds repos to the graph; only repos indexed with `codegraph index` are
refreshed. Path arguments are canonicalized and must resolve under a configured root. UNC
and device paths (`\\host\share`, `\\?\UNC\...`) are rejected lexically, before any
filesystem call. Unindexed files are outlined only under a root and within `max_file_kb`.
Credential files (`.env*`, `*.pem`, `*.key`, `id_*`, `*.pfx`, `*.p12`, `*.kdbx`,
`credentials*.json`, `secrets.*`, `*.tfstate`, `.npmrc`, `.netrc`) are never indexed or
read. Source snippets have control characters replaced. Responses are capped at 256 KB with
lines clipped at 400 characters; large answers keep a short text summary next to
`structuredContent`.

## Config

`%USERPROFILE%\.config\kit\codegraph.toml` (or `$KIT_CONFIG_DIR\codegraph.toml`). The graph
lives in `<KIT_DATA_DIR or config dir>\codegraph\graph.db`. A missing file means defaults, and
unknown keys produce a warning.

```toml
roots = ["~/dev"]                 # every git repo below becomes a graph repo
skip_dirs = ["vendor", "node_modules", "target", "build", "dist", "out", ".git", ".venv",
             "venv", "__pycache__", ".next", ".cache", "third_party", "bower_components",
             "coverage", "testdata"]   # on top of .gitignore
max_file_kb = 1024                # skip bigger files (bundles, generated code)
languages = ["go", "rust", "c", "cpp", "typescript", "tsx", "javascript", "python"]
color = "auto"                    # auto | always | never
refresh_budget_ms = 1500          # MCP auto-refresh budget per call (0 = off)
threads = 0                       # parser threads (0 = one per core)
```

## How it works

1. **Walk**: the `ignore` crate walks each root (respecting `.gitignore` and `skip_dirs`).
   A file belongs to its nearest ancestor holding `.git`. `.h` is treated as C++ when its
   directory has C++ sources and no `.c` files.
2. **Change detection**: a file is skipped when its mtime and size match the stored row,
   otherwise it is hashed with xxh3. An unchanged hash only updates the mtime.
3. **Extract** (rayon, one parser per thread): the tag query captures definitions,
   references (calls, type uses, `impl Trait for T`, `extends`/`implements`, functions passed
   as values) and imports. Code then computes nesting, containers (Go receivers, Rust `impl`
   types, C++ `A::b` scopes, classes), doc comments, signatures and exported-ness. Language
   builtins (`len`, `println!`, `print`, `string`) are dropped.
4. **Store**: each repo is written in one SQLite transaction (`repos`, `files`, `symbols`,
   `edges`, plus an FTS5 trigram index over names, qualified names and docs).
5. **Resolve**: every repo that changed is re-resolved as a whole, in memory. Import
   bindings are computed first:
   - Go: import paths map to package directories via `go.mod`.
   - Rust: crate and module paths come from the file layout and `mod`, and `use` trees are
     parsed.
   - TS/JS: relative imports, `index` files and tsconfig `baseUrl`/`paths`; default,
     named and namespace imports; `require`.
   - Python: absolute and relative module paths, `from x import y` of submodules.
   - C/C++: `#include` resolution plus header/source companions.

   Then each edge is resolved with ordered rules (see `src/resolve.rs`).

## Precision: be honest

This is a **heuristic graph, not a type checker**. It is right most of the time on idiomatic
code, and it tells you how sure it is:

| confidence | produced by |
|---|---|
| `exact` | same-file / lexical scope; `self`/`this`/Go receiver calls to the enclosing type's own methods; qualifiers bound by an import (`pkg.F`, `ns.f`, `mod::f`, `Type::new` where `Type` is imported); same Go package |
| `likely` | inherited methods (via extends/impl edges); receiver calls where the method's type is mentioned in the file; C/C++ include graph; glob imports; a unique match within the crate |
| `name_only` | receiver calls (`x.foo()`) with only weak evidence (the method's module is imported or in the same package); picked by proximity. Check these by reading the code. |

Known blind spots:

- No type inference. In `s := NewStore(); s.Get()`, `s.Get` only resolves by name.
- Interface/trait dispatch points at whichever method matches: the interface declaration
  or one implementation, not all of them.
- Go implicit interface satisfaction isn't recorded.
- Calls through function-typed fields, reflection, macros that generate code, decorators,
  and dynamic `import()` are invisible. Functions passed as values are captured only when
  they are qualified (`s.handle`) or defined/imported in the same file.
- A qualifier bound to an import outside the repo (stdlib, dependencies) resolves to
  nothing, by design, instead of guessing a same-named symbol. Cross-repo edges (one kit
  repo importing another) are not linked.
- Very common std method names (`get`, `len`, `iter`, `Close`, `String`, `push`, ...)
  called on unknown receivers are never above `name_only` and need direct evidence.
- Rust module paths come from file layout (`src/a/b.rs` is `a::b`); `#[path]` attributes
  and `include!` are ignored. C/C++ macros are not expanded, and function prototypes are not
  symbols (definitions are).
- `hotspots` orphans are *candidates*: entry points, callbacks registered indirectly, and
  exported API are excluded or invisible, so review before deleting anything.

## Development

```powershell
cargo fmt --check ; cargo clippy --all-targets -- -D warnings ; cargo test
task snapshots   # re-record fixture snapshots, then review the diff
task release     # dist\codegraph-0.1.1-windows-amd64.zip
```

Tests:
- `tests/fixtures/<lang>/` mini-projects are snapshotted with `insta` and asserted for the
  classic resolution traps: the same name in two packages, a method vs a function, Go
  receivers, Rust `impl` blocks and traits, TS default vs named exports and path aliases,
  and Python relative imports and inheritance.
- Incremental re-indexing: an edit re-parses one file; a delete drops its symbols with no
  dangling edges.
- Graph algorithms (depth, cycles, path, impact) on a known graph.
- CLI envelopes and exit codes.
- An MCP stdio session that indexes a never-seen repo on demand.
- A self-index test that finds `main`.

## License

MIT © 2026 crowbett

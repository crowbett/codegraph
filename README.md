# codegraph

A symbol + call graph of every repo you own, built with tree-sitter, stored in SQLite, and
served to AI agents over MCP. It answers the structural questions an agent otherwise
answers with a dozen greps: *where is this defined, who calls it, what does it call, what
breaks if I change this file?*, with `file:line` on every answer, and a confidence
(`exact` / `likely` / `name_only`) on every edge so you know how much to trust it.

- **8 languages, one engine**: Go, Rust, C, C++, TypeScript, TSX, JavaScript/JSX, Python,
  via per-language tree-sitter tag queries (`queries/<lang>/tags.scm`).
- **Type-aware resolution**: receiver calls (`x.foo()`) resolve through an inferred type,
  not just a name guess; Go implicit interface satisfaction and dynamic dispatch
  (interfaces, traits, Rust `dyn`) are tracked.
- **Cross-file resolution** with a confidence on every edge, measured against
  compiler-grade ground truth (see [Accuracy](#accuracy)).
- **Incremental**: only files whose content hash changed are re-parsed (in parallel).
- **MCP server** that re-indexes the repo you're asking about before it answers.

Part of **kit**, a personal developer toolkit. It follows the shared contract (config paths,
`--json` envelope, exit codes, MCP conventions).

## Install

See [docs/INSTALL.md](docs/INSTALL.md) for every platform (install scripts, scoop, winget,
Homebrew, npm, `cargo install`) with checksums. Short version:

```powershell
irm https://raw.githubusercontent.com/crowbett/codegraph/main/packaging/install.ps1 | iex
```

```sh
curl -fsSL https://raw.githubusercontent.com/crowbett/codegraph/main/packaging/install.sh | sh
```

**The repo is currently private.** Every install path above needs GitHub auth until it's
public — a `GH_TOKEN`/`gh auth token` for the scripts, SSH for `cargo install --git`;
scoop/winget/Homebrew/npm don't work at all yet. Details and exact commands in
[docs/INSTALL.md](docs/INSTALL.md#while-the-repository-is-private).

## Quickstart

```powershell
codegraph index ~/dev          # index every repo under the configured roots
codegraph find Store           # search
codegraph def Store::open      # definition + doc comment
codegraph callers Store::open  # who calls it
codegraph impact Store::open   # what breaks if it changes
```

Real output, from indexing this repo (`codegraph index`, then a few queries):

```text
$ codegraph find Store --kind struct
# codegraph C:/Users/corbe/dev/rust/codegraph
  struct  store::Store                src/store.rs:159                           pub struct Store
  struct  a.Store                     tests/fixtures/go/a/a.go:5                 type Store struct
  struct  store.Store                 tests/fixtures/go_infer/store/store.go:16  type Store struct
  struct  store::Store                tests/fixtures/rust/src/store.rs:5         pub struct Store
  struct  store::FileRow (substring)  src/store.rs:116                           pub struct FileRow
  struct  store::RepoRow (substring)  src/store.rs:129                           pub struct RepoRow
```

```text
$ codegraph def store::Store::open
store::Store::open method C:/Users/corbe/dev/rust/codegraph/src/store.rs:174-193
173|     /// Open (creating if needed) the database at `path`.
174|     pub fn open(path: &Path) -> Result<Self> {
175|         if let Some(dir) = path.parent() {
176|             std::fs::create_dir_all(dir)
177|                 .map_err(|e| Error::io(format!("create {}", dir.display()), e))?;
178|         }
179|         let conn = Connection::open(path)?;
...
```

```text
$ codegraph callers store::Store::open --depth 1
store::Store::open (method)  C:/Users/corbe/dev/rust/codegraph/src/store.rs:174
├─ accuracy::index_repo  bench/src/accuracy.rs:399
├─ App::open_store  src/main.rs:54
├─ mcp::tests::server  src/mcp.rs:566
├─ store::tests::open_twice_and_version_reset  src/store.rs:481 ~ ×2
├─ index_dir  tests/common/mod.rs:36
├─ edit_touch_delete_and_full  tests/incremental.rs:29
└─ secret_files_are_never_indexed  tests/incremental.rs:96
9 node(s), depth 1; paths relative to each root's repo; ~ likely, ? name-only
```

```text
$ codegraph impact store::Store::open --depth 2
impact of store::Store::open: 17 dependent symbol(s) in 6 file(s) within depth 2 (1 seed symbol(s))
depth 1 (direct)
  fn accuracy::index_repo  bench/src/accuracy.rs:399  via call
  method App::open_store  src/main.rs:54  via call
  fn mcp::tests::server  src/mcp.rs:566  via call
  ...
depth 2
  fn accuracy::score_repo  bench/src/accuracy.rs:416  via call
  fn run  src/main.rs:160  via call
  ...
files
     6  d1  src/mcp.rs
     3  d1  tests/incremental.rs
     2  d1  bench/src/accuracy.rs
```

(Generated with a throwaway `KIT_DATA_DIR`/`KIT_CONFIG_DIR`, never `~/.config/kit`; paths
above are shown as they'd appear from a normal `~/dev/rust/codegraph` checkout.)

**Staying fresh:** `codegraph watch` re-indexes a repo about 300 ms after its files change (see
[watch](#watch)); `codegraph mcp --watch` does it in the background, and every MCP call also
refreshes the repo it asks about.

## MCP registration

```powershell
claude mcp add codegraph -- codegraph mcp
```

## Usage

```text
codegraph index [--full] [PATH...]      index the configured roots (default ~/dev), incrementally
codegraph find <name> [--kind fn] [--repo x]
codegraph def <symbol>                  definition + source (with its doc comment)
codegraph refs <symbol> [--kind call|ref|impl|inherit]
codegraph callers <fn> [--depth N]      call hierarchy up (tree), dispatch-aware
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

In trees, `~` marks a `likely` edge and `?` marks `name_only`. `[via Trait::method]` marks a
call resolved through dynamic dispatch (an interface/trait/base-class method, not the
concrete implementation shown). `[as value]` means the function is passed as a callback or
handler, not called. `[cycle]` and `[seen above]` mark recursion and repeated subtrees.

### `watch`

`codegraph watch [PATH...]` indexes the roots once, then re-indexes a repo ~300 ms after its
files change (`--debounce-ms`, max 2 s latency), with the same filters as `index`; build
output (`target/`, `node_modules/`, `.git/`...) never triggers work, new repos are picked up.
`--json` streams NDJSON (`ready`, `reindex`, `rescan`, `error`); Ctrl-C exits 0.
`codegraph mcp --watch` does the same in the background for repos already in the graph.

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
| `codegraph_callers` / `codegraph_callees` | call hierarchy trees with call sites, dispatch-aware |
| `codegraph_outline` | nested outline of one file (works on unindexed files too) |
| `codegraph_impact` | transitive dependents of a file or symbol, by distance, plus affected files |
| `codegraph_status` | repos, sizes, resolution rates, freshness |

`hotspots`, `dot` and `path` are CLI-only (not exposed over MCP). Each MCP result has
`structuredContent` (same shape as `--json` data) and a compact text rendering. The text
uses repo-relative paths under a `# repo root` header, so it stays token-efficient. All
tools are read-only.

**Auto-refresh:** before each call, the server re-indexes the relevant repo within
`refresh_budget_ms`. That is the repo named by `repo`, else the one containing a
`file`/`target` path, else the one containing the server's working directory. A repo is
refreshed at most once every 3 s. Only files whose mtime or size changed are re-hashed, and
only files whose content hash changed are re-parsed. This is what keeps the graph fresh
today; `codegraph watch` (above) will do the same for the CLI between MCP calls.

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
   as values, and calls inside macro invocations like `assert_eq!(f(x))` or `vec![t(1)]`) and
   imports. Alongside each receiver-expression call, extraction records a **type hint**
   (`src/infer.rs`: the enclosing type, a written type, a constructor call, a field, a
   collection element, ...) so resolution isn't purely name-based. Code then computes
   nesting, containers (Go receivers, Rust `impl` types, C++ `A::b` scopes, classes), doc
   comments, signatures and exported-ness. Language builtins (`len`, `println!`, `print`,
   `string`) are dropped.
4. **Store**: each repo is written in one SQLite transaction (`repos`, `files`, `symbols`,
   `edges`, plus an FTS5 trigram index over names, qualified names and docs).
5. **Resolve**: every repo that changed is re-resolved as a whole, in memory. Import
   bindings are computed first:
   - Go: import paths map to package directories via `go.mod`.
   - Rust: crate and module paths come from the file layout and `mod`, and `use` trees are
     parsed, including `pub use` re-exports.
   - TS/JS: relative imports, `index` files, tsconfig `baseUrl`/`paths`, and monorepo
     workspace packages (`@scope/pkg` resolves to that package's own sources); default,
     named and namespace imports; `require`.
   - Python: absolute and relative module paths, `from x import y` of submodules.
   - C/C++: `#include` resolution plus header/source companions.

   Then each edge is resolved with ordered rules (`src/resolve.rs`): lexical scope and
   self/receiver calls first, then import-bound qualifiers, then **typed receivers**
   (`src/resolve_types.rs` evaluates the extraction-time hint against the whole repo — a
   type known in-repo, a type provably external, or unknown), then name-based fallback
   rules for what's left.
6. **Dispatch**: Go interfaces get implicit-satisfaction edges (method sets, including
   embedding and common stdlib interfaces, checked by arity — no explicit `implements`
   needed), and `callers`/`impact` follow interface/trait/base-class dispatch, annotating
   the call site `[via Interface::method]` when it's resolved through the abstract method
   rather than the concrete one shown.

## Accuracy

codegraph is a heuristic graph, not a type checker. [docs/ACCURACY.md](docs/ACCURACY.md)
measures how right it is against **compiler-grade ground truth**: SCIP indexes built by
each language's own semantic indexer (scip-go, rust-analyzer, scip-typescript, scip-python)
on 16 real repos (10 public: go-chi/chi, urfave/cli, labstack/echo, sharkdp/fd, tokio-rs/bytes,
serde-rs/json, sindresorhus/ky, mobx, zustand, pallets/click; 6 of the author's own tools),
scored site by site.

| language | files | callers recall | false call edges | refs recall | refs precision | impl recall / precision |
|---|---:|---:|---:|---:|---:|---:|
| Go | 364 | 98.9% | 1.1% | 98.6% | 99.4% | 87.5% / 79.5% |
| Python | 124 | 96.0% | 1.9% | 76.4% | 98.3% | 100% / 91.4% |
| TypeScript/JS | 396 | 94.3% | 1.6% | 72.9% | 98.2% | 50.0% / 28.6% |
| Rust | 195 | 85.1% | 2.7% | 64.6% | 92.7% | – |
| **all** | 1079 | 94.2% | 1.7% | 81.9% | 97.5% | 87.5% / 74.8% |

The same harness scores the v0.1.1 resolver at 78.7% callers recall with 14.0% false call edges.
Where it still falls short, honestly: Rust generic- and macro-heavy code (serde-json 71%,
bytes 82% callers recall; the author's own Rust tools are 94–97%), and TS narrowing through
exported factory objects (mobx 86%, zustand 97%). Per-repo numbers are in
[docs/ACCURACY.md](docs/ACCURACY.md).

Confidence calibration (share of judged edges that are actually correct):

| language | exact | likely | name_only |
|---|---:|---:|---:|
| Go | 100.0% | 67.7% | 49.1% |
| Python | 99.7% | 97.9% | 69.8% |
| TypeScript/JS | 98.9% | 96.1% | 87.6% |
| Rust | 94.9% | 89.4% | 7.7% |
| **all** | 98.7% (35,515 / 35,998) | 91.5% | 60.0% |

Rust has no `impl`/`inherit` score: rust-analyzer's SCIP output emits no implementation
relationships to compare against.

Reproduce with:

```powershell
task bench:accuracy
```

(This clones the pinned corpus and builds four SCIP indexers into `.cache/bench/` on first
run — expect it to take a while and to need network access; the private repos in the corpus
are skipped on machines that don't have them.)

## Agent eval

[docs/AGENT-EVAL.md](docs/AGENT-EVAL.md) runs the same 31 code questions (Go/Rust/TS/Python;
callers, callees, impact, definitions, implementations, semantic) through the same agent
loop twice — once with grep-only tools, once with the codegraph MCP tools — and measures
token spend. **Read the numbers honestly:**

| arm | pass rate | tokens total (messages) | tokens / question (median) | tokens incl. tool schemas (est.) |
|---|---|---:|---:|---:|
| grep | 31/31 (100%) | 45,226 | 1,385 | 91,866 |
| codegraph | 31/31 (100%) | 24,400 | 678 | 125,369 |

- **This run used a scripted fake LLM, not a real model.** The 100%/100% pass rate proves
  the harness and grading work, not that a real agent is equally correct on both arms — the
  fake cannot get anything wrong. A real-model run is written but not yet executed (needs
  `ANTHROPIC_API_KEY`; see the doc for the estimate-then-run commands and a proxy-enforced
  $5 cap).
- **Content tokens favor codegraph, 0.54× of grep's**, on these scripted, well-aimed tool
  paths. But the codegraph MCP server ships 10 tool schemas resent on every turn (~1.5k
  tokens) against grep's 3 (~530), so **including schema overhead the codegraph arm's raw
  total is 1.36× grep's** for this question set — prompt caching would offset most of
  that in practice, but it isn't measured here.
- Tokens reflect real tool output sizes along realistic tool-call paths (the proxy actually
  executes every call against the real `codegraph mcp` server and the corpus); they do not
  reflect how many turns, or wrong turns, a real model would take.

Reproduce with:

```powershell
task eval:agent
```

## Performance

Cold index of `~/dev` (2,634 files across every repo below it): **5.3 s** on a release
build, Windows 11, 16 cores (down from 7.0 s on v0.1.1). Full scale numbers (per-language
breakdown, no-op re-index, `--full` re-index, database size) are being written up in
`docs/SCALE.md`.

## Limitations (honest)

This is a **heuristic graph, not a type checker**. The numbers above are the actual measured
error rates — read them before trusting an edge blindly, especially `likely` and
`name_only` ones.

- **No general type checker.** Typed-receiver resolution (`src/infer.rs` +
  `src/resolve_types.rs`) covers the common patterns — locals, fields, constructors,
  parameters, loop/callback elements, `?`/`.unwrap()`/`await` unwrapping — but it is still
  a heuristic walking syntax, not a compiler. When it has no opinion, resolution falls back
  to name-based rules, which is where `name_only` and most wrong `likely` edges come from
  (calibration table above).
- **Interface/trait/base-class dispatch** points `callers`/`impact` at all known
  implementations via `[via ...]` annotations, but a resolved call still targets one
  concrete symbol (or the abstract method) per edge, not a fan-out to every implementer.
- **Rust is the weakest language: 97.5% callers recall on the corpus** (99% on the author's
  own Rust tools, 95–98% on generic- and macro-heavy libraries like serde-json and bytes).
  Names bound by `macro_rules!` expansions, trait paths with explicit receivers, closure
  parameters and `#[cfg]` twins across modules are the main misses. Rust refs recall is ~90%;
  most of the rest are `use` items and functions passed as struct-field values. False call
  edges stay low (1.4%).
- **`#[cfg]` / build-tag twins**: codegraph picks one variant of a `#[cfg]`-gated or
  build-tag-gated definition; SCIP (and the compiler) pick the host's. This shows up as
  scoring noise on gated code, not as a systematic blind spot.
- **Macro-generated code** (Rust proc macros, decorators that synthesize methods, code
  generators) is invisible beyond what tree-sitter can see in the source; calls written
  *inside* a macro invocation (`assert_eq!(f(x))`) are extracted, but a macro that
  *generates* a new function or call from nothing is not.
- **C/C++ are not in the accuracy benchmark.** They're extracted and resolved with the same
  engine and rules as everything else (imports via `#include`, header/source companions),
  but no SCIP-equivalent ground truth was scored for them — treat their confidence tiers
  with extra caution.
- **Reflection, dynamic `import()`, decorators that rewrite dispatch, and calls through
  function-typed fields** are invisible unless the receiver/qualifier is resolvable by the
  rules above. Functions passed as values are captured only when qualified (`s.handle`) or
  defined/imported in the same file.
- **Cross-repo edges are not linked**: a qualifier bound to an import outside the repo
  (stdlib, a dependency, another kit repo) resolves to nothing, by design, instead of
  guessing a same-named symbol elsewhere.
- **Rust module paths come from file layout** (`src/a/b.rs` is `a::b`); `#[path]`
  attributes and `include!` are ignored.
- **`hotspots` orphans are candidates, not verdicts**: entry points, callbacks registered
  indirectly, and exported API are excluded or invisible, so review before deleting
  anything.
- A handful of scoring artifacts remain even in the numbers above (e.g. 2 of the Go `impl`
  "false" pairs in the corpus are real cross-package implementations that scip-go itself
  doesn't record) — see `docs/OVERNIGHT.md`'s "Known remaining gaps" for the full list.

See also [docs/COMPARISON.md](docs/COMPARISON.md) for how this stacks up against grep, ctags,
LSP servers, SCIP/Sourcegraph and GitHub code search.

## Development

```powershell
cargo fmt --check ; cargo clippy --all-targets -- -D warnings ; cargo test
task snapshots        # re-record fixture snapshots, then review the diff
task bench:accuracy   # resolver accuracy vs SCIP ground truth, delta vs baseline
task eval:agent       # grep vs codegraph token/pass-rate comparison (fake LLM)
task release          # dist/codegraph-<ver>-windows-amd64.zip
```

Tests:
- `tests/fixtures/<lang>/` mini-projects are snapshotted with `insta` and asserted for the
  classic resolution traps: the same name in two packages, a method vs a function, Go
  receivers and implicit interfaces, Rust `impl` blocks and traits, TS default vs named
  exports and path aliases, and Python relative imports and inheritance.
- `tests/fixtures/{go,rust,ts}_infer/` cover typed-receiver resolution specifically
  (`tests/resolve.rs`).
- Incremental re-indexing: an edit re-parses one file; a delete drops its symbols with no
  dangling edges.
- Graph algorithms (depth, cycles, path, impact) on a known graph.
- CLI envelopes and exit codes.
- An MCP stdio session that indexes a never-seen repo on demand.
- A self-index test that finds `main`.
- `bench/` (accuracy vs SCIP) and `evals/agent/` (agent token/pass-rate eval) are their own
  harnesses with their own test suites; see docs/ACCURACY.md and docs/AGENT-EVAL.md.

## License

MIT © 2026 crowbett

# Overnight run — codegraph 1.0 (branch `overnight/1.0`)

Running log: what was done, numbers before/after, decisions and dead ends. Newest milestone
last. Started 2026-09-25 ~21:45 local.

## Needs Corbett

- Decide public vs token-only installs: while the repo is private every install path needs
  auth (`GH_TOKEN` for the scripts, SSH for `cargo install --git`); scoop/winget/brew/curl 404.
- Real agent-eval run (bar 2): needs `ANTHROPIC_API_KEY`; commands in docs/AGENT-EVAL.md.
- Delete the stray evalrig worktree (see M4): `git -C ~/dev/py/evalrig worktree remove --force
  .claude/worktrees/agent-abd2e738dc32c6eda` and `git -C ~/dev/py/evalrig branch -D
  worktree-agent-abd2e738dc32c6eda`.
- Delete the superseded remote branch `wip/sec1-extract-dos` (its work landed as 9f5bf03):
  `git push origin --delete wip/sec1-extract-dos ; git branch -D wip/sec1-extract-dos`.
- Merge `overnight/1.0` into `main` when you're happy with it (schema is now v4: the first
  `index` after upgrading rebuilds the graph).
- Cut the first multi-platform release (tag → draft release → publish), then fill the
  scoop/winget/brew hashes from its SHA256SUMS. npm publishing needs your npm login; brew needs
  a `homebrew-tap` repo.

## M1 — accuracy benchmark

**Harness**: `bench/` is a standalone crate (`codegraph-bench`, own `[workspace]`, not
shipped). `prepare` clones the pinned corpus into `.cache/bench/corpus`, installs pinned
indexers into `.cache/bench/tools`, and builds SCIP indexes; `accuracy` indexes each clone
with the codegraph library into a throwaway DB and scores it site by site
(`docs/ACCURACY.md` has the method). Tasks: `task bench:accuracy` (delta vs baseline + miss
reports in `.cache/bench/report/*.tsv`), `task bench:record` (writes `bench/results/` and the
docs table).

**Corpus**: chi, pulse, keep (Go) · fd, stash, recall (Rust) · ky, devkit (TS) · ledger
(Python). 477 files. Personal repos are cloned from their working copies at their HEAD, so
the benchmark never writes into them.

Decisions:
- SCIP over LSP for every language: all four indexers run on Windows. scip-python 0.6.6
  crashes on Windows (`new RegExp(path.sep)` with `\`); `prepare` patches the local npm copy.
  scip-go moved to `github.com/scip-code/scip-go` (the sourcegraph path no longer installs).
- Cache lives at repo-root `.cache/bench`, not `bench/.cache`: any Cargo `[workspace]` above a
  cloned Rust repo makes rust-analyzer's `cargo metadata` fail, and `workspace.exclude` did
  not help. So `bench/` has its own `[workspace]` and the root `Cargo.toml` stays untouched.
- Scoring fixes found by reading the miss reports (harness bugs, not codegraph bugs):
  TS `new C()` points at `C#<constructor>` in SCIP (folded into `C`); SCIP `local` symbols
  (nested functions) are matched by position; ledger's `src/` layout needed pyright
  `extraPaths` or every test import became a local (Python precision 72% → 99%, a GT
  artifact); rust-analyzer enum variants (`Enum#Variant#`) excluded like fields;
  rust-analyzer has no implementation relationships, so Rust impl is unscored (was a fake 0%).
- Later harness fixes (each re-baselined by running the fixed harness against 8d20102 in a
  scratch worktree, `CODEGRAPH_BENCH_CACHE` pointing at the same ground truth):
  scip-typescript escapes the constructor as `` `<constructor>`(). `` (the fold never fired),
  and object-literal method definitions (`run() {}`) were counted as call sites.
- Strict scoring: `name_only` edges count. A false edge includes resolving a stdlib receiver
  call (`wg.Add`) to an in-repo method of the same name.

**Baseline** (v0.1.1 resolver, commit 8d20102) — `bench/baseline/accuracy.json`, re-scored
after the harness fixes below (the TS/JS row was a scoring artifact at first: 91%/93%):

| language | callers recall | false call edges | refs recall | impl recall |
|---|---:|---:|---:|---:|
| Go | 97.3% | 8.8% | 97.6% | 0.0% |
| Rust | 67.5% | 13.2% | 53.9% | – |
| TypeScript/JS | 99.3% | 0.2% | 79.7% | 100% |
| Python | 99.7% | 0.7% | 78.9% | 100% |

Calibration: `exact` 99.2% correct, `likely` 86.7%, `name_only` 60.3%.

What the misses say (input for M2):
- Rust recall: 599 of 921 missed call sites have **no edge at all** — calls inside macro
  token trees (`assert_eq!(f(x), ..)`, `vec![t(..)]`, `format!("{}", id.short())`) are never
  extracted. Plus unresolved method calls on typed locals.
- Go false edges: interface calls through params (`next.ServeHTTP` with `next http.Handler`)
  and stdlib receivers (`wg.Add`, `r.Header.Get`) guessed onto in-repo methods by name;
  `chi.URLParam` resolved to the method `Context.URLParam` instead of the function.
- TS false edges: few; recall misses are mostly `Unresolved`/`Wrong` receivers.
- Python: nested `def render` shadowed by a module-level `render` (lexical scope ignored).

## M2 — resolver upgrades (done)

Every change measured with `task bench:accuracy`; kept only if it improved the numbers.

| commit | change | effect (callers recall / precision unless noted) |
|---|---|---|
| b4caff1 | Rust: calls inside macro token trees (`assert_eq!(f(x))`, `vec![t(1)]`) | Rust recall 67.5 → 84.5 |
| d672acb | lexical scoping for same-file names; Go `pkg.F` never a method | Go precision +0.2 |
| 82d9323 | **type-aware receivers** (`src/infer.rs` hints + `src/resolve_types.rs`), schema v2 | Go 97.5/91.4 → 99.8/99.3; Rust 84.6/87.9 → 89.3/93.5 |
| d1bf02a | TS/JS workspace packages (`@scope/pkg` → its sources) | TS recall 99.0 → 99.8 |
| e63efa0 | element types (loops, callbacks, if-let), Rust `pub use`, `mod` outside src/, no Rust name-only method guesses | Rust 92.4/95.0 → 94.8/98.6 |
| 49d60ef | **Go implicit interface satisfaction** edges (method sets incl. embedding + stdlib ifaces, arity check) | Go impl recall 0 → 100%, precision 94.4% |
| c92b64a | Rust receivers inside macros, `Arc::new(x)`, path qualifiers never typed | Rust recall 94.8 → 95.6 |
| e625f09 | `callers`/`impact` follow dynamic dispatch (`[via Backend::list]`) | query feature (Rust trait + Go iface + class dispatch) |
| 36c5c02 | no name guessing for value refs (`f(r.Name)`) | Go likely-tier calibration 45.8 → 75.9 |

Final (36c5c02) vs baseline (8d20102):

| language | callers recall | callers precision | false call edges | impl recall |
|---|---:|---:|---:|---:|
| Go | 97.3 → **99.9%** | 91.2 → **99.3%** | 8.8 → **0.7%** | 0 → **100%** |
| Rust | 67.5 → **95.6%** | 86.8 → **98.6%** | 13.2 → **1.4%** | – (RA has no GT) |
| TypeScript/JS | 99.3 → **99.8%** | 99.8 → **99.8%** | 0.2 → **0.2%** | 100% |
| Python | 99.7 → **100%** | 99.3 → **99.7%** | 0.7 → **0.3%** | 100% |

Calibration: `exact` 99.9% correct (12228/12237), `likely` 85.9%, `name_only` 71.6%.

Note: 82d9323's message quotes the Rust "before" numbers wrong (80.0/88.1); the real ones are
84.6/87.9 (history not rewritten).

Dead ends: excluding methods from every module-qualified lookup (only right for Go: Rust lost
4.5 pp because `repo.load_tree()` was resolving through a *module* named `repo`, which the
dot-call rule in 82d9323 then fixed properly).

Known remaining gaps: Rust refs recall 68% (type refs in qualifier position like
`Error::Locked(..)` and `Type::f` paths are not edges); `#[cfg]` / build-tag twins (codegraph
picks one variant, SCIP the host's); macro-generated items; the 2 "false" Go impl pairs are
real cross-package implementations that scip-go does not record.

## M4 — agent eval (done, fake provider only)

Built by a worktree agent, re-run by me (identical numbers). `task eval:agent`: 31 questions
(Go/Rust/TS/Python; callers, callees, impact, definitions, implementations, semantic), expected
answers verified against SCIP (24) or source (7) — never from codegraph. evalrig is single-turn,
so `evals/agent/proxy.py` is an OpenAI-compatible agent loop evalrig calls as provider
`cgagent:grep` / `cgagent:codegraph`; tools execute for real over the pinned corpus (codegraph arm
= real `codegraph mcp` + read_file/list_files, no grep).

| arm | pass | tokens (messages) | median/question | tool calls/q | tokens incl. tool schemas (est.) |
|---|---|---:|---:|---:|---:|
| grep | 31/31 | 45,226 | 1,385 | 2.2 | 91,866 |
| codegraph | 31/31 | 24,400 | 678 | 1.2 | 125,369 (was 165,167 before e4d18ce) |

Honest reading: correctness is equal **by construction** (the fake answers correctly on both
arms); the token column is real tool output along scripted paths. Content tokens favor codegraph
0.54×, but its tool schemas are resent every turn, so raw totals are 1.36× grep (prompt caching
would bill most of that at ~10%). The real-model run (correctness + real turn counts) is pending
an API key: `python evals/agent/run.py --estimate`, then `--real --budget-usd 5` (proxy-enforced
cap; estimate ≈ $0.41, ≤ $1.23 with a 3× margin).

Side effect to clean up: the agent's worktree was created inside the evalrig repo
(`~/dev/py/evalrig/.claude/worktrees/agent-abd2e738dc32c6eda`, branch
`worktree-agent-abd2e738dc32c6eda`, containing an untracked `codegraph/` clone). No evalrig file
was changed; I left it for you to delete rather than run destructive git in another repo.

## M5 — cross-platform (merged early, fee8a1e)

Done by a worktree agent in parallel with M2, verified by me: CI matrix linux/macOS-arm64/
windows green (run 36221905205, and again on the merge: fee8a1e), release workflow dry run
built 5 targets (windows-amd64 zip, linux-amd64 static musl, linux-arm64 glibc 2.34+,
macos-arm64, macos-amd64) + SHA256SUMS (run 36220857568); no release or tag created.
Packaging prepared, not published: `packaging/install.sh` / `install.ps1` (verify SHA256SUMS,
never touch PATH), npm package set (`packaging/npm`, esbuild-style per-platform packages),
Homebrew formula, `docs/INSTALL.md`. One portability fix: macOS `/var` → `/private/var`
symlinked roots (`query::repo_containing`).

## Round 2 — wider corpus, review, security (after the user's "fanout the next batch")

Four parallel streams: security audit, Rust review, docs rewrite (merged 7489c63), corpus
widening (merged 3b7c07e, +7 public repos: urfave/cli, labstack/echo, tokio-rs/bytes,
serde-rs/json, mobx, zustand, pallets/click; 3 more harness scoring fixes, see d59284a).

**The wider corpus changed the verdict.** v0.1.1 → now, callers recall / false call edges on
all 16 repos (baseline re-scored with the same harness, bench/baseline/accuracy.json):

| language | v0.1.1 | after M2 (9-repo winners) | now |
|---|---|---|---|
| Go | 92.0% / 15.9% | 98.2% / 1.1% | **98.9% / 1.1%** ✔ |
| Python | 56.2% / 5.9% | 94.4% / 2.0% | **96.0% / 1.9%** ✔ |
| Rust | 63.0% / 17.4% | 85.1% / 3.9% | **85.1% / 2.7%** ✖ recall |
| TypeScript/JS | 88.1% / 8.7% | 90.5% / 6.7% | **91.2% / 1.7%** ✖ recall |

Round-2 resolver commits: review fixes H1–H4/M1/M3 (1f03b2f), Rust trait-overload `from` and
generic-looking names (25c…), Python subscripted bases + `import a.b` binds `a` (4908f0d),
nearest-preceding same-file definition (2c92719: TS false edges 6.7 → 1.7%), type aliases
(1cc3bb6). Dead end: a `Foreign(type)` evaluation for Rust extension traits on std types
(+0.2 recall / −0.3 precision, reverted).

What still blocks Rust/TS (both need extraction changes, queued behind the SEC-1 stream that
owns extract.rs/infer.rs): Rust generic-bounded fields (`self.read.peek()` with `R: Read`),
turbofish calls inside macros (`from_str::<T>(..)` in `assert!`), serde's macro-generated
code; TS: a tree-sitter-typescript grammar bug (two call signatures in an interface without a
separator swallow the rest of the file — mobx `action.ts`), `??`/narrowed receivers, scope
spans for anonymous callbacks.

Security audit (no critical/high; cargo audit clean): SEC-3/4/9/10 fixed and merged (f44394b,
e6d9d40): repo marker files read without following links, 1 MiB cap; def/refs verify the
indexed file (hash) before printing source; paths sanitized (ESC/OSC/bidi); MCP line cap +
per-call refresh budget. SEC-1 (quadratic extraction on long method chains) and
SEC-2/5/6/7/8 (eval proxy grep secrets, actions pinning, installer token handling, ledger) are
in flight.

Perf since M2: bulk FTS/index rebuild, include index, streaming writer, parallel pass 2,
path-ordered determinism: ~/dev cold index 7.0 s (v0.1.1) → ~4.4 s.

## M3 — freshness + scale (merged a1aab64)

`codegraph watch` (notify, per-repo debounce 300 ms / max 2 s, NDJSON with `--json`, new repos
picked up, busy-retry) and `codegraph mcp --watch` landed from the M3 worktree agent (5/5
consecutive `tests/watch.rs` passes after the merge). Its scale harness (`task bench:scale`,
pins in bench/scale.toml: Linux v6.12, rust-lang/rust 1.85.0, kubernetes v1.32.0) measured the
**pre-overnight** code: Linux 60k files cold 543 s, the 102k-file combined run 1,054–1,129 s
(docs/SCALE.md). Fixed since (each from that profile): include basename index, bulk FTS +
deferred query indexes, streaming writer, parallel pass 2, deterministic ties, `#define`
constants out of substring search (schema v3), per-repo refresh counts. Linux cold on current
code (before schema v3): **224 s** (parse 57 / write tail 6 / resolve 58 / index+FTS rebuild
~100). Re-measured on 9f5bf03 in round 3 (below).

## Round 3 — day session 2026-09-26 (continuing the interrupted night)

Picked up the two ungated WIP commits, then went after the Rust recall gap.

| commit | what | verified |
|---|---|---|
| 4b3dfcc | gates b1d2b9f (turbofish calls in macro args) + fixture assertions that fail on the old query | fmt/clippy/tests |
| 9f5bf03 | **SEC-1 landed** (see below) | tests/limits.rs, bench, ~/dev graph diff |
| 64e00c1 | Rust `#[path = ".."] mod x;` modules, schema v4 | bench, tests/resolve.rs |
| b06ebb5 | recorded accuracy: Rust callers recall 85.1 → **88.3%**, precision 97.3 → 97.4% | `bench:record` |

**SEC-1.** The interrupted branch had the right idea (O(1) `Nav` links instead of
tree-sitter 0.27's O(depth) `parent()`/siblings, a query match limit, a hint time budget) but
missed the real wall: past a tree depth of 65,535 (a ~64k-link `x.b().b()…` chain, 256 KB)
tree-sitter's query cursor never finishes, match limit or not (32k links: 0.8 s; 64k: killed
after minutes). Added a pre-query depth walk: trees deeper than 4,096 are not extracted
(`FileFacts::limited`, lines/has_errors only). The WIP's own test also showed the 256 match
limit drops calls on 4k-link chains; measured on the corpus it costs nothing (accuracy JSON
byte-identical with and without the limit), so it stays and is documented as lossy only on
absurd chains. Results: 1 MB hostile chain 0.7–2.3 s release (was: never finishes); ~/dev
graph identical to the pre-change build (2,655 files / 33,455 symbols / 316,207 edges, same
confidence split); cold index of ~/dev 4.06 s vs 4.24 s (mean of 3).

**`#[path]` modules.** serde-json's lexical tests live in a test crate that pulls the library's
sources in with `#[path = "../src/lexical/mod.rs"] mod lexical;`; none of it resolved.
File-level `#[path] mod` declarations are now `mod_path` imports; `crate::` in a file outside
`src/` loaded that way means its declarer's crate (a `src/` file keeps its own crate even when
a test crate includes it — the fixture caught that). First version added 116 exact-tier
false refs: serde-json's `io` module is a `#[cfg]` twin (`#[cfg(not(feature = "std"))]
#[path = "core.rs"] mod imp;` next to `use std::io as imp;`) and the host build is std. A
`#[path]` name that is also `use`-bound in the same file, or declared twice, is now skipped.
serde-json callers recall 76.6 → 83.5%, no new wrong call edges; Rust refs recall +1.8 pp;
calibration unchanged (exact 98.6 / likely 91.4 / name_only 54.5 on the 16-repo corpus).

Dead end: TS explicit `this` parameters (`function make_(this: Annotation, ..)`, ~7 mobx
sites). The hint is easy (`pattern_names` skipped the `this` pattern), but `resolve_types`
does not look up methods declared in a type alias's object type, so mobx gained 0 and
devkit lost 1 hit. Reverted; needs type-literal members in resolve_types first.

Still open for Rust recall (bytes 79%, serde-json 83%): in-repo extension traits on std
types (`impl BufMut for Vec<u8>` → `vec.put(..)`; round 2's `Foreign(type)` attempt was
+0.2/−0.3), `json!(..)` macro return types (`data.pointer(..)`), generic-bounded fields.

## Round 4 — "speed it up" (2026-09-26 afternoon)

Scale work driven by `CODEGRAPH_TRACE=1` (new) on a persistent Linux graph. The finding
that reframed it: of Linux's 42 s resolve, resolution was ~2 s; everything else was SQLite
round trips (18 s loading 11.5 M rows back, 16–19 s of `UPDATE`s that rewrite the 1.5 GB
edges table). Details and numbers in docs/SCALE.md ("Results after the SQLite work").

| commit | what |
|---|---|
| cbb4d34 | parallel rowid-slice loads, cold loads drop per-file indexes, per-repo counts (schema v5, `status` 66x faster), partial `edges_to_file`, mimalloc, `CODEGRAPH_TRACE` |
| b221410 | cold repos resolve in memory and insert edges once (64 rows/statement), repo indexes dropped (schema v6), implicit-edge cleanup by id, parallel symbol tables |
| (this) | bench harness uses mimalloc like the CLI (it was measuring a 2x slower allocator path) |

Net, same machine: Linux cold 153–193 s → 109–129 s; 1-file re-index 32–38 s → 12–14 s;
no-op 4.2 s → 1.0 s; `status` 7.9 s → 0.12 s; 102k combined cold 249–265 s → 180 s.
Verification: accuracy bench JSON byte-identical after every step; full edge dump of ~/dev
identical between the cold (in-memory) and stored (UPDATE) paths; new tests
`cold_and_stored_paths_agree` (7 fixtures) and stored-count checks in tests/incremental.rs.

Costs: peak memory +1.2 GB on a Linux-sized cold index (held references), ~+0.8 GB from
mimalloc. Dead ends: SQLite sorter threads, `UPDATE … FROM`, mmap reads (all measured, no
gain). Deferred: a scoped re-resolve for 1-file edits (needs every file's scope and
`mentioned` set because of re-exports and return-type evaluation in the defining file, so
it would save ~35% of the 1-file path for real correctness risk); a resident resolver in
`watch` is the bigger lever.

## Round 5 — resident resolver for `watch` (2026-09-26 evening)

- `watch` and the MCP server keep the last big repo's resolver state (`ResolveCache`);
  an edit splices only the edited files in and re-resolves in memory. Linux per-edit
  re-index 12 s -> 3.5–3.7 s (numbers in docs/SCALE.md). Tests: cached == whole-repo
  resolve after every edit on 4 fixtures, stale state after another connection writes is
  not used (`PRAGMA data_version`; the test fails without the check).
- First Linux verification found 53 then 2–4 differing edges. The 53: appended symbols
  changed tie-breaks (resolution compares symbol indices), fixed by splicing in load order.
  The 2–4: the whole-repo resolver itself was non-deterministic — Python `import ksft`
  with two `ksft.py` candidates took the first hit of a `HashMap` walk, whose order is
  random per process. Sorted; a full resolve now reproduces itself.
- Mistake: stopping a test watcher with `taskkill /IM codegraph.exe` also killed the
  installed codegraph MCP server of the running Claude session; kill by PID.

## Round 6 — accuracy push, every language (2026-09-26 night)

Four read-only agents clustered every miss per language by root cause; Go, TS/JS and Rust
fixes were implemented by worktree agents, Python in the main tree, then merged here
(conflicts: both Go and TS invented a `G:` hint — TS's type-guard hint is `N:`; the TS
side added a `line` argument to eval_val/find_type, threaded through everything else).
Each agent benchmarked on a private cache (junctions to corpus/scip, own cgdb/report):
concurrent runs on the shared `.cache/bench` delete each other's graphs.

Callers recall / precision, 16-repo corpus, before -> after (platform-twin scoring):

| language | before | after | left |
|---|---|---|---|
| Go | 98.9 / 98.9 | **100.0 / 99.8** | 3 (multi-result index, anonymous interface case, a SCIP param local) |
| Python | 96.0 / 98.3 | **100.0 / 99.6** | 1 overload stub (+ none real) |
| TypeScript/JS | 94.6 / 98.8 | **99.2 / 99.5** | 28: 16 sites with two correct targets (one edge can hit one), guard unions, generic inference |
| Rust | 88.7 / 97.4 | **97.5 / 98.6** | ~150: ~53 macro_rules-bound names (needs expansion), trait paths with explicit receivers, closure params, cfg twins across modules |

Calibration: exact 99.7%, likely 96.5%. Harness changes (each re-scored the frozen
baseline at 8d20102): definition-less SCIP locals are unjudged; platform twins
(`#[cfg]`, Go build constraints, Python module-level `if`) count as correct. Cost: ~5-10%
slower indexing (16-repo corpus 2.4 -> 2.5-2.6 s; ~/dev --full 4.8-5.0 -> 5.1-5.5 s).

## Round 7 — refs recall (2026-09-29)

The harness only reported missed *call* sites; `--report` now also writes
`<repo>.refmisses.tsv` (class, outcome, site) for every other missed reference. Clustering
them: nearly all ref misses are `NoEdge` (never extracted). Rust: 2,207 of ~3,200 were a
type in path-qualifier position (`Store::open(..)`, `ErrorCode::Eof`, `N::Float(_)`),
633 `use` items, the rest struct-field fn values and macro bodies.

- `queries/rust/tags.scm`: capitalised `scoped_identifier` paths (plain, nested with a
  qualifier, and `Type::` tokens inside macro token trees) are type refs. Schema v9.
- First run: Rust `likely` calibration 94.3 -> 88.6 (60 new wrong edges). Most were
  serde-json's `de::Error` (`use serde::de::{self, ..}`) resolving to the associated type
  `type Error = Error;` in the crate's own `src/de.rs`: the lowercase "module of this
  crate" guess ran before the external `use` binding. Now a single-segment lowercase
  qualifier bound outside the repo resolves to nothing. First version also blocked longer
  paths and cost 18 serde-json calls (`de::Error::invalid_type(..)`, `ser::SerializeSeq::
  end(self)` legitimately reach the in-repo impl), so it is limited to `q == head`.

Rust, before -> after: refs recall 73.5 -> **89.8%**, refs precision 97.6 -> 98.2%, exact
99.2 -> 99.6%, likely 94.3 -> 95.3%; callers recall/precision unchanged (97.5 / 98.6). Other
languages unchanged (same tables; only the Rust fixture snapshot moved). ~/dev cold index 5.2 s (was 5.1-5.5), 380k edges.

Then, same session (each measured, kept only at equal-or-better precision/calibration):

| commit | what | effect |
|---|---|---|
| bafc36a | names in import statements are refs (`I` hint: resolve only through that file's binding, never by name) | refs recall TS 76.3 -> 91.6, Python 78.5 -> 84.2, Rust 89.8 -> 94.8 |
| 97ff2e7 | Python names used as values / in `X \| None`, `X[T]` annotations; value refs may target classes | Python refs 84.2 -> 94.8 |
| 62d7146 (harness: 1512fc0) | TS/JS names used as values (shorthand, `instanceof`, `Enum.X` objects, initializers, `typeof`, closing JSX tags) | TS refs 91.9 -> 95.5 |
| 8baed85, ea5ac0d | **same-file `macro_rules!` expansion** (src/macros.rs) | Rust defs found 94.7 -> 99.6, callers recall 97.5 -> 98.3 |

Harness fixes (baseline re-scored each time with the new `task bench:rebaseline`):
- scip-typescript records only the contextual member at a shorthand property (`{ make_ }`
  -> `Annotation.make_`), never the binding: a shorthand-shaped site resolved to the
  same-named same-file definition counts as correct.
- Implementations: SCIP lists supertypes transitively, so a pair is found through a chain
  of codegraph edges; scip-typescript has no truth for an interface's own `extends` or for
  local symbols (mobx's tests), so those edges are unjudged. TS impl 78.1/37.3 -> 100/100.
  Checked before encoding it: the first hypothesis ("scip-typescript skips class `extends`")
  was wrong; the dumped ground truth showed class extends present and interface sources and
  mobx test files absent.
- Not encoded, documented instead: scip-go misses echo's 22 cross-package value-receiver
  `MiddlewareConfigurator` implementations (but records other cross-package pairs), and
  scip-python drops some direct subclass pairs. No principled rule, so they stay false.

Macro expansion notes: the repetition separator is an unnamed regex token in
tree-sitter-rust (no node), read from the bytes between `)` and the operator; `ident!(..)`
inside another macro's arguments is plain tokens to tree-sitter, so those are found as token
triples. Guards: 64 levels of nesting, 4 expansion levels, 4096 expansions per file (shared
by all levels: a macro invoking itself eight times per level stays bounded), 256 KiB per
expansion. A duplicate-free graph dropped Rust exact edges 12037 -> 11827 (test_stream!
repeats its body three times) and callers precision 98.7 -> 98.6: the honest number.

Recorded (docs/ACCURACY.md): all languages callers 99.4% / precision 99.4%, refs 97.0% /
99.4%, defs 99.9%, exact 99.8%, likely 97.1%.

Left, by size: Rust UFCS trait calls (`ser::SerializeSeq::end(self)`, `Display::fmt(&x, f)`:
needs the trait of each impl method), extension traits on std types, impls on `&T` vs `T`;
TS object-literal properties implementing interface members and `this.prop` accessor reads;
Python attribute reads of properties; a `go/types` check of the disputed Go impl pairs.

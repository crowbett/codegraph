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
~100). Re-measurement of linux + combined on current code is running.

# Overnight run — codegraph 1.0 (branch `overnight/1.0`)

Running log: what was done, numbers before/after, decisions and dead ends. Newest milestone
last. Started 2026-09-25 ~21:45 local.

## Needs Corbett

- (none yet)

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
- Strict scoring: `name_only` edges count. A false edge includes resolving a stdlib receiver
  call (`wg.Add`) to an in-repo method of the same name.

**Baseline** (v0.1.1 resolver, commit 8d20102) — `bench/baseline/accuracy.json`:

| language | callers recall | false call edges | refs recall | impl recall |
|---|---:|---:|---:|---:|
| Go | 97.2% | 8.8% | 97.6% | 0.0% |
| Rust | 67.5% | 13.2% | 53.9% | – |
| TypeScript/JS | 91.1% | 7.2% | 75.8% | 100% |
| Python | 99.7% | 0.7% | 78.9% | 100% |

Calibration: `exact` 98.5% correct, `likely` 86.7%, `name_only` 60.3%.

What the misses say (input for M2):
- Rust recall: 599 of 921 missed call sites have **no edge at all** — calls inside macro
  token trees (`assert_eq!(f(x), ..)`, `vec![t(..)]`, `format!("{}", id.short())`) are never
  extracted. Plus unresolved method calls on typed locals.
- Go false edges: interface calls through params (`next.ServeHTTP` with `next http.Handler`)
  and stdlib receivers (`wg.Add`, `r.Header.Get`) guessed onto in-repo methods by name;
  `chi.URLParam` resolved to the method `Context.URLParam` instead of the function.
- TS false edges: few; recall misses are mostly `Unresolved`/`Wrong` receivers.
- Python: nested `def render` shadowed by a module-level `render` (lexical scope ignored).

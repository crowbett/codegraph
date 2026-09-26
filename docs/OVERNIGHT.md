# Overnight run — codegraph 1.0 (branch `overnight/1.0`)

Running log: what was done, numbers before/after, decisions and dead ends. Newest milestone
last. Started 2026-09-25 ~21:45 local.

## Needs Corbett

- Decide public vs token-only installs: while the repo is private every install path needs
  auth (`GH_TOKEN` for the scripts, SSH for `cargo install --git`); scoop/winget/brew/curl 404.
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

## M2 — resolver upgrades (in progress)

Every change measured with `task bench:accuracy`; kept only if it improved the numbers.

| commit | change | effect |
|---|---|---|
| b4caff1 | Rust: calls inside macro token trees (`assert_eq!(f(x))`, `vec![t(1)]`) | Rust recall 67.5 → 84.5 |
| d672acb | lexical scoping for same-file names; Go `pkg.F` never a method | Go precision +0.2 |
| 82d9323 | **type-aware receivers** (`src/infer.rs` hints + `src/resolve_types.rs`), schema v2 | Go precision 91.4 → 99.3, recall 99.8; Rust recall 84.6 → 89.3, precision 87.9 → 93.5 |
| d1bf02a | TS/JS workspace packages (`@scope/pkg` → its sources) | TS recall 99.0 → 99.8 |
| e63efa0 | element types (loops, callbacks, if-let), Rust `pub use`, `mod` outside src/, no Rust name-only method guesses | Rust recall 92.4 → 94.8, precision 95.0 → 98.6 |

Note: 82d9323's message quotes the Rust "before" numbers wrong (80.0/88.1); the real ones are
84.6/87.9 (history not rewritten).

Dead ends: excluding methods from every module-qualified lookup (only right for Go: Rust lost
4.5 pp because `repo.load_tree()` was resolving through a *module* named `repo`, which the
dot-call rule in 82d9323 then fixed properly).

Current (e63efa0): Go 99.9% / 99.3% · Rust 94.8% / 98.6% · TS 99.8% / 99.8% · Python 100% /
99.7% (callers recall / precision).

## M5 — cross-platform (merged early, fee8a1e)

Done by a worktree agent in parallel with M2, verified by me: CI matrix linux/macOS-arm64/
windows green (run 36221905205, and again on the merge: fee8a1e), release workflow dry run
built 5 targets (windows-amd64 zip, linux-amd64 static musl, linux-arm64 glibc 2.34+,
macos-arm64, macos-amd64) + SHA256SUMS (run 36220857568); no release or tag created.
Packaging prepared, not published: `packaging/install.sh` / `install.ps1` (verify SHA256SUMS,
never touch PATH), npm package set (`packaging/npm`, esbuild-style per-platform packages),
Homebrew formula, `docs/INSTALL.md`. One portability fix: macOS `/var` → `/private/var`
symlinked roots (`query::repo_containing`).

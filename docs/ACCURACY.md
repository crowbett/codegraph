# Accuracy

codegraph is a heuristic graph. This page measures how right it is, against compiler-grade
ground truth, on real repositories. Reproduce with `task bench:accuracy`.

## Method

**Ground truth** is a [SCIP](https://github.com/scip-code/scip) index built by each
language's semantic indexer, which resolves every reference the way the compiler does:

| language | indexer | notes |
|---|---|---|
| Go | scip-go 0.2.7 | |
| Rust | `rust-analyzer scip` (toolchain's) | emits no implementation relationships, so Rust impl edges are unscored |
| TypeScript/JS | scip-typescript 0.4.0 | `npm install` runs in the clone first so types resolve |
| Python | scip-python 0.6.6 | patched locally for a Windows `path.sep` regex crash; `src/` layout passed via `pyrightconfig.json` `extraPaths` |

**Corpus** (`bench/corpus.toml`): 9 repos pinned to commits, cloned into the gitignored
`.cache/bench/` so the originals are never touched. Three public (go-chi/chi, sharkdp/fd,
sindresorhus/ky) and six of the author's kit tools (pulse, keep, stash, recall, devkit,
ledger). The private ones are skipped on machines that don't have them.

**Scoring** is site by site (`bench/src/accuracy.rs`):

- *defs found*: SCIP definitions of functions/methods (`name().`) and types (`Name#`) in files
  codegraph indexes, matched to a codegraph symbol with the same name whose span holds the
  definition. Fields and enum variants are not codegraph symbols and are excluded.
- *callers recall*: every SCIP reference to an in-repo function/method that is syntactically a
  call (`f(`, `f::<T>(`, `f<T>(`, `F[T](`). A hit is a codegraph edge at the same
  `file:line:col` with the same name resolved to the right symbol. Resolving an interface
  method call to one of its implementations (or the reverse) counts as a *dispatch* hit.
- *callers precision*: every *resolved* codegraph call edge whose site SCIP knows is judged;
  it is a false edge when SCIP says the site points at another symbol, a local, or something
  outside the repo. *false call edges* = 1 − precision.
- *refs recall / precision*: the same over references to functions, methods and types (any
  edge kind: call, type ref, value).
- *impl*: SCIP `is_implementation` relationships between two in-repo types vs codegraph
  `impl`/`inherit` edges.
- *calibration*: precision of all judged edges per confidence tier.

Local symbols (nested functions) are matched by position, and a TS `new C()` that SCIP
attributes to `C`'s constructor counts as `C`.

## Results

`bench/baseline/accuracy.json` is the frozen baseline from before any resolver work (v0.1.1,
commit 8d20102); `bench/results/accuracy.json` is the latest recorded run.

<!-- results:start -->
codegraph `e625f09`

| language | files | defs found | callers recall | callers precision | false call edges | refs recall | refs precision | impl recall | impl precision |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| Go | 192 | 100.0% | 99.9% | 99.3% | 0.7% | 99.3% | 99.6% | 100.0% | 94.4% |
| Python | 45 | 100.0% | 100.0% | 99.7% | 0.3% | 79.8% | 99.8% | 100.0% | 100.0% |
| Rust | 90 | 92.8% | 95.6% | 98.6% | 1.4% | 68.1% | 98.8% | – | – |
| TypeScript/JS | 150 | 100.0% | 99.8% | 99.8% | 0.2% | 81.0% | 99.9% | 100.0% | 83.3% |
| **all** | 477 | 97.4% | 98.4% | 99.2% | 0.8% | 83.3% | 99.4% | 100.0% | 93.8% |

Confidence calibration (share of judged edges that are correct):

| language | exact | likely | name_only |
|---|---:|---:|---:|
| Go | 100.0% (5960/5960) | 75.9% (22/29) | 50.0% (18/36) |
| Python | 100.0% (1167/1167) | 0.0% (0/2) | 97.8% (44/45) |
| Rust | 99.8% (3674/3683) | 88.0% (249/283) | 0.0% (0/6) |
| TypeScript/JS | 100.0% (1427/1427) | 60.0% (3/5) | 100.0% (1/1) |
| all | 99.9% (12228/12237) | 85.9% (274/319) | 71.6% (63/88) |

Per repo:

| repo | lang | callers recall | callers precision | GT call sites | resolved call edges | refs recall |
|---|---|---:|---:|---:|---:|---:|
| chi | go | 100.0% | 98.7% | 1283 | 1463 | 98.8% |
| devkit | typescript | 99.7% | 99.7% | 701 | 977 | 85.8% |
| fd | rust | 97.2% | 96.9% | 796 | 903 | 64.9% |
| keep | go | 100.0% | 99.9% | 1351 | 1453 | 99.6% |
| ky | typescript | 100.0% | 100.0% | 138 | 1264 | 70.0% |
| ledger | python | 100.0% | 99.7% | 913 | 1119 | 79.8% |
| pulse | go | 99.6% | 99.5% | 1131 | 1151 | 99.3% |
| recall | rust | 93.6% | 99.3% | 715 | 693 | 71.9% |
| stash | rust | 95.8% | 99.3% | 1326 | 1301 | 67.8% |

<!-- results:end -->

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
codegraph `8d20102`

| language | files | defs found | callers recall | callers precision | false call edges | refs recall | refs precision | impl recall | impl precision |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| Go | 192 | 100.0% | 97.2% | 91.2% | 8.8% | 97.6% | 92.6% | 0.0% | – |
| Python | 45 | 100.0% | 99.7% | 99.3% | 0.7% | 78.9% | 96.1% | 100.0% | 100.0% |
| Rust | 90 | 92.8% | 67.5% | 86.8% | 13.2% | 53.9% | 90.6% | – | – |
| TypeScript/JS | 150 | 100.0% | 91.1% | 92.8% | 7.2% | 75.8% | 95.1% | 100.0% | 83.3% |
| **all** | 477 | 97.4% | 86.8% | 91.3% | 8.7% | 76.5% | 92.7% | 24.4% | 91.7% |

Confidence calibration (share of judged edges that are correct):

| language | exact | likely | name_only |
|---|---:|---:|---:|
| Go | 100.0% (4500/4501) | 80.0% (758/947) | 69.6% (641/921) |
| Python | 96.4% (1061/1101) | 97.2% (70/72) | 91.7% (66/72) |
| Rust | 98.7% (2302/2333) | 93.7% (709/757) | 29.0% (100/345) |
| TypeScript/JS | 95.0% (1305/1373) | 95.1% (39/41) | 100.0% (1/1) |
| all | 98.5% (9168/9308) | 86.7% (1576/1817) | 60.3% (808/1339) |

Per repo:

| repo | lang | callers recall | callers precision | GT call sites | resolved call edges | refs recall |
|---|---|---:|---:|---:|---:|---:|
| chi | go | 98.5% | 86.7% | 1283 | 1633 | 97.7% |
| devkit | typescript | 92.8% | 94.9% | 749 | 970 | 80.5% |
| fd | rust | 70.5% | 92.0% | 796 | 698 | 50.2% |
| keep | go | 97.6% | 93.1% | 1352 | 1517 | 98.2% |
| ky | typescript | 83.6% | 83.7% | 165 | 1266 | 65.1% |
| ledger | python | 99.7% | 99.3% | 913 | 1122 | 78.9% |
| pulse | go | 95.3% | 94.6% | 1131 | 1170 | 96.9% |
| recall | rust | 72.6% | 80.6% | 715 | 664 | 60.9% |
| stash | rust | 63.0% | 87.5% | 1326 | 973 | 52.3% |

<!-- results:end -->

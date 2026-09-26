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

**Corpus** (`bench/corpus.toml`): 16 repos pinned to commits, cloned into the gitignored
`.cache/bench/` so the originals are never touched. Ten public and six of the author's kit
tools (pulse, keep, stash, recall, devkit, ledger); the private ones are skipped on machines
that don't have them. Public repos, pinned to release-tag commits:

| repo | lang | tag | commit | files | why |
|---|---|---|---|---:|---|
| go-chi/chi | Go | – | `3d1777a` | 84 | router, middleware |
| urfave/cli | Go | v3.13.0 | `ae1fde9` | 70 | CLI framework, generic flag types |
| labstack/echo | Go | v5.3.1 | `ed8bbe4` | 102 | server lib, interfaces, generic binders |
| sharkdp/fd | Rust | – | `ce97e47` | 24 | CLI binary |
| tokio-rs/bytes | Rust | v1.12.1 | `76c0fbb` | 34 | `Buf`/`BufMut` traits, macro-forwarded impls |
| serde-rs/json | Rust | v1.0.151 | `de85007` | 71 | generics, macros, `#[cfg]` twins |
| sindresorhus/ky | TS | – | `0d59458` | 87 | small functional lib |
| mobxjs/mobx | TS | mobx@7.0.5 | `404c6e0` | 195 | npm-workspaces monorepo, classes |
| pmndrs/zustand | TS | v5.0.15 | `2115efb` | 51 | hooks, middleware generics |
| pallets/click | Python | 8.5.0 | `8b19813` | 79 | classes, decorators, `@overload` |

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

Scoring rules that keep the ground truth fair (each found by reading miss reports; each time
the v0.1.1 baseline was re-scored with the fixed harness so deltas stay comparable):

- Local symbols (nested functions) are matched by position.
- A TS `new C()` that SCIP attributes to `C`'s constructor counts as `C` (also for classes
  declared inside functions, whose constructors are SCIP locals).
- A symbol with several definition sites (TS overloads, rust-analyzer `#[cfg]` twins,
  scip-python `@overload` stubs) accepts any of them as the target.
- Object-literal method definitions (`run() {}`) that SCIP records as references to the
  interface member they implement are definition sites, not call sites.
- A site whose SCIP symbol is an unresolved import (scip-typescript can't see a workspace
  package whose types point at an unbuilt `dist/`) is left unjudged instead of counted false.
- Fields and Rust enum variants are not codegraph symbols and are excluded; rust-analyzer emits
  no implementation relationships, so Rust impl edges are unscored.

## Results

`bench/baseline/accuracy.json` is the v0.1.1 resolver (commit 8d20102) scored by the current
harness on the current corpus; `bench/results/accuracy.json` is the latest recorded run.

<!-- results:start -->
codegraph `c01b911`

| language | files | defs found | callers recall | callers precision | false call edges | refs recall | refs precision | impl recall | impl precision |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| Go | 364 | 100.0% | 98.9% | 98.9% | 1.1% | 98.6% | 99.4% | 87.5% | 79.5% |
| Python | 124 | 100.0% | 96.0% | 98.1% | 1.9% | 76.4% | 98.3% | 100.0% | 91.4% |
| Rust | 195 | 94.7% | 85.1% | 97.3% | 2.7% | 64.6% | 92.7% | – | – |
| TypeScript/JS | 396 | 99.9% | 94.3% | 98.4% | 1.6% | 72.9% | 98.2% | 50.0% | 28.6% |
| **all** | 1079 | 98.4% | 94.2% | 98.3% | 1.7% | 81.9% | 97.5% | 87.5% | 74.8% |

Confidence calibration (share of judged edges that are correct):

| language | exact | likely | name_only |
|---|---:|---:|---:|
| Go | 100.0% (18673/18673) | 67.7% (65/96) | 49.1% (82/167) |
| Python | 99.7% (4222/4236) | 97.9% (508/519) | 69.8% (132/189) |
| Rust | 94.9% (7726/8143) | 89.4% (897/1003) | 7.7% (13/168) |
| TypeScript/JS | 98.9% (4894/4946) | 96.1% (199/207) | 87.6% (276/315) |
| all | 98.7% (35515/35998) | 91.5% (1669/1825) | 60.0% (503/839) |

Per repo:

| repo | lang | callers recall | callers precision | GT call sites | resolved call edges | refs recall |
|---|---|---:|---:|---:|---:|---:|
| bytes | rust | 79.2% | 94.6% | 1277 | 1199 | 67.5% |
| chi | go | 100.0% | 98.7% | 1283 | 1463 | 98.8% |
| click | python | 94.6% | 97.5% | 2449 | 3459 | 75.4% |
| devkit | typescript | 99.7% | 99.7% | 701 | 977 | 85.8% |
| echo | go | 98.8% | 98.8% | 4021 | 4052 | 98.6% |
| fd | rust | 97.5% | 97.8% | 796 | 903 | 65.0% |
| keep | go | 100.0% | 99.9% | 1351 | 1453 | 99.6% |
| ky | typescript | 100.0% | 100.0% | 138 | 1264 | 70.0% |
| ledger | python | 100.0% | 99.7% | 913 | 1119 | 79.8% |
| mobx | typescript | 91.7% | 98.5% | 2285 | 7820 | 67.5% |
| pulse | go | 99.6% | 99.5% | 1131 | 1151 | 99.3% |
| recall | rust | 93.6% | 99.3% | 715 | 693 | 71.9% |
| serde-json | rust | 73.4% | 96.7% | 1892 | 2241 | 58.8% |
| stash | rust | 95.7% | 99.2% | 1326 | 1301 | 67.8% |
| urfave-cli | go | 97.5% | 98.3% | 2515 | 2509 | 98.0% |
| zustand | typescript | 99.3% | 94.2% | 276 | 385 | 83.9% |

<!-- results:end -->

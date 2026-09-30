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
- Platform twins: the ground truth was indexed on one Windows machine, so for code
  defined per platform or build configuration it knows only the active variant, and
  codegraph (which cannot know the build) may point at another. A target counts as correct
  when it has the wanted symbol's name and is its platform twin: Rust, same file and both
  carry `#[cfg(..)]`; Go, same package directory, different files that each carry a build
  constraint (`//go:build`, `// +build`, or a GOOS/GOARCH file-name suffix); Python,
  codegraph's definition sits in a module-level `if`/`elif`/`else` block. Added on
  2026-09-26; the baseline was re-scored with it.
- A site whose SCIP symbol is a local with no definition anywhere is SCIP's placeholder for
  a name it could not resolve (an import of a workspace package whose types point at an
  unbuilt `dist/`; a member of a module it did not type-check, like `mobx.autorun` in mobx's
  test files, which sit outside its tsconfig). Real locals always have a definition. Such
  sites are left unjudged instead of counted false (widened on 2026-09-26 from import
  bindings only; the baseline was re-scored with the same rule).
- TS/JS shorthand properties (`{ make_, extend_ }`): `{ make_ }` means `{ make_: make_ }`,
  but scip-typescript records only the contextual type's member (`Annotation.make_`) when
  the object has one, never the binding the value comes from. A target counts as correct
  when the site is shaped like a shorthand property (after `{` or `,`, before `,`, `}` or
  the end of the line) and the target is the same-named definition in the same file. Any
  other symbol SCIP records on the exact same range is also accepted. Added on 2026-09-29;
  the baseline was re-scored with it (TS refs hits +23).
- Implementations: SCIP lists every supertype of a type transitively (`Atom -> IObservable`
  through `IAtom`), codegraph stores direct `impl`/`inherit` edges, so a ground-truth pair
  is found when a chain of codegraph edges reaches it. scip-typescript records nothing for
  an interface's own `extends` or for local symbols (classes in files outside the tsconfig,
  such as mobx's tests), so codegraph edges from those are unjudged. Added on 2026-09-29;
  the baseline was re-scored with it.
- Known ground-truth gaps, still scored as false: scip-go does not record echo's
  `XConfig -> MiddlewareConfigurator` (value-receiver `ToMiddleware` in package
  `middleware`, interface in `echo`; 22 pairs), and scip-python omits some direct subclass
  pairs it records one level down (`BadParameter(UsageError)` is missing while
  `MissingParameter -> BadParameter` is present).
- Fields and Rust enum variants are not codegraph symbols and are excluded; rust-analyzer emits
  no implementation relationships, so Rust impl edges are unscored.

## Results

`bench/baseline/accuracy.json` is the v0.1.1 resolver (commit 8d20102) scored by the current
harness on the current corpus; `bench/results/accuracy.json` is the latest recorded run.

<!-- results:start -->
codegraph `ea5ac0d`

| language | files | defs found | callers recall | callers precision | false call edges | refs recall | refs precision | impl recall | impl precision |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| Go | 364 | 100.0% | 100.0% | 99.8% | 0.2% | 99.2% | 99.9% | 100.0% | 81.6% |
| Python | 124 | 100.0% | 100.0% | 99.6% | 0.4% | 94.8% | 99.6% | 100.0% | 91.4% |
| Rust | 195 | 99.6% | 98.3% | 98.6% | 1.4% | 95.7% | 98.3% | – | – |
| TypeScript/JS | 396 | 100.0% | 99.2% | 99.5% | 0.5% | 95.5% | 99.7% | 100.0% | 100.0% |
| **all** | 1079 | 99.9% | 99.4% | 99.4% | 0.6% | 97.0% | 99.4% | 100.0% | 86.2% |

Confidence calibration (share of judged edges that are correct):

| language | exact | likely | name_only |
|---|---:|---:|---:|
| Go | 100.0% (18859/18859) | 85.2% (23/27) | 76.1% (51/67) |
| Python | 99.9% (5513/5520) | 99.6% (519/521) | 78.1% (57/73) |
| Rust | 99.6% (11777/11827) | 95.4% (888/931) | 5.3% (7/131) |
| TypeScript/JS | 99.8% (6735/6748) | 100.0% (239/239) | 95.7% (264/276) |
| all | 99.8% (42884/42954) | 97.1% (1669/1718) | 69.3% (379/547) |

Per repo:

| repo | lang | callers recall | callers precision | GT call sites | resolved call edges | refs recall |
|---|---|---:|---:|---:|---:|---:|
| bytes | rust | 97.8% | 97.4% | 1277 | 1453 | 96.0% |
| chi | go | 100.0% | 99.2% | 1283 | 1457 | 98.8% |
| click | python | 100.0% | 99.5% | 2449 | 3540 | 93.6% |
| devkit | typescript | 100.0% | 100.0% | 701 | 989 | 96.5% |
| echo | go | 100.0% | 99.9% | 4021 | 4055 | 99.3% |
| fd | rust | 99.0% | 99.4% | 796 | 902 | 95.9% |
| keep | go | 100.0% | 99.9% | 1351 | 1452 | 99.6% |
| ky | typescript | 100.0% | 100.0% | 138 | 1718 | 99.4% |
| ledger | python | 100.0% | 99.9% | 913 | 1117 | 98.5% |
| mobx | typescript | 98.8% | 99.5% | 2291 | 9040 | 94.0% |
| pulse | go | 99.9% | 99.8% | 1131 | 1149 | 99.4% |
| recall | rust | 99.3% | 99.3% | 715 | 734 | 98.0% |
| serde-json | rust | 97.5% | 98.5% | 1892 | 2779 | 94.1% |
| stash | rust | 98.8% | 99.4% | 1326 | 1339 | 97.0% |
| urfave-cli | go | 100.0% | 100.0% | 2515 | 2528 | 99.1% |
| zustand | typescript | 100.0% | 98.6% | 276 | 387 | 98.8% |

<!-- results:end -->

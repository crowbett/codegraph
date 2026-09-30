# Scale — where the time goes (M3 baseline)

The 1.0 bar is "a 100k-file repo indexes in < 60 s". This page is the **baseline before the
overnight perf work**: every number below was measured on commit `4f3124c`
(`src/index.rs`, `src/store.rs`, `src/resolve.rs` as of that commit; the `watch` commits on
top do not touch the index path). Several fixes from the plan at the bottom have since
landed on `overnight/1.0`; re-measure with the command below before quoting any of this as
current.

Reproduce (release build; clones ~2.6 GB into `.cache/bench/scale/` on first run):

```powershell
task bench:scale                                   # all runs, 3 iterations each (~3 h here)
cd bench ; cargo run --release -q -- scale --run linux -n 1       # one run, one iteration
cd bench ; cargo run --release -q -- scale --run combined -n 1    # the >100k-file run
cd bench ; cargo run --release -q -- scale --prepare-only         # clone/verify pins only
```

Pins: [`bench/scale.toml`](../bench/scale.toml). Throwaway graphs: `.cache/bench/scale-db/<run>/`.
Results are merged by run name into `.cache/bench/scale.json` after every run; stdout gets a
markdown table. The peak working set is a process-lifetime peak, so run one `--run` per
invocation for per-run memory.

## Corpus

No single sensible public repo reaches 100k files in codegraph's languages once the default
`skip_dirs` apply (`vendor`, `third_party`, `testdata`, `build`, ...). The Linux kernel is the
largest and closest (60k indexable `.c`/`.h`), so:

| run | repos (pin) | files indexed |
|---|---|---:|
| `linux` | torvalds/linux `v6.12` (`adc21867`) | 60,094 |
| `linux+rust` | + rust-lang/rust `1.85.0` (`4d91de4e`, 31,296 files) — not enough on its own | 91,390 |
| `combined` | + kubernetes/kubernetes `v1.32.0` (`70d3cc98`, 10,968 files), one `index` call | **102,358** |

Windows cannot check out the kernel's `aux.c`/`aux.h` (device names); the bench keeps those 3
files out with a sparse checkout.

## Results (baseline, commit `4f3124c`)

| run | iterations | files | symbols | edges (resolved) | cold | parse / write / resolve / other | no-op | 1 file touched | peak WS | DB |
|---|---|---:|---:|---:|---:|---|---:|---:|---:|---:|
| linux | median of 3 | 60,094 | 3,011,164 | 8,458,631 (7,899,836) | **542.7 s** (499–722) | 31.4 / 273.0 / 233.9 / 3.9 s | 3.7 s | **198.8 s** (176–419) | 4,301 MB | 2,482 MB |
| linux+rust | median of 3 | 91,390 | 3,235,224 | 9,418,533 (8,390,812) | **522.1 s** (522–547) | 32.3 / 265.8 / 221.4 / 4.4 s | 4.4 s | **253.4 s** (167–309) | 4,325 MB¹ | 2,720 MB |
| combined | 2 (not a median) | 102,358 | not measured² | not measured² | **1,054 s / 1,129 s** | 35.7 / 428.0 / 584.2 s; 46.8 / 348.3 / 726.0 s | 7.6 / 7.3 s | **288 s / 362 s** | not measured² | not measured² |

Medians are per column (they may come from different iterations). ¹ process-lifetime peak,
includes the `linux` run before it. ² the combined session was stopped during its third
iteration, before the results file is written; the numbers come from the progress log.
"1 file touched" appends a comment to `kernel/sched/core.c`; it re-parses 1 file and then
re-resolves the whole Linux repo (resolve is ~98 % of it).

**Verdict: the bar is missed by ~17x** (1,054–1,129 s for 102k files vs 60 s). Parse is not the
problem (36–47 s wall on 16 threads); the single-threaded SQLite write and the resolver are.

Machine: Windows 11, 16 logical cores, 32 GB RAM, NVMe; release build (`lto`,
`codegen-units=1`). Other agents' builds and a desktop file indexer were running during all
runs (total CPU 80–95 % while the bench's own single-threaded resolve ran), so single-threaded
phases vary by up to ~2x between iterations (resolve 213–357 s on the same input). The
ranking below held in every run.

## Results after the fixes (commit `9f5bf03`, 2026-09-26)

Two iterations per run (the table's "median of 2" is the slower one); raw numbers in
`.cache/bench/scale-9f5bf03.json`. Noisy: accuracy benches and cargo builds shared the machine
during both iterations, so the fast iteration is listed too.

| run | files | symbols | edges (resolved) | cold (slow / fast) | parse / write / resolve / other (slow) | no-op | 1 file touched | peak WS | DB |
|---|---:|---:|---:|---:|---|---:|---:|---:|---:|
| linux | 60,094 | 3,011,164 | 8,458,788 (7,899,926) | **219.5 s / 152.6 s** (was 542.7) | 57.5 / 8.4 / 74.6 / 79.1 s | 4.2 s | **37.1 / 32.1 s** (was 198.8) | 5,171 MB | 2,073 MB |
| combined | 102,358 | 3,369,149 | 10,799,807 (9,255,569) | **265.0 s / 249.0 s** (was 1,054–1,129) | 83.0 / 8.3 / 96.8 / 79.9 s | 6.6 s | **34.0 / 32.8 s** (was 288–362) | 5,198 MB | 2,605 MB |

The write phase went from 273–428 s to ~8 s (bulk FTS + deferred indexes); "other" (~80 s)
is now mostly the index/FTS rebuild after the bulk load. Combined cold is **4.1–4.5x faster**
but still ~4x the 60 s bar; the 1-file touch is still dominated by a whole-repo re-resolve
(~28 s). Peak working set rose ~0.9 GB (4.3 → 5.2 GB): the streaming writer and parallel
pass 2 hold more in memory at once. Edge counts match the baseline to within 0.002% (Linux
+157 edges: SEC-1-era extraction changes, not measured separately).

## Results after the SQLite work (commit `b221410`, 2026-09-26 afternoon)

Profile first (`CODEGRAPH_TRACE=1`): resolving itself took ~2 s of Linux's 42 s resolve
phase; the rest was SQLite I/O. Loading 8.5 M edges + 3 M symbols back out took 18 s, and
writing results back as 8.3 M `UPDATE`s took 16–19 s. Any `UPDATE` rewrites the 1.5 GB
edges table: even a single `UPDATE edges SET to_symbol = NULL` takes 7–9 s. Changes, each
kept only if the graph and accuracy bench output were unchanged:

| change | effect (Linux) |
|---|---|
| parallel read-only connections over rowid slices for symbols/edges | resolve load 18 s → 7 s |
| cold loads also drop the per-file indexes (append-only inserts) | parse phase 52 s → 34 s (the single writer had been throttling the parsers) |
| cold loads resolve in memory and insert each edge once, 64 rows per statement | resolve write-back 16 s → gone; one-shot edge insert ~13 s |
| per-repo counts on `repos` (schema v5) | `status` 7.9 s → 0.12 s; index summary no longer scans edges |
| Go implicit-edge cleanup by id; context freed on a background thread; parallel symbol tables | ~4 s per re-resolve |
| `symbols_repo` / `edges_repo` dropped, `edges_to_file` partial (imports only) | fewer rows to rebuild after a cold load |
| mimalloc | ~9% faster extraction |

Rejected (measured, no gain): SQLite sorter threads for `CREATE INDEX`, `UPDATE … FROM` a
temp table, mmap reads.

Scale bench, two iterations each (slow / fast; the machine was shared, so the spread is
wide):

| run | cold | parse / write / resolve / other (fast) | no-op | 1 file touched | peak WS | DB |
|---|---:|---|---:|---:|---:|---:|
| linux | **114.1 / 129.0 s** (9f5bf03: 152.6 / 219.5) | 38.9 / 2.7 / 23.5 / 49.0 s | **1.0 s** (was 4.2) | **11.8 / 14.0 s** (was 32–37) | 8,351 MB | 1,801 MB |
| combined (102k files) | **179.6 s** / 271.8 s (was 249–265) | 54.8 / 4.3 / 43.7 / 76.7 s | 4.5 s | 31.5 s in the harness, **15.5 s** from the CLI | 8,470 MB | 2,258 MB |

The harness's combined 1-file number runs in the same process right after a cold index
(an 8 GB heap); the CLI on the same graph takes 15.5 s, resolve 12.6 s (same as Linux
alone). Peak memory: +1.2 GB from holding a cold repo's references until they are
resolved (CLI: 6.0 → 7.2 GB on Linux) and ~+0.8 GB from mimalloc vs the system heap.

What's left, per the trace: ~40–50 s rebuilding 8 indexes + the FTS index after a cold
load (SQLite `CREATE INDEX` is single-threaded and scan-bound), ~28 s of pure extraction
CPU (tree-sitter parse ~16 s), ~13 s inserting edges, and on a 1-file re-index ~7 s
loading all symbols and edges back because resolution is whole-repo. Going much further
means a scoped re-resolve (only files whose edges can change) or keeping the resolver
resident in `watch`/`mcp --watch`.

## Re-indexing an edit in `watch` / `mcp --watch` (2026-09-26 evening)

The one-file cost was a whole-repo reload (~7 s of 11.5 M rows) plus a whole-repo resolve.
Long-running callers now keep the repo's resolver state in memory (`resolve::ResolveCache`,
repos with at least 100k edges): an edit reloads only the edited files' symbols and edges,
splices them into the symbol array at their load-order position (resolution breaks ties
by symbol index, so appending changed results; the splice shifts later indices instead of
rebuilding the tables), and re-resolves in memory. Structural changes (files added or
removed, a module or package change, `--full`) and writes by any other connection
(`PRAGMA data_version`) fall back to the whole-repo path.

Linux, `codegraph watch`, one-line edits to `kernel/sched/core.c`: first edit 9.7 s (loads
the state), then **3.5–3.7 s** per edit (was ~12 s; 32–37 s this morning): walk ~0.6 s,
parse+write ~0.3 s, splice ~0.3 s, re-resolve ~2 s, write ~0.3 s. Verified with
`CODEGRAPH_VERIFY_RESIDENT=1` (a whole-repo resolve after each cached one): 0 edges differ
across comment, add-function, rename-a-widely-used-function and revert edits. Getting under
a second needs re-resolving only the affected edges; not done (return-type and inheritance
chains through the edited file make the affected set hard to bound safely).

## Profile of the cold Linux index (baseline)

Phase/sub-phase timers in a scratch copy of the crate (never committed); two runs: as-is,
and with the FTS insert trigger + 8 secondary indexes dropped during the load and rebuilt
after.

| step | as-is | no FTS trigger / no secondary indexes | rows |
|---|---:|---:|---:|
| walk + group + classify | 1.0 s | 1.0 s | 60,094 files |
| read (thread-sum) | 53 s | 39 s | 910 MB |
| extract / tree-sitter (thread-sum) | 624 s | 409 s | 59,994 files |
| **parse phase (wall, 16 threads)** | **43 s** | **28 s** | |
| `replace_file` → symbols insert | **223 s** | 10 s | 3,011,164 |
| `replace_file` → edge inserts (refs) | 45 s | 17 s | 8,458,631 |
| `replace_file` → import inserts | 2.9 s | 1.2 s | 380,343 |
| delete old + file row | 3.9 s | 0.5 s | 59,994 |
| commit | 9.8 s | 4.4 s | |
| **write phase** | **305 s** | **48 s** | |
| rebuild indexes afterwards | — | 47 s | 10 indexes |
| rebuild FTS afterwards (`'rebuild'`) | — | 38 s | 3.0 M symbols |
| resolve: load files/symbols/edges | 13 s | 13 s | 8.8 M edges into memory |
| resolve: **pass 1 (imports / includes)** | **162 s** (201–456 s in other runs) | | 380,343 includes |
| resolve: pass 2 (8.5 M edges) | 8 s | 8 s | |
| resolve: UPDATE edges one row at a time | 45 s | 12 s | 8,272,996 |
| **resolve phase** | **234 s** | (pass 1 dominates) | |
| final counts (`count(*)` x3) | 2.4 s | 2.3 s | |
| **wall** | **585 s** | 574 s (+85 s rebuilds) | |

Re-resolving the same repo with **no changes** takes 225 s (pass 1: 201 s) and still rewrites
~1,300 edges: resolution was not deterministic for ties (#7).

Shape of the Linux graph: 3.0 M symbols, of which **2.0 M are `#define` constants** (`const`),
707k functions, 135k function-like macros, 102k structs; 8.46 M non-import edges (4.9 M call,
3.3 M ref, 0.3 M value) and 380k `#include`s; DB 2.5 GB.

Not profiled: kubernetes adds only 11k Go files but +400–500 s of cold time (write +80–160 s,
resolve +360–500 s) in the combined run. Profile the Go resolve path first when re-measuring.

## Hot spots and the fix plan (ranked by measured gain)

Status notes from the integrator (2026-09-26): items marked *landed* are reported as done on
`overnight/1.0` after this baseline; not re-measured here.

1. **C/C++ include resolution is O(includes x files)** — `Ctx::c_include`: every `<system>`
   include, and every quoted include not found next to the file, scanned *all* repo paths with
   `ends_with` (380k includes x 60k paths ≈ 2.3·10¹⁰ compares), then sorted the hits. Pass 1:
   **162–456 s** of the 234–494 s resolve, paid again on every re-resolve (why touching one
   file costs minutes). *Fix*: `basename → files` map built once in `load`, `ends_with` only on
   those candidates; memoize `(dir, include) → target`. *Expected*: pass 1 → ~1 s; cold and
   one-file re-index each −160 s or more. *Landed* (basename index + memo).
2. **Symbol inserts pay for FTS5 trigram + 5 secondary indexes row by row** — 74 µs per symbol
   (223 s for 3.0 M) vs 3 µs without the `symbols_ai` trigger and indexes; edge inserts and the
   resolve UPDATEs pay the same way (45 → 17 s, 45 → 12 s). *Fix*: on bulk loads drop the insert
   trigger and query-side indexes, load, resolve, then `CREATE INDEX` (47 s) and
   `INSERT INTO symbols_fts(symbols_fts) VALUES('rebuild')` (38 s). *Expected*: write 305 → ~48 s,
   UPDATEs 45 → ~12 s, +85 s rebuilds: **≈ −200 s** cold. Follow-ups: FTS over `name` + `qname`
   only, `detail=none`; drop `*_repo` indexes if query plans allow. *Landed* (bulk-mode deferral).
3. **Parse and write were sequential, with everything held in memory** — `index_repo` collected
   every `FileFacts` before the single-threaded write; peak working set 4.3 GB for Linux.
   *Fix*: bounded channel from the rayon parsers into one SQLite writer. *Expected*: hides the
   30–45 s parse behind the write and caps memory. *Landed* (streaming parse→write).
4. **One-file re-index re-resolves the whole repo** — 199 s median for Linux (176–419 s); the
   watcher inherits this (instant on normal repos, minutes on the kernel). After #1 it is load
   (13 s) + pass 2 (8 s) + small. *Fix*: incremental resolve — edges of the changed files,
   edges whose `name` matches a symbol added/removed there, and files importing them; keep
   the resolver context resident in `watch`/MCP between batches instead of reloading 8.8 M rows.
   *Expected*: one-file re-index on Linux from minutes to < 1 s. **Not landed** (as far as
   reported) — the biggest remaining item for `watch` on huge repos.
5. **Single-row UPDATEs of resolved edges** (8.27 M statements, 45 s; 12 s without indexes, so #2
   covers most of it). Further: resolve cold edges before inserting them so each edge is written
   once, or batch through a temp table + one `UPDATE ... FROM`. Pass 2 itself is only 8 s
   (*landed*: parallel pass 2).
6. **Row volume**: 2.0 M of 3.0 M kernel symbols are object-like `#define` constants, each a
   symbol row + FTS row + index entries. Keeping macro constants out of FTS (or out of the graph
   unless referenced) removes ~⅔ of the symbol cost; edges could store interned name ids. Product
   decision, not measured.
7. **Resolution ties were not deterministic** — a no-change re-resolve rewrote ~1,300 edges
   (`c_include` ordered ties by `HashMap` iteration). *Fix*: path as the final sort key.
   *Landed* (path-ordered deterministic resolution).
8. Small: `finish` runs three `count(*)` over the edges after every run (2.4 s on Linux, also
   paid by every `watch` batch) — keep per-repo counters. The no-op re-index (walk + stat of
   60k files) is 3.7 s; 7.3–7.6 s for 102k files.

**Projection** for cold Linux after #1–#3 (from the sub-phase numbers): parse ~40 s overlapped
with write ~50 s, resolve ~25 s, index + FTS rebuild ~85 s ⇒ roughly **150–170 s** — still ~3x
the bar at 60k files. Under 60 s for 100k files needs #6 (fewer rows / smaller FTS) and
cheaper index builds (or building FTS in the background after the graph is queryable), plus
whatever the unprofiled Go resolve cost turns out to be.

## After the refs, macro and resolver work (2026-09-29)

Release build, cold `--full` index into an empty data dir, same machine:

| repo | files | symbols | edges | exact | time |
|---|---:|---:|---:|---:|---:|
| rust-lang/rust 1.85.0 | 31,294 | 228,135 | 1.28 M | 589 k | 30.8 s (1529eae: 34.8 s best of 3, 1.03 M edges) |
| Linux v6.12 | 59,994 | 3.01 M | 8.47 M | 1.58 M | 91.8 s (b221410: 114-129 s) |

More edges than before (import-statement and value references, `macro_rules!`
expansions) at no cost in time. On the way, a Python base-class walk without a visited set
turned the rust-lang/rust index into a hang (40+ min of CPU; f62ef69 bounds it), which the
accuracy corpus could not show.


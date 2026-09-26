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

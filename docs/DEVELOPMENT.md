# Development commands and rules in full

Moved verbatim from CLAUDE.md. The short invariant list is in [../CLAUDE.md](../CLAUDE.md).

## Commands

```powershell
cargo fmt --check ; cargo clippy --all-targets -- -D warnings ; cargo test
cargo build --release
task snapshots   # re-record fixture snapshots (INSTA_UPDATE=always), then review the diff
task release     # dist\codegraph-<ver>-windows-amd64.zip
```

Resolver/accuracy work — **measure before keeping a change**:

```powershell
task bench:accuracy   # score against frozen SCIP ground truth, delta vs bench/baseline/; miss
                       # reports in .cache/bench/report/*.tsv (clones the corpus + builds 4
                       # SCIP indexers on first run: network + time)
task bench:record     # once you're keeping the change: writes bench/results/ + docs/ACCURACY.md
task bench:rebaseline # after changing the *harness's* scoring: re-scores the frozen baseline
                       # (8d20102) so deltas stay comparable; document the rule in ACCURACY.md
                       # --report also writes <repo>.refmisses.tsv (non-call refs) to cluster
```

Agent eval (grep vs codegraph tool cost/correctness):

```powershell
task eval:agent        # fake LLM, no API key, no spend, rewrites docs/AGENT-EVAL.md
task eval:agent:real   # needs ANTHROPIC_API_KEY; estimate then a $5-capped real run
```

Profiling a big index: `CODEGRAPH_TRACE=1` prints phase timings (resolve load/passes/write,
index and FTS rebuild) to stderr.

Smoke against a throwaway data dir (never the real `~/.config/kit`):

```powershell
$env:KIT_DATA_DIR = "$env:TEMP\cg" ; .\target\release\codegraph.exe index $env:USERPROFILE\dev -v
```

## Rules

- Library code uses `crate::error::Error` (thiserror); only `main.rs` uses anyhow. No
  `unwrap`/`expect` outside tests (clippy `unwrap_used`/`expect_used` are deny).
- Adding a language: grammar crate in Cargo.toml, `Lang` variant + extension, a
  `queries/<lang>/tags.scm`, binding parser in `extract::parse_import`, a unit rule in
  `layout.rs`, a fixture + snapshot + resolution assertions in `tests/lang.rs`.
- Changing extraction output or the schema: bump `store::SCHEMA_VERSION` (currently **20**;
  old graphs are wiped and rebuilt on next `index`) and re-record snapshots.
- **Any resolver change (`src/resolve.rs`, `src/resolve_types.rs`, `src/infer.rs`) must be
  measured with `task bench:accuracy` before it's kept.** Compare against
  `bench/baseline/accuracy.json` and the last recorded `bench/results/accuracy.json`; a
  change that regresses recall/precision on any language, or calibration (`exact`/`likely`/
  `name_only`), gets reverted or fixed, not shipped on vibes. Read the miss reports in
  `.cache/bench/report/*.tsv` before assuming a regression is a harness bug.
- The corpus cannot show performance cliffs: after a resolver change also time a big repo
  (`codegraph index .cache/bench/scale/rust --full` into a temp `KIT_DATA_DIR`, ~31 s on
  this box; the Linux clone next to it ~92 s). A by-name walk without a visited set once
  turned the rust-lang/rust index from 31 s into a hang while every accuracy number held.
- Adding or changing a type hint: the grammar lives in `src/infer.rs`'s module doc (`S`,
  `T:`, `C:`, `M:`, `F:`, `Q:`/`A:`, `E:`, `L`, `U`, Python's `I:`/`P:`/`V:`/`D:`) — read it, keep new hints inside `MAX_DEPTH`/
  `MAX_HINT`, and add fixture coverage under `tests/fixtures/{go,rust,ts}_infer/` plus an
  assertion in `tests/resolve.rs`.
- Resolution must stay honest: when a qualifier is bound to an import that points outside
  the repo, resolve to nothing rather than guessing by name. Guesses are `name_only`. This
  also holds across repos in the same graph: cross-repo edges are never linked by name.
- MCP tools are read-only; stdout is protocol only, logs go to stderr. Keep tool output
  compact (repo-relative paths under a repo header, `file:line` everywhere).


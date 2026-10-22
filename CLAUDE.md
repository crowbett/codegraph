# codegraph

Tree-sitter symbol + call graph of every repo under the configured roots, served to agents over MCP (Rust, `codegraph.exe`). Part of **kit**: follow `KIT.md` in the workspace root (`../../KIT.md`) (config paths, `--json` envelope, exit codes, MCP rules). Do not edit KIT.md.

## Where things are
- `docs/PACKAGES.md`: what every `src/*`, `tests/*`, `bench/`, `evals/`, `packaging/` file does.
- `docs/DEVELOPMENT.md`: full command list (accuracy bench, agent eval, profiling, smoke run) and the rules with their reasoning.
- `docs/ACCURACY.md`, `AGENT-EVAL.md`, `INSTALL.md`, `SCALE.md`, `COMPARISON.md`.

## Invariants
- Library code uses `crate::error::Error` (thiserror); only `main.rs` uses anyhow. No `unwrap`/`expect` outside tests (clippy denies them). `unsafe_code = "forbid"`.
- **Any resolver change (`src/resolve.rs`, `src/resolve_types.rs`, `src/infer.rs`) must be measured with `task bench:accuracy` before it is kept.** Compare against `bench/baseline/accuracy.json` and the last `bench/results/accuracy.json`; a regression in recall/precision on any language, or in calibration (`exact`/`likely`/`name_only`), is reverted or fixed. Read `.cache/bench/report/*.tsv` before blaming the harness.
- After a resolver change also time a big repo (`codegraph index .cache/bench/scale/rust --full` into a temp `KIT_DATA_DIR`, about 31 s here); the corpus cannot show performance cliffs.
- Changing extraction output or the schema: bump `store::SCHEMA_VERSION` (currently **20**; old graphs are wiped and rebuilt) and re-record snapshots (`task snapshots`, review the diff).
- Adding a language: grammar crate, `Lang` variant + extension, `queries/<lang>/tags.scm`, binding parser in `extract::parse_import`, a rule in `layout.rs`, fixture + snapshot + assertions in `tests/lang.rs`.
- Type hints: grammar is in `src/infer.rs`'s module doc; keep new hints inside `MAX_DEPTH`/`MAX_HINT`; add fixtures under `tests/fixtures/{go,rust,ts}_infer/` and an assertion in `tests/resolve.rs`.
- Resolution stays honest: a qualifier bound to an import outside the repo resolves to nothing, not a name guess (guesses are `name_only`). Cross-repo edges are never linked by name.
- MCP tools are read-only; stdout is protocol only, logs go to stderr; keep output compact (`file:line`, repo-relative paths).
- Smoke runs use a throwaway `KIT_DATA_DIR`, never the real `~/.config/kit`.

## Commands
```powershell
cargo fmt --check ; cargo clippy --all-targets -- -D warnings ; cargo test
cargo build --release
task snapshots ; task release ; task bench:accuracy ; task bench:record ; task eval:agent
```
More (rebaseline, real agent eval, `CODEGRAPH_TRACE=1`, smoke index): `docs/DEVELOPMENT.md`.

## Definition of done
`cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` and `cargo test` pass, `cargo build --release` succeeds, and a smoke run of the release binary works: `index` of `~/dev` into a temp `KIT_DATA_DIR`, `find` / `callers` / `outline` / `impact` on real symbols, and an MCP stdio exchange.

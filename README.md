# codegraph

[![ci](https://github.com/crowbett/codegraph/actions/workflows/ci.yml/badge.svg)](https://github.com/crowbett/codegraph/actions/workflows/ci.yml)
[![release](https://img.shields.io/github/v/release/crowbett/codegraph)](https://github.com/crowbett/codegraph/releases/latest)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

**A Rust CLI + MCP server that answers "who calls this / what breaks if I change it" across
8 languages for AI agents, with every edge's accuracy measured against compiler-grade
ground truth.**

**Results:** **99.5% callers recall, 0.6% false call edges** and 99.2% refs recall vs SCIP
ground truth (scip-go, rust-analyzer, scip-typescript, scip-python) on 16 repos / 1,079 files;
`exact` edges are 99.9% correct (47,193 / 47,258)
([docs/ACCURACY.md](docs/ACCURACY.md), raw run: [bench/results/accuracy.json](bench/results/accuracy.json)).
Cold index of Linux v6.12 (59,994 files) in **91.8 s**; a one-line edit under `watch`
re-indexes it in **3.5–3.7 s** ([docs/SCALE.md](docs/SCALE.md)).

A symbol + call graph of every repo you own, built with tree-sitter, stored in SQLite, and
served to AI agents over MCP. It answers the structural questions an agent otherwise
answers with a dozen greps: *where is this defined, who calls it, what does it call, what
breaks if I change this file?*, with `file:line` on every answer, and a confidence
(`exact` / `likely` / `name_only`) on every edge so you know how much to trust it.

- **8 languages, one engine**: Go, Rust, C, C++, TypeScript, TSX, JavaScript/JSX, Python,
  via per-language tree-sitter tag queries (`queries/<lang>/tags.scm`).
- **Type-aware resolution**: receiver calls (`x.foo()`) resolve through an inferred type,
  not just a name guess; Go implicit interface satisfaction and dynamic dispatch
  (interfaces, traits, Rust `dyn`) are tracked.
- **Cross-file resolution** with a confidence on every edge, measured against
  compiler-grade ground truth (see [docs/ACCURACY.md](docs/ACCURACY.md)).
- **Incremental**: only files whose content hash changed are re-parsed (in parallel).
- **MCP server** that re-indexes the repo you're asking about before it answers.

It follows a small shared CLI contract (config paths, `--json` envelope, exit codes, MCP
conventions).

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/crowbett/codegraph/main/packaging/install.sh | sh      # Linux / macOS
irm https://raw.githubusercontent.com/crowbett/codegraph/main/packaging/install.ps1 | iex           # Windows PowerShell
cargo install --git https://github.com/crowbett/codegraph --locked codegraph                        # from source (Rust 1.85+, a C compiler)
```

The installers fetch the [latest release](https://github.com/crowbett/codegraph/releases/latest) and
verify it against `SHA256SUMS`. Other channels: [docs/INSTALL.md](docs/INSTALL.md).

## Quickstart

```sh
codegraph index ~/dev            # index every repo under the configured roots
codegraph find Store             # search
codegraph def Store::open        # definition + doc comment
codegraph callers Store::open    # who calls it
codegraph impact Store::open     # what breaks if it changes
claude mcp add codegraph -- codegraph mcp   # serve it to Claude Code over MCP
```

Also: `refs`, `callees`, `outline`, `path`, `hotspots`, `dot`, `watch`, `status`. A symbol can be a
simple name, a qualified name (`store::Store::open`) or a `file:line`. In trees, `~` marks a `likely`
edge and `?` a `name_only` one.

## Docs

- [docs/GUIDE.md](docs/GUIDE.md): walkthrough, every command and MCP tool, config, architecture, trade-offs, limitations
- [docs/ACCURACY.md](docs/ACCURACY.md): how every edge tier is measured against SCIP ground truth
- [docs/SCALE.md](docs/SCALE.md): performance on large trees
- [docs/COMPARISON.md](docs/COMPARISON.md): how it compares to other code-graph tools
- [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md): building, testing, the accuracy bench

## License

MIT © 2026 crowbett

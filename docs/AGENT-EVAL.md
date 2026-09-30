# Agent eval: grep-only vs codegraph

1.0 bar (M4): *an evalrig suite shows an agent answers code questions with fewer tokens and
more correctness using codegraph than with grep only.*

This directory-level harness (`evals/agent/`) runs the **same 31 code questions** through the
**same agent loop** twice: once with grep-style tools, once with the codegraph MCP tools, and
lets evalrig grade both arms and record their token spend. It runs end to end today against
evalrig's scripted fake LLM (no API key, no spend) and is ready for a real model run.

## How it works

```
evalrig run evals/agent/suite.yaml -m cgagent:grep -m cgagent:codegraph
   │  one chat completion per (question, arm); provider "cgagent" = openai-compatible
   ▼
evals/agent/proxy.py  (127.0.0.1 only)            ── traces ──► .cache/evals/traces/*.jsonl
   │  model = arm name; runs the tool loop (≤ 12 turns):
   │    upstream turn ─► tool calls ─► execute locally (read-only, corpus-confined) ─► repeat
   │  returns ONE completion: content = final answer, usage = SUM over all upstream turns
   ├──► upstream model: fake_llm (scripted) | Anthropic Messages API | any OpenAI-compatible gateway
   └──► `codegraph mcp` (one stdio server, graph of the corpus in a throwaway KIT_DATA_DIR)
```

evalrig is single-turn, so the agent loop lives in the proxy; because the proxy reports the summed
usage, evalrig's token and cost columns are the **total agent spend per question**.

### The two arms

| arm | tools |
|---|---|
| `grep` | `grep` (ripgrep, regex, optional repo/glob/context, ≤ 30 matches per file), `read_file` (≤ 400 lines), `list_files` |
| `codegraph` | the codegraph MCP tools exactly as `codegraph mcp` serves them (`codegraph_find/def/refs/callers/callees/outline/impact/status`, schemas forwarded from `tools/list`, results forwarded verbatim), plus `read_file` and `list_files` |

The codegraph arm deliberately has **no text search**, so the comparison is crisp: any question the
codegraph arm answers, it answers through the graph (plus reading files). Both arms see the same
system prompt apart from one sentence naming their tools, the same corpus, the same caps
(12,000 chars per tool result, marked `[truncated: ...]`) and the same turn limit.

### Questions

31 questions over the pinned accuracy corpus (`bench/corpus.toml`: chi, pulse, keep = Go; fd,
stash, recall = Rust; ky, devkit = TypeScript; ledger = Python), 9 Go / 9 Rust / 7 TS / 6 Python:

| kind | n | example |
|---|---|---|
| callers | 10 | Q07 "which functions call `originAllowed`?" |
| implementations | 6 | Q16 "which types implement recall's `Source` trait?" (a few are grep-friendly on purpose) |
| definition | 4 | Q22 "where is the helper `limitResponseSize` defined (not the `Ky` method)?" |
| callees | 4 | Q21 "what does `Ky.#fetch` call?" |
| semantic | 4 | Q25 "find the function that computes the WCAG contrast ratio" (find-by-behavior) |
| impact | 2 | Q26 "if `ledger.frame.load` changes, which functions need updating?" |
| methods | 1 | Q03 "methods of chi's `Context`" |

Every question carries its id in the prompt (`[Q07] ...`) so traces and fake rules can key on it,
and asks for a short list of names; grading is evalrig `contains` (mode `all`) over identifier
names or repo-relative paths, with `threshold: 1.0` (every expected name must appear).

**Verification.** No expected answer comes from codegraph. 24 are checked against the SCIP
ground-truth indexes built by `task bench:prepare` (`python evals/agent/truth.py check` shells
out to `codegraph-bench dump` for every corpus file, then recomputes callers/callees/definition
sites; enclosing functions come from the SCIP DEF positions). The other 7 (interface/trait/
protocol implementations, a type's methods) are read-verified in the source, with the evidence
(`file:line`) recorded in `evals/agent/cases.py` next to each case. Where a SCIP symbol cannot
express the answer (TS arrow-function consts), the lines were read and noted.

### Fake mode (tonight)

`evals/agent/fake-rules.json` scripts, per question and per arm, the tool calls a competent agent
would make with that arm's tools (codegraph: usually one `codegraph_callers`/`refs`/`callees`/
`impact` call, or `find` then one more; grep: grep the name, then read the one or two ranges that
contain the enclosing functions), followed by the final answer. The proxy **really executes**
every scripted call against the corpus and the real codegraph server, so the tool outputs, and
therefore the prompt sizes on later turns, are real.

What that does and does not show:

- **Correctness is equal by construction.** The fake cannot judge anything: both arms end with the
  verified correct answer, so the 100% / 100% pass rates only prove the plumbing and grading work.
  The correctness half of the M4 claim needs the real-model run.
- **Tokens reflect real tool output sizes** along realistic, fixed tool paths. They do not capture
  how many turns a real model would take, nor wrong turns.
- **Token counts are fake_llm's `chars / 4`** of all message contents, and fake_llm does **not**
  count tool schemas. A real model is billed for the system prompt + tool definitions on every
  turn. The codegraph arm ships 10 tool schemas (~2,100 tokens) vs the grep arm's 3 (~530), so
  the results table also shows an estimate *including* schemas. On these short, well-aimed paths
  that fixed overhead outweighs the smaller tool results; prompt caching (the proxy marks the
  system prompt and tools with `cache_control` in Anthropic mode) only helps once the prefix is
  above the model's minimum cacheable length, which the codegraph arm's ~2.4k-token prefix may not
  reach on Haiku 4.5. Trimming the MCP tool descriptions is the obvious lever if the real run
  confirms it.

## Results

<!-- results:start -->
Run `20260930-123405-71b1` · mode **fake** · upstream `fake_llm (scripted, fake-rules.json)` · 31 questions × 2 arms · 2026-09-30 05:34

| arm | pass rate | tokens total | tokens / question (median) | tool calls / question (mean) | turns / question (mean) | tool output chars (total) | tokens incl. tool schemas (est.) |
|---|---|---|---|---|---|---|---|
| grep | 31/31 (100%) | 46,521 | 1,430 | 2.2 | 2.8 | 85,901 | 94,481 |
| codegraph | 31/31 (100%) | 25,384 | 708 | 1.2 | 2.2 | 22,062 | 127,291 |

codegraph / grep total tokens: **0.55×** (tool schemas per turn, est.: grep 545, codegraph 1,521 tokens)

| Q | lang | kind | grep tokens | codegraph tokens | cg/grep | grep calls | cg calls | grep pass | cg pass |
|---|---|---|---|---|---|---|---|---|---|
| Q01 | go | callers | 2,086 | 623 | 0.30 | 3 | 1 | yes | yes |
| Q02 | go | implementations | 2,511 | 732 | 0.29 | 2 | 1 | yes | yes |
| Q03 | go | methods | 544 | 1,296 | 2.38 | 1 | 2 | yes | yes |
| Q04 | go | callers | 1,681 | 649 | 0.39 | 3 | 1 | yes | yes |
| Q05 | go | callees | 1,322 | 853 | 0.65 | 2 | 1 | yes | yes |
| Q06 | go | impact | 1,457 | 710 | 0.49 | 4 | 1 | yes | yes |
| Q07 | go | callers | 1,430 | 668 | 0.47 | 3 | 1 | yes | yes |
| Q08 | go | semantic | 2,580 | 1,786 | 0.69 | 4 | 2 | yes | yes |
| Q09 | go | definition | 497 | 600 | 1.21 | 1 | 1 | yes | yes |
| Q10 | rust | callers | 1,840 | 642 | 0.35 | 2 | 1 | yes | yes |
| Q11 | rust | definition | 492 | 598 | 1.22 | 1 | 1 | yes | yes |
| Q12 | rust | implementations | 499 | 611 | 1.22 | 1 | 1 | yes | yes |
| Q13 | rust | callers | 1,427 | 646 | 0.45 | 2 | 1 | yes | yes |
| Q14 | rust | implementations | 504 | 609 | 1.21 | 1 | 1 | yes | yes |
| Q15 | rust | callees | 1,174 | 796 | 0.68 | 2 | 1 | yes | yes |
| Q16 | rust | implementations | 543 | 707 | 1.30 | 1 | 1 | yes | yes |
| Q17 | rust | semantic | 2,228 | 1,130 | 0.51 | 3 | 2 | yes | yes |
| Q18 | rust | callers | 2,060 | 660 | 0.32 | 3 | 1 | yes | yes |
| Q19 | typescript | implementations | 582 | 777 | 1.34 | 1 | 1 | yes | yes |
| Q20 | typescript | callers | 1,846 | 623 | 0.34 | 3 | 1 | yes | yes |
| Q21 | typescript | callees | 1,368 | 885 | 0.65 | 2 | 1 | yes | yes |
| Q22 | typescript | definition | 661 | 708 | 1.07 | 1 | 1 | yes | yes |
| Q23 | typescript | callers | 1,830 | 644 | 0.35 | 2 | 1 | yes | yes |
| Q24 | typescript | callers | 1,944 | 641 | 0.33 | 2 | 1 | yes | yes |
| Q25 | typescript | semantic | 1,877 | 1,124 | 0.60 | 2 | 2 | yes | yes |
| Q26 | python | impact | 3,537 | 790 | 0.22 | 5 | 1 | yes | yes |
| Q27 | python | callers | 1,308 | 622 | 0.48 | 3 | 1 | yes | yes |
| Q28 | python | implementations | 1,263 | 977 | 0.77 | 2 | 1 | yes | yes |
| Q29 | python | callees | 1,614 | 901 | 0.56 | 2 | 1 | yes | yes |
| Q30 | python | definition | 500 | 767 | 1.53 | 1 | 1 | yes | yes |
| Q31 | python | semantic | 3,316 | 1,609 | 0.49 | 4 | 2 | yes | yes |

Median per-question token ratio codegraph/grep: **0.56** (codegraph cheaper on 22/31 questions).

| question kind | n | median cg/grep token ratio |
|---|---|---|
| callees | 4 | 0.65 |
| callers | 10 | 0.35 |
| definition | 4 | 1.21 |
| impact | 2 | 0.36 |
| implementations | 6 | 1.22 |
| methods | 1 | 2.38 |
| semantic | 4 | 0.55 |
<!-- results:end -->

## Run it

```powershell
task eval:agent                 # = python evals/agent/run.py  (fake upstream, ~1 min)
python -m unittest evals/agent/test_proxy.py -v
python evals/agent/truth.py check   # re-verify expected answers against SCIP (needs task bench:prepare)
python evals/agent/gen.py           # after editing evals/agent/cases.py: regenerate suite.yaml + fake-rules.json
```

`run.py` builds `target/release/codegraph.exe`, indexes the corpus into `.cache/evals/cg/`
(its own `KIT_CONFIG_DIR`/`KIT_DATA_DIR`), starts evalrig's `scripts/fake_llm.py` with
`fake-rules.json` and the proxy on free loopback ports, writes a throwaway evalrig config
(`.cache/evals/evalrig-config/evalrig.toml`: provider `cgagent`, `max_retries = 0`,
`timeout_s = 900`), runs `evalrig run evals/agent/suite.yaml -m cgagent:grep -m cgagent:codegraph
--no-cache` with `KIT_DATA_DIR=.cache/evals/evalrig-data`, stops everything, and rewrites the
Results block above from evalrig's results plus the proxy traces. `~/.config/kit` is never read
or written. Overrides: `CGAGENT_CORPUS`, `CGAGENT_FAKE_LLM`, `CGAGENT_CODEGRAPH`.

### Real run (claude-haiku-4-5-20251001, $5 cap)

Not run yet: no API key was available. Estimate first, then run:

```powershell
$env:ANTHROPIC_API_KEY = (vault get ANTHROPIC_API_KEY)   # or set it however you normally do
python evals/agent/run.py --estimate                     # projection from the last fake run + evalrig run --estimate (no calls)
python evals/agent/run.py --real --budget-usd 5          # = task eval:agent:real (runs the estimate first)
```

- Upstream is the native Anthropic Messages API (`https://api.anthropic.com/v1/messages`,
  `x-api-key`, `anthropic-version: 2023-06-01`), model `claude-haiku-4-5-20251001`, temperature 0,
  2,048 max tokens per turn. Any OpenAI-compatible gateway works too:
  `--upstream-kind openai --upstream-url http://127.0.0.1:8090/v1 --upstream-model <id> --key-env AIGW_API_KEY`.
- **The $5 cap is enforced by the proxy**, not by evalrig: every upstream turn is priced
  (Haiku 4.5: $1 / $5 per MTok in/out, $0.10 cache reads, 1.25x cache writes) into a ledger at
  `.cache/evals/agent/spend-real.json` that persists across runs; once it reaches the budget the
  proxy refuses further turns with HTTP 400 (non-retryable, so evalrig does not re-run the loop).
  It can overshoot by at most one turn per in-flight request (concurrency 4, well under $0.10).
  Delete the ledger file to start a fresh budget deliberately.
- `evalrig run --estimate` only sees the single-turn prompt and is a floor; `run.py --estimate`
  projects from the fake-run traces (real tool outputs + tool schemas + 200 output tokens/turn,
  ×3 safety factor). Last projection: ~$0.41 for the scripted paths, ~$1.23 with the safety factor.
- evalrig's cost column prices `cgagent:grep`/`cgagent:codegraph` at Haiku rates via `[pricing]`
  entries in the throwaway config; it bills cache writes at 1x, so the proxy ledger (and the
  `cost_usd` in each trace line) is the authoritative spend.

## Files

| path | what |
|---|---|
| `evals/agent/cases.py` | single source: questions, expected names, verification evidence, scripted tool paths per arm |
| `evals/agent/gen.py` | renders `suite.yaml` + `fake-rules.json` (`--check` for staleness, `--dry-run-tools` executes every scripted call) |
| `evals/agent/suite.yaml` | evalrig suite (generated) |
| `evals/agent/fake-rules.json` | fake_llm rules (generated): `(?s)\[Qnn\](?:.*?\[tool \w+\]){k}` selects turn k; most-advanced rule first |
| `evals/agent/proxy.py` | the agent proxy (stdlib only) |
| `evals/agent/run.py` | one-command runner + results writer + estimate |
| `evals/agent/truth.py` | SCIP ground-truth queries and `check` |
| `evals/agent/test_proxy.py` | self-tests: confinement, caps, loop, usage sums, budget, Anthropic wire format, loopback guard |

Sandbox rules (KIT.md): every path argument is normalized, each component is `lstat`ed
(symlinks and junctions refused), the realpath must stay under the corpus root; UNC/NUL paths,
`.git/` internals and credential-looking files (`.env*`, keys, `id_*`, keystores, `.netrc`, ...)
are refused and excluded from grep/list; ripgrep runs with `--no-config`, the pattern is passed
via `--regexp` and globs as `--glob=...` (no flag injection); codegraph path arguments
(`file`/`target`/`symbol`) must stay inside the corpus. The HTTP server binds 127.0.0.1, accepts
only loopback `Host` values and refuses browser requests (`Origin` / cross-site `Sec-Fetch-Site`).
The only ordering marker added to tool results is a `[tool NAME]` line (a few tokens, same for
both arms), which the fake rules key on.

"""One-command agent eval: build, index, start the fake (or a real upstream) and the
agent proxy, run the evalrig suite for both arms, and write docs/AGENT-EVAL.md.

    python evals/agent/run.py                       # fake upstream (no API key, no spend)
    python evals/agent/run.py --estimate --real     # projected real-run cost, no calls
    python evals/agent/run.py --real --budget-usd 5 # claude-haiku-4-5-20251001 via Anthropic

Everything lives under .cache/evals/ (gitignored): the codegraph graph (its own
KIT_CONFIG_DIR/KIT_DATA_DIR), evalrig's config + SQLite (KIT_CONFIG_DIR/KIT_DATA_DIR),
proxy traces and the spend ledger. ~/.config/kit is never touched. Stdlib only.
"""

from __future__ import annotations

import argparse
import contextlib
import json
import os
import shutil
import socket
import statistics
import subprocess
import sys
import time
import urllib.request
from pathlib import Path
from typing import Any

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
sys.path.insert(0, str(HERE))

from cases import CASES  # noqa: E402

STATE = ROOT / ".cache" / "evals"
DOC = ROOT / "docs" / "AGENT-EVAL.md"
SUITE = HERE / "suite.yaml"
RULES = HERE / "fake-rules.json"
DEFAULT_CORPUS = Path(os.environ.get("CGAGENT_CORPUS", str(ROOT / ".cache" / "bench" / "corpus")))
# Default assumes an evalrig checkout at <workdir>/py/evalrig next to <workdir>/rust/codegraph.
DEFAULT_FAKE = Path(
    os.environ.get("CGAGENT_FAKE_LLM", str(ROOT.parent.parent / "py" / "evalrig" / "scripts" / "fake_llm.py"))
)
REAL_MODEL = "claude-haiku-4-5-20251001"
HAIKU = {"input": 1.0, "output": 5.0, "cache_read": 0.1}
START, END = "<!-- results:start -->", "<!-- results:end -->"

Json = dict[str, Any]


def log(msg: str) -> None:
    print(f"[eval:agent] {msg}", flush=True)


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return int(s.getsockname()[1])


def wait_http(url: str, proc: subprocess.Popen[bytes], what: str, timeout_s: float = 120) -> None:
    deadline = time.monotonic() + timeout_s
    while time.monotonic() < deadline:
        if proc.poll() is not None:
            raise SystemExit(f"{what} exited early (code {proc.returncode}); see its log")
        try:
            with urllib.request.urlopen(url, timeout=2):  # noqa: S310 - loopback
                return
        except OSError:
            time.sleep(0.3)
    raise SystemExit(f"{what} did not come up at {url}")


def quiet_env(**extra: str) -> dict[str, str]:
    env = dict(os.environ)
    env.update({"NO_COLOR": "1", "CLICOLOR_FORCE": "0", "PYTHONIOENCODING": "utf-8"})
    env.update(extra)
    return env


def spawn(cmd: list[str], env: dict[str, str], logfile: Path) -> subprocess.Popen[bytes]:
    logfile.parent.mkdir(parents=True, exist_ok=True)
    fh = open(logfile, "wb")  # noqa: SIM115 - closed with the process
    return subprocess.Popen(
        cmd,
        env=env,
        stdout=fh,
        stderr=subprocess.STDOUT,
        cwd=ROOT,
        creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0),
    )


def stop(proc: subprocess.Popen[bytes] | None) -> None:
    if proc is None or proc.poll() is not None:
        return
    proc.terminate()
    try:
        proc.wait(timeout=10)
    except subprocess.TimeoutExpired:
        proc.kill()


def evalrig_config(cfg_dir: Path, proxy_url: str, prices: dict[str, float]) -> None:
    cfg_dir.mkdir(parents=True, exist_ok=True)
    price = f"input = {prices['input']}\noutput = {prices['output']}\ncache_read = {prices['cache_read']}\n"
    (cfg_dir / "evalrig.toml").write_text(
        "# written by evals/agent/run.py - throwaway config for the agent eval\n"
        "[run]\nconcurrency = 4\ntimeout_s = 900\nmax_retries = 0\n\n"
        '[providers.cgagent]\ntype = "openai-compatible"\n'
        f'base_url = "{proxy_url}"\nmodels = ["grep", "codegraph"]\nmax_concurrency = 4\n\n'
        f"[pricing.grep]\n{price}\n[pricing.codegraph]\n{price}",
        encoding="utf-8",
    )


def evalrig(args: list[str], env: dict[str, str]) -> tuple[int, Json]:
    exe = shutil.which("evalrig")
    if not exe:
        raise SystemExit("evalrig not on PATH (uv tool install evalrig)")
    res = subprocess.run(
        [exe, "--json", *args], env=env, cwd=ROOT, capture_output=True, text=True, encoding="utf-8", errors="replace"
    )
    try:
        doc: Json = json.loads(res.stdout)
    except json.JSONDecodeError:
        sys.stderr.write(res.stdout[-2000:] + res.stderr[-2000:])
        raise SystemExit(f"evalrig {' '.join(args)} printed no JSON (exit {res.returncode})") from None
    return res.returncode, doc


# --------------------------------------------------------------------------- summary


def load_traces(path: Path) -> list[Json]:
    if not path.exists():
        return []
    return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines() if line]


def schema_tokens() -> dict[str, int]:
    """System prompt + tool definitions per arm (chars/4), as the proxy served them.
    fake_llm does not count tool schemas in prompt_tokens; a real model is billed for
    them on every turn (minus prompt-cache hits)."""
    path = STATE / "cg" / "arms.json"
    if not path.exists():
        return {"grep": 0, "codegraph": 0}
    arms = json.loads(path.read_text(encoding="utf-8"))
    return {a: (len(v["system"]) + len(json.dumps(v["tools"]))) // 4 for a, v in arms.items()}


def summarize(results: list[Json], traces: list[Json], mode: str, run_id: str, upstream: str) -> str:
    last: dict[tuple[str, str], Json] = {}
    for t in traces:  # last trace per (arm, case) wins
        if t.get("case"):
            last[(t["arm"], t["case"])] = t
    passed: dict[tuple[str, str], bool] = {}
    for r in results:
        arm = str(r.get("model", "")).split(":")[-1]
        passed[(arm, str(r.get("case_id")))] = bool(r.get("passed"))
    schema = schema_tokens()
    arms = ("grep", "codegraph")
    lines = []
    lines.append(
        f"Run `{run_id}` · mode **{mode}** · upstream `{upstream}` · "
        f"{len(CASES)} questions × 2 arms · {time.strftime('%Y-%m-%d %H:%M')}\n"
    )
    lines.append(
        "| arm | pass rate | tokens total | tokens / question (median) | "
        "tool calls / question (mean) | turns / question (mean) | tool output chars (total) | "
        "tokens incl. tool schemas (est.) |"
    )
    lines.append("|---|---|---|---|---|---|---|---|")
    totals: dict[str, int] = {}
    for arm in arms:
        ts = [last[(arm, c["id"])] for c in CASES if (arm, c["id"]) in last]
        toks = [t["usage"]["input"] + t["usage"]["output"] for t in ts]
        calls = [len(t["tool_calls"]) for t in ts]
        turns = [t["turns"] for t in ts]
        chars = sum(tc["result_chars"] for t in ts for tc in t["tool_calls"])
        adj = sum(tok + t["turns"] * schema[arm] for tok, t in zip(toks, ts, strict=True))
        npass = sum(1 for c in CASES if passed.get((arm, c["id"])))
        totals[arm] = sum(toks)
        lines.append(
            f"| {arm} | {npass}/{len(CASES)} ({100 * npass / len(CASES):.0f}%) | {sum(toks):,} | "
            f"{statistics.median(toks) if toks else 0:,.0f} | "
            f"{statistics.mean(calls) if calls else 0:.1f} | {statistics.mean(turns) if turns else 0:.1f} | "
            f"{chars:,} | {adj:,} |"
        )
    if totals.get("grep"):
        lines.append(
            f"\ncodegraph / grep total tokens: **{totals['codegraph'] / totals['grep']:.2f}×** "
            f"(tool schemas per turn, est.: grep {schema['grep']:,}, codegraph {schema['codegraph']:,} tokens)\n"
        )
    lines.append(
        "| Q | lang | kind | grep tokens | codegraph tokens | cg/grep | grep calls | cg calls | grep pass | cg pass |"
    )
    lines.append("|---|---|---|---|---|---|---|---|---|---|")
    ratios = []
    for c in CASES:
        g, k = last.get(("grep", c["id"])), last.get(("codegraph", c["id"]))
        gt = g["usage"]["input"] + g["usage"]["output"] if g else 0
        kt = k["usage"]["input"] + k["usage"]["output"] if k else 0
        ratio = kt / gt if gt else 0
        if gt and kt:
            ratios.append(ratio)
        mark = {True: "yes", False: "**no**"}
        lines.append(
            f"| {c['id']} | {c['lang']} | {c['kind']} | {gt:,} | {kt:,} | {ratio:.2f} | "
            f"{len(g['tool_calls']) if g else '-'} | {len(k['tool_calls']) if k else '-'} | "
            f"{mark[passed.get(('grep', c['id']), False)]} | {mark[passed.get(('codegraph', c['id']), False)]} |"
        )
    if ratios:
        lines.append(
            f"\nMedian per-question token ratio codegraph/grep: **{statistics.median(ratios):.2f}** "
            f"(codegraph cheaper on {sum(r < 1 for r in ratios)}/{len(ratios)} questions)."
        )
    by_kind: dict[str, list[float]] = {}
    for c in CASES:
        g, k = last.get(("grep", c["id"])), last.get(("codegraph", c["id"]))
        if g and k:
            gt = g["usage"]["input"] + g["usage"]["output"]
            kt = k["usage"]["input"] + k["usage"]["output"]
            by_kind.setdefault(c["kind"], []).append(kt / gt)
    lines.append("\n| question kind | n | median cg/grep token ratio |")
    lines.append("|---|---|---|")
    for kind, rs in sorted(by_kind.items()):
        lines.append(f"| {kind} | {len(rs)} | {statistics.median(rs):.2f} |")
    return "\n".join(lines) + "\n"


def write_doc(block: str) -> None:
    text = DOC.read_text(encoding="utf-8") if DOC.exists() else f"# Agent eval\n\n{START}\n{END}\n"
    if START not in text or END not in text:
        text += f"\n{START}\n{END}\n"
    head, rest = text.split(START, 1)
    _, tail = rest.split(END, 1)
    DOC.write_text(f"{head}{START}\n{block}{END}{tail}", encoding="utf-8", newline="\n")


def estimate(real_prices: dict[str, float]) -> int:
    """Project real-run cost from the latest fake-run traces (tokens there are chars/4
    of real tool outputs) plus tool schemas, with a safety factor for longer real loops."""
    traces_dir = STATE / "traces"
    files = sorted(traces_dir.glob("fake-*.jsonl")) if traces_dir.exists() else []
    if not files:
        log("no fake-run traces yet: run `python evals/agent/run.py` first")
        return 1
    traces = load_traces(files[-1])
    schema = schema_tokens()
    total_in = total_out = 0
    for t in traces:
        total_in += t["usage"]["input"] + t["turns"] * schema[t["arm"]]
        total_out += t["turns"] * 200  # a real model writes tool-call JSON + some prose per turn
    base = (total_in * real_prices["input"] + total_out * real_prices["output"]) / 1e6
    factor = 3.0
    log(
        f"fake run {files[-1].name}: {len(traces)} agent runs, {total_in:,} input tokens incl. tool "
        f"schemas, ~{total_out:,} output tokens assumed (200/turn)"
    )
    log(
        f"projected {REAL_MODEL} cost: ${base:.3f} for the scripted paths; "
        f"x{factor:.0f} safety factor for real, longer loops -> ${base * factor:.2f} "
        "(the proxy's --budget-usd ledger is the hard cap)"
    )
    # evalrig's own estimate only sees the single-turn prompt (it cannot know the agent
    # loop), so it is a floor, not a projection; shown for completeness.
    ev_env = quiet_env(KIT_CONFIG_DIR=str(STATE / "evalrig-config"), KIT_DATA_DIR=str(STATE / "evalrig-data"))
    evalrig_config(STATE / "evalrig-config", "http://127.0.0.1:9/v1", real_prices)
    code, doc = evalrig(["run", str(SUITE), "-m", "cgagent:grep", "-m", "cgagent:codegraph", "--estimate"], ev_env)
    if doc.get("ok"):
        for model, m in (doc["data"].get("models") or {}).items():
            log(
                f"evalrig --estimate {model}: {m.get('input_tokens', '?')} prompt tokens, "
                f"max ${m.get('max_cost_usd', '?')} (single turn only - a floor)"
            )
    else:
        log(f"evalrig --estimate failed: {doc.get('error')}")
    return 0


# --------------------------------------------------------------------------- main


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--corpus", type=Path, default=DEFAULT_CORPUS)
    ap.add_argument("--no-build", action="store_true", help="skip cargo build --release")
    ap.add_argument(
        "--codegraph",
        type=Path,
        default=Path(os.environ.get("CGAGENT_CODEGRAPH", ROOT / "target" / "release" / "codegraph.exe")),
    )
    ap.add_argument("--fake-llm", type=Path, default=DEFAULT_FAKE)
    ap.add_argument(
        "--real", action="store_true", help=f"real upstream (default: Anthropic Messages API, {REAL_MODEL})"
    )
    ap.add_argument("--upstream-url", default="https://api.anthropic.com/v1")
    ap.add_argument("--upstream-kind", choices=["anthropic", "openai"], default="anthropic")
    ap.add_argument("--upstream-model", default=REAL_MODEL)
    ap.add_argument("--key-env", default="ANTHROPIC_API_KEY")
    ap.add_argument("--budget-usd", type=float, default=5.0)
    ap.add_argument("--estimate", action="store_true", help="print projected real cost and exit")
    ap.add_argument("--no-doc", action="store_true", help="do not rewrite docs/AGENT-EVAL.md")
    args = ap.parse_args()

    prices = HAIKU if args.real else {"input": 0.0, "output": 0.0, "cache_read": 0.0}
    if args.estimate:
        return estimate(HAIKU)

    if subprocess.run([sys.executable, str(HERE / "gen.py"), "--check"], cwd=ROOT).returncode:
        return 1
    if args.real and not os.environ.get(args.key_env):
        log(f"--real needs ${args.key_env} set (the key never leaves the proxy's upstream calls)")
        return 4
    if not args.no_build:
        log("cargo build --release")
        subprocess.run(["cargo", "build", "--release", "--quiet"], cwd=ROOT, check=True)
    if not args.codegraph.is_file():
        log(f"codegraph binary missing: {args.codegraph}")
        return 1
    if not args.corpus.is_dir():
        log(f"corpus missing: {args.corpus} (task bench:prepare)")
        return 1

    stamp = time.strftime("%Y%m%d-%H%M%S")
    mode = "real" if args.real else "fake"
    cg_state = STATE / "cg"
    shutil.rmtree(cg_state / "kit-data", ignore_errors=True)  # fresh graph every run
    cg_env = quiet_env(KIT_CONFIG_DIR=str(cg_state / "kit-config"), KIT_DATA_DIR=str(cg_state / "kit-data"))
    for d in ("kit-config", "kit-data"):
        (cg_state / d).mkdir(parents=True, exist_ok=True)
    log(f"codegraph index {args.corpus}")
    subprocess.run([str(args.codegraph), "index", str(args.corpus)], env=cg_env, check=True, cwd=ROOT)

    fake = proxy_proc = None
    trace = STATE / "traces" / f"{mode}-{stamp}.jsonl"
    try:
        if args.real:
            upstream_env = {
                "CGAGENT_UPSTREAM_URL": args.upstream_url,
                "CGAGENT_UPSTREAM_KIND": args.upstream_kind,
                "CGAGENT_UPSTREAM_MODEL": args.upstream_model,
                "CGAGENT_UPSTREAM_KEY_ENV": args.key_env,
                "CGAGENT_BUDGET_USD": str(args.budget_usd),
                "CGAGENT_LEDGER": str(STATE / "agent" / "spend-real.json"),
                "CGAGENT_PRICE_IN": str(HAIKU["input"]),
                "CGAGENT_PRICE_OUT": str(HAIKU["output"]),
                "CGAGENT_PRICE_CACHE_READ": str(HAIKU["cache_read"]),
            }
            upstream = f"{args.upstream_kind}:{args.upstream_model}"
        else:
            if not args.fake_llm.is_file():
                log(f"fake_llm.py not found: {args.fake_llm}")
                return 1
            fport = free_port()
            fake = spawn(
                [sys.executable, str(args.fake_llm), "--port", str(fport), "--responses", str(RULES)],
                quiet_env(),
                STATE / "logs" / "fake_llm.log",
            )
            wait_http(f"http://127.0.0.1:{fport}/v1/models", fake, "fake_llm")
            upstream_env = {
                "CGAGENT_UPSTREAM_URL": f"http://127.0.0.1:{fport}/v1",
                "CGAGENT_UPSTREAM_KIND": "openai",
                "CGAGENT_UPSTREAM_MODEL": "fake-{arm}",
                "CGAGENT_LEDGER": str(STATE / "agent" / f"spend-fake-{stamp}.json"),
                "CGAGENT_PRICE_IN": "0",
                "CGAGENT_PRICE_OUT": "0",
                "CGAGENT_PRICE_CACHE_READ": "0",
            }
            upstream = "fake_llm (scripted, fake-rules.json)"
        pport = free_port()
        penv = quiet_env(**upstream_env)
        proxy_proc = spawn(
            [
                sys.executable,
                str(HERE / "proxy.py"),
                "--port",
                str(pport),
                "--corpus",
                str(args.corpus),
                "--codegraph",
                str(args.codegraph),
                "--state-dir",
                str(cg_state),
                "--trace",
                str(trace),
            ],
            penv,
            STATE / "logs" / "proxy.log",
        )
        wait_http(f"http://127.0.0.1:{pport}/v1/models", proxy_proc, "proxy")

        ev_env = quiet_env(KIT_CONFIG_DIR=str(STATE / "evalrig-config"), KIT_DATA_DIR=str(STATE / "evalrig-data"))
        evalrig_config(STATE / "evalrig-config", f"http://127.0.0.1:{pport}/v1", prices)
        log(f"evalrig run ({mode}) -> trace {trace.relative_to(ROOT)}")
        code, doc = evalrig(
            [
                "run",
                str(SUITE),
                "-m",
                "cgagent:grep",
                "-m",
                "cgagent:codegraph",
                "--no-cache",
                "--tag",
                f"agent-{mode}-{stamp}",
            ],
            ev_env,
        )
        if not doc.get("ok"):
            log(f"evalrig run failed: {doc.get('error')}")
            return code or 1
        run = doc["data"]
        run_id = str(run.get("id") or run.get("run", {}).get("id") or "latest")
        results = fetch_results(run, run_id, ev_env)
    finally:
        stop(proxy_proc)
        stop(fake)

    traces = load_traces(trace)
    block = summarize(results, traces, mode, run_id, upstream)
    print(block)
    if not args.no_doc:
        write_doc(block)
        log(f"wrote {DOC.relative_to(ROOT)}")
    return 0


def fetch_results(run: Json, run_id: str, env: dict[str, str]) -> list[Json]:
    """Per-case results for the run: from the run document if present, else the SQLite store."""
    for key in ("results", "cases"):
        if isinstance(run.get(key), list) and run[key] and "case_id" in run[key][0]:
            return list(run[key])
    import sqlite3

    db = STATE / "evalrig-data" / "evalrig" / "evalrig.db"
    con = sqlite3.connect(db)
    con.row_factory = sqlite3.Row
    try:
        cols = {r["name"] for r in con.execute("PRAGMA table_info(results)")}
        rid = run_id
        if rid == "latest":
            rid = con.execute("SELECT id FROM runs ORDER BY started_at DESC LIMIT 1").fetchone()["id"]
        rows = con.execute("SELECT * FROM results WHERE run_id = ?", (rid,)).fetchall()
        out = []
        for r in rows:
            d = dict(r)
            if "data" in cols and isinstance(d.get("data"), str):
                with contextlib.suppress(json.JSONDecodeError):
                    d.update(json.loads(d["data"]))
            out.append(d)
        return out
    finally:
        con.close()


if __name__ == "__main__":
    sys.exit(main())

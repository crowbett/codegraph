"""Ground-truth helper for the agent-eval suite: query the SCIP indexes built by
`task bench:prepare` (never codegraph's own graph).

It shells out to `codegraph-bench dump <repo> <file>` for every source file of a
corpus repo, caches the occurrences as JSON under .cache/evals/scip/, and answers:

    python evals/agent/truth.py defs    <repo> <name>           # DEF sites whose symbol ends in name
    python evals/agent/truth.py refs    <repo> <symbol-suffix>  # every ref of matching symbols
    python evals/agent/truth.py callers <repo> <symbol-suffix>  # enclosing function per ref
    python evals/agent/truth.py callees <repo> <file> <start> <end>  # function refs in a line range
    python evals/agent/truth.py check                           # re-verify cases.py truths

`callers` takes the nearest preceding function/method DEF in the same file as the
enclosing symbol. That is sound for top-level Go/Rust/Python code; closures, nested
functions and TS arrow consts need a human look, so every suite answer was also read
in the source (see docs/AGENT-EVAL.md). Stdlib only.

Env overrides: CGAGENT_CORPUS (corpus dir), CGAGENT_BENCH_BIN (codegraph-bench.exe),
CGAGENT_BENCH_CWD (dir the bench binary runs in, where .cache/bench/scip lives).
"""

from __future__ import annotations

import json
import os
import re
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO_ROOT = HERE.parent.parent
MAIN = Path(os.environ.get("CGAGENT_BENCH_CWD", str(REPO_ROOT)))
CORPUS = Path(os.environ.get("CGAGENT_CORPUS", str(MAIN / ".cache" / "bench" / "corpus")))
BENCH = Path(os.environ.get("CGAGENT_BENCH_BIN", str(MAIN / "bench" / "target" / "release" / "codegraph-bench.exe")))
CACHE = REPO_ROOT / ".cache" / "evals" / "scip"
EXTS = {".go", ".rs", ".ts", ".tsx", ".py"}
LINE = re.compile(r"^(\d+):(\d+)-(\d+) (DEF|ref) (.*)$")

Occ = dict[str, object]


def _env() -> dict[str, str]:
    return {**os.environ, "NO_COLOR": "1", "CLICOLOR_FORCE": "0"}


def source_files(repo: str) -> list[str]:
    out = subprocess.run(
        ["rg", "--no-config", "--files"],
        cwd=CORPUS / repo,
        capture_output=True,
        text=True,
        check=True,
        env=_env(),
    ).stdout
    files = [f.replace("\\", "/") for f in out.splitlines()]
    return sorted(f for f in files if Path(f).suffix in EXTS and "node_modules/" not in f)


def occurrences(repo: str) -> list[Occ]:
    cache = CACHE / f"{repo}.json"
    if cache.exists():
        data: list[Occ] = json.loads(cache.read_text(encoding="utf-8"))
        return data
    occ: list[Occ] = []
    for f in source_files(repo):
        res = subprocess.run(
            [str(BENCH), "dump", repo, f],
            cwd=MAIN,
            capture_output=True,
            text=True,
            encoding="utf-8",
            errors="replace",
            env=_env(),
        )
        for line in res.stdout.splitlines():
            m = LINE.match(line)
            if not m or m.group(5).startswith("local "):
                continue
            occ.append({"file": f, "line": int(m.group(1)), "role": m.group(4), "sym": m.group(5)})
    CACHE.mkdir(parents=True, exist_ok=True)
    cache.write_text(json.dumps(occ), encoding="utf-8")
    return occ


def is_fn(sym: str) -> bool:
    return sym.endswith(").")


def short(sym: str) -> str:
    return sym.split(" ")[-1]


def leaf(sym: str) -> str:
    """`pkg`/Type#Method(). -> Method ; `mod`/func(). -> func"""
    s = short(sym).rstrip(".").rstrip(")").rstrip("(").rstrip("`")
    return re.split(r"[#./`:\[\]]", s)[-1]


def enclosing(occ: list[Occ], file: str, line: int) -> str | None:
    best: tuple[int, str] | None = None
    for o in occ:
        if o["file"] != file or o["role"] != "DEF" or not is_fn(str(o["sym"])):
            continue
        ln = int(str(o["line"]))
        if ln <= line and (best is None or ln >= best[0]):
            best = (ln, str(o["sym"]))
    return best[1] if best else None


def matching(repo: str, suffix: str) -> set[str]:
    return {str(o["sym"]) for o in occurrences(repo) if o["role"] == "DEF" and str(o["sym"]).endswith(suffix)}


def callers(repo: str, suffix: str) -> dict[str, list[str]]:
    occ = occurrences(repo)
    syms = matching(repo, suffix)
    out: dict[str, list[str]] = {}
    for o in occ:
        if o["role"] == "ref" and o["sym"] in syms:
            enc = enclosing(occ, str(o["file"]), int(str(o["line"])))
            key = short(enc) if enc else "<top-level>"
            out.setdefault(key, []).append(f"{o['file']}:{o['line']}")
    return out


def cmd_defs(repo: str, name: str) -> None:
    for o in occurrences(repo):
        s = str(o["sym"])
        if o["role"] == "DEF" and leaf(s) == name:
            print(f"{o['file']}:{o['line']}  {short(s)}")


def cmd_refs(repo: str, suffix: str) -> None:
    syms = matching(repo, suffix)
    print("symbols:", *sorted(map(short, syms)), sep="\n  ")
    for o in occurrences(repo):
        if o["role"] == "ref" and o["sym"] in syms:
            print(f"{o['file']}:{o['line']}")


def cmd_callers(repo: str, suffix: str) -> None:
    print("symbols:", *sorted(map(short, matching(repo, suffix))), sep="\n  ")
    for k, v in sorted(callers(repo, suffix).items()):
        print(f"{k}  <- {', '.join(v)}")


def cmd_callees(repo: str, file: str, start: int, end: int) -> None:
    seen: dict[str, int] = {}
    for o in occurrences(repo):
        if o["file"] == file and o["role"] == "ref" and start <= int(str(o["line"])) <= end:
            s = str(o["sym"])
            if is_fn(s):
                seen.setdefault(short(s), int(str(o["line"])))
    for s, ln in sorted(seen.items(), key=lambda kv: kv[1]):
        print(f"{ln}  {s}")


def cmd_check() -> int:
    """Re-verify every SCIP-checkable expectation in cases.py."""
    sys.path.insert(0, str(HERE))
    from cases import CASES

    spec = [{"id": c["id"], "repo": c["repo"], "symbol": "", **c["truth"]} for c in CASES]
    bad = 0
    for case in spec:
        repo, kind, sym = case["repo"], case["kind"], case["symbol"]
        if kind == "callers":
            got = {leaf(k) for k in callers(repo, sym) if k != "<top-level>"}
        elif kind == "defs":
            got = {str(o["file"]) for o in occurrences(repo) if o["role"] == "DEF" and str(o["sym"]).endswith(sym)}
        elif kind == "callees":
            f, a, b = case["range"]
            got = {
                leaf(str(o["sym"]))
                for o in occurrences(repo)
                if o["file"] == f and o["role"] == "ref" and a <= int(str(o["line"])) <= b and is_fn(str(o["sym"]))
            }
        elif kind == "ref_files":
            got = {str(o["file"]) for o in occurrences(repo) if o["role"] == "ref" and o["sym"] in matching(repo, sym)}
        else:  # "source": verified by reading the cited lines; nothing to recompute
            print(f"read {case['id']} {repo} {sym}: {case.get('evidence', '')}")
            continue
        want = set(case["expect"])
        ok = want <= got and (not case.get("exact") or want == got)
        bad += not ok
        tag = "ok  " if ok else "FAIL"
        print(f"{tag} {case['id']} {repo} {kind} {sym}: want {sorted(want)} got {sorted(got)}")
    auto = sum(1 for c in spec if c["kind"] != "source")
    print(f"{auto - bad}/{auto} SCIP-checked expectations hold ({len(spec) - auto} read-verified)")
    return 1 if bad else 0


def main(argv: list[str]) -> int:
    if not argv:
        print(__doc__)
        return 2
    cmd, *rest = argv
    if cmd == "defs":
        cmd_defs(rest[0], rest[1])
    elif cmd == "refs":
        cmd_refs(rest[0], rest[1])
    elif cmd == "callers":
        cmd_callers(rest[0], rest[1])
    elif cmd == "callees":
        cmd_callees(rest[0], rest[1], int(rest[2]), int(rest[3]))
    elif cmd == "check":
        return cmd_check()
    else:
        print(__doc__)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))

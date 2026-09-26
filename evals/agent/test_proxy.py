"""Self-tests for the agent proxy: sandbox confinement, output caps, the tool loop
against stub upstreams (OpenAI and Anthropic wire formats), usage summing, the
budget cap and the loopback guard. No network beyond 127.0.0.1, no codegraph binary.

    python -m unittest evals/agent/test_proxy.py -v
"""

from __future__ import annotations

import contextlib
import http.client
import io
import json
import os
import shutil
import subprocess
import sys
import tempfile
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parent))

import proxy  # noqa: E402

HAS_RG = shutil.which("rg") is not None


def make_corpus(root: Path) -> None:
    repo = root / "alpha"
    (repo / "src").mkdir(parents=True)
    (repo / ".git").mkdir()
    (repo / ".git" / "config").write_text(
        '[core]\n[remote "origin"]\n\turl = https://x:needle-gittoken@example.invalid/r.git\n', encoding="utf-8"
    )
    (repo / "credentials.json").write_text('{"needle-creds": 1}\n', encoding="utf-8")
    (repo / "Secrets.yaml").write_text("needle-secretsyaml: 1\n", encoding="utf-8")
    (repo / "server.pem").write_text("needle-pem\n", encoding="utf-8")
    (repo / "config").mkdir()
    (repo / "config" / ".npmrc").write_text("//r/:_authToken=needle-npm\n", encoding="utf-8")
    (repo / "src" / "main.go").write_text(
        "package main\n\nfunc helper() int { return 1 }\n\nfunc main() { helper() }\n", encoding="utf-8"
    )
    (repo / "src" / "big.txt").write_text("\n".join(f"line {i} needle" for i in range(2000)), encoding="utf-8")
    (repo / ".env").write_text("TOKEN=needle-secret\n", encoding="utf-8")
    (repo / "id_ed25519").write_text("needle-private-key\n", encoding="utf-8")
    (root.parent / "outside.txt").write_text("needle outside\n", encoding="utf-8")


def link_dir(link: Path, target: Path) -> bool:
    """Symlink, else an NTFS junction (no admin needed). False if neither works."""
    try:
        os.symlink(target, link, target_is_directory=True)
        return True
    except OSError:
        pass
    if os.name == "nt":
        res = subprocess.run(["cmd", "/c", "mklink", "/J", str(link), str(target)], capture_output=True)
        return res.returncode == 0
    return False


class SandboxTest(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = Path(tempfile.mkdtemp())
        self.root = self.tmp / "corpus"
        self.root.mkdir()
        make_corpus(self.root)
        self.corpus = proxy.Corpus(self.root)

    def tearDown(self) -> None:
        shutil.rmtree(self.tmp, ignore_errors=True)

    def refused(self, raw: str) -> None:
        with self.assertRaises(proxy.ToolError, msg=raw):
            self.corpus.resolve(raw)

    def test_inside_paths_resolve(self) -> None:
        p = self.corpus.resolve("alpha/src/main.go", want="file")
        self.assertEqual(p.name, "main.go")
        p2 = self.corpus.resolve(str(self.root / "alpha" / "src" / "main.go"), want="file")
        self.assertEqual(p, p2)
        self.assertEqual(self.corpus.resolve("alpha\\src\\main.go"), p)

    def test_escapes_refused(self) -> None:
        self.refused("../outside.txt")
        self.refused("alpha/../../outside.txt")
        self.refused(str(self.tmp / "outside.txt"))
        self.refused("C:/Windows/win.ini")
        self.refused("\\\\evil-host\\share\\x")
        self.refused("//evil-host/share/x")
        self.refused("alpha/src/\x00main.go")
        self.refused("")

    def test_secrets_and_git_refused(self) -> None:
        self.refused("alpha/.env")
        self.refused("alpha/id_ed25519")
        self.refused("alpha/.git/config")
        self.assertTrue(proxy.is_secret_name(".env.production"))
        self.assertTrue(proxy.is_secret_name("server.pem"))
        self.assertFalse(proxy.is_secret_name("envelope.go"))

    def test_symlink_or_junction_escape_refused(self) -> None:
        outside = self.tmp / "elsewhere"
        outside.mkdir()
        (outside / "loot.txt").write_text("needle loot\n", encoding="utf-8")
        if not link_dir(self.root / "alpha" / "link", outside):
            self.skipTest("cannot create a symlink or junction here")
        self.refused("alpha/link/loot.txt")
        self.refused("alpha/link")
        self.assertNotIn("link", self.corpus.repos())

    def test_repo_name_validated(self) -> None:
        with self.assertRaises(proxy.ToolError):
            self.corpus.repo_dir("../corpus")
        with self.assertRaises(proxy.ToolError):
            self.corpus.repo_dir("nope")
        self.assertEqual(self.corpus.repo_dir("alpha").name, "alpha")

    def test_codegraph_arg_guard(self) -> None:
        with self.assertRaises(proxy.ToolError):
            proxy.guard_codegraph_args(self.corpus, {"file": str(self.tmp / "outside.txt")})
        with self.assertRaises(proxy.ToolError):
            proxy.guard_codegraph_args(self.corpus, {"target": "alpha/../../outside.txt"})
        with self.assertRaises(proxy.ToolError):
            proxy.guard_codegraph_args(self.corpus, {"file": "\\\\host\\share\\x.go"})
        proxy.guard_codegraph_args(self.corpus, {"symbol": "alpha/src/main.go:3"})
        proxy.guard_codegraph_args(self.corpus, {"symbol": "pkg.Type.Method", "repo": "alpha"})
        proxy.guard_codegraph_args(self.corpus, {"file": str(self.root / "alpha" / "src" / "main.go")})


class ToolTest(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = Path(tempfile.mkdtemp())
        self.root = self.tmp / "corpus"
        self.root.mkdir()
        make_corpus(self.root)
        self.s = proxy.Settings(corpus=proxy.Corpus(self.root), max_tool_chars=2000)

    def tearDown(self) -> None:
        shutil.rmtree(self.tmp, ignore_errors=True)

    def test_cap_marks_truncation(self) -> None:
        text, cut = proxy.cap("a\n" * 5000, 100, "hint")
        self.assertTrue(cut)
        self.assertIn("[truncated: showing", text)
        self.assertLess(len(text), 200)
        self.assertEqual(proxy.cap("short", 100, "h"), ("short", False))

    def test_read_file_range_and_cap(self) -> None:
        out = proxy.tool_read_file(self.s, {"path": "alpha/src/main.go", "start_line": 3, "end_line": 3})
        self.assertIn("    3  func helper()", out)
        self.assertIn("lines 3-3 of 5", out)
        big = proxy.tool_read_file(self.s, {"path": "alpha/src/big.txt", "start_line": 1, "end_line": 1000})
        self.assertIn("[truncated:", big)
        self.assertLessEqual(len(big), 2200)
        with self.assertRaises(proxy.ToolError):
            proxy.tool_read_file(self.s, {"path": "alpha/src/main.go", "start_line": -1})

    @unittest.skipUnless(HAS_RG, "ripgrep not on PATH")
    def test_grep_confined_capped_and_secret_free(self) -> None:
        out = proxy.tool_grep(self.s, {"pattern": "needle"})
        self.assertIn("[per-file cap: only the first 30 matches shown for alpha/src/big.txt]", out)
        small = proxy.Settings(corpus=self.s.corpus, max_tool_chars=300)
        self.assertIn("[truncated:", proxy.tool_grep(small, {"pattern": "needle"}))
        self.assertNotIn("needle-secret", out)
        self.assertNotIn("needle-private-key", out)
        self.assertNotIn("outside", out)
        hit = proxy.tool_grep(self.s, {"pattern": r"helper\(", "repo": "alpha", "glob": "src/*.go"})
        self.assertIn("alpha/src/main.go:3:", hit)
        self.assertIn("alpha/src/main.go:5:", hit)
        self.assertEqual(proxy.tool_grep(self.s, {"pattern": "zzz-nothing"}), "no matches")
        with self.assertRaises(proxy.ToolError):
            proxy.tool_grep(self.s, {"pattern": "x", "repo": ".."})

    @unittest.skipUnless(HAS_RG, "ripgrep not on PATH")
    def test_grep_pattern_cannot_inject_flags(self) -> None:
        out = proxy.tool_grep(self.s, {"pattern": "--files", "repo": "alpha"})
        self.assertEqual(out, "no matches")
        out = proxy.tool_grep(self.s, {"pattern": "--pre=cmd", "repo": "alpha"})
        self.assertEqual(out, "no matches")

    @unittest.skipUnless(HAS_RG, "ripgrep not on PATH")
    def test_list_files_hides_secrets(self) -> None:
        out = proxy.tool_list_files(self.s, {"repo": "alpha"})
        self.assertIn("alpha/src/main.go", out)
        for leak in (".env", "id_ed25519", "credentials.json", "Secrets.yaml", "server.pem", ".npmrc", ".git/"):
            self.assertNotIn(leak, out)
        with self.assertRaises(proxy.ToolError):
            proxy.tool_list_files(self.s, {"repo": "alpha", "glob": ".env"})
        with self.assertRaises(proxy.ToolError):
            proxy.tool_list_files(self.s, {"repo": "alpha", "glob": "{.git,.git/*}"})


# --------------------------------------------------------------------------- SEC-2: grep secret bypasses

SECRET_MARKERS = (
    "needle-secret",
    "needle-private-key",
    "needle-gittoken",
    "needle-creds",
    "needle-secretsyaml",
    "needle-pem",
    "needle-npm",
)


class GrepSecretTest(unittest.TestCase):
    """Each case was a working bypass: a model-supplied glob appended after the
    excludes (last-match-wins, whitelist overrides hidden-file skipping) or a secret
    name the old exclude list did not cover."""

    def setUp(self) -> None:
        self.tmp = Path(tempfile.mkdtemp())
        self.root = self.tmp / "corpus"
        self.root.mkdir()
        make_corpus(self.root)
        self.s = proxy.Settings(corpus=proxy.Corpus(self.root), max_tool_chars=20000)

    def tearDown(self) -> None:
        shutil.rmtree(self.tmp, ignore_errors=True)

    def assert_clean(self, out: str) -> None:
        for marker in SECRET_MARKERS:
            self.assertNotIn(marker, out)

    def test_secret_globs_refused(self) -> None:
        for glob in (
            ".env",
            ".env*",
            "*.pem",
            "**/*.pem",
            "*.{pem,go}",
            "{.git,.git/*}",
            ".git/**",
            "**/.git/config",
            ".gi?/*",
            "credentials.json",
            "secrets.yaml",
            "SECRETS.YML",
            "id_rsa",
            "*/.npmrc",
            "!*.go",
            " !src/**",
        ):
            with self.subTest(glob=glob), self.assertRaises(proxy.ToolError):
                proxy._glob_arg(glob)

    def test_ordinary_globs_allowed(self) -> None:
        for glob in ("*", "**/*", "*.go", "src/**", "src/*.go", "**/*.{rs,go}", "*.gitignore", "docs/*.md"):
            with self.subTest(glob=glob):
                self.assertEqual(proxy._glob_arg(glob), f"--glob={glob}")

    @unittest.skipUnless(HAS_RG, "ripgrep not on PATH")
    def test_no_glob_hides_all_secret_names(self) -> None:
        out = proxy.tool_grep(self.s, {"pattern": "needle", "repo": "alpha"})
        self.assertIn("alpha/src/big.txt", out)
        self.assert_clean(out)
        self.assert_clean(proxy.tool_grep(self.s, {"pattern": "needle"}))  # corpus-wide too

    @unittest.skipUnless(HAS_RG, "ripgrep not on PATH")
    def test_audit_bypasses_via_tool_grep(self) -> None:
        for glob in (".env", "*.pem", "{.git,.git/*}", "credentials.json", "!src/**"):
            with self.subTest(glob=glob), self.assertRaises(proxy.ToolError):
                proxy.tool_grep(self.s, {"pattern": "needle", "repo": "alpha", "glob": glob})

    @unittest.skipUnless(HAS_RG, "ripgrep not on PATH")
    def test_broad_globs_still_hide_secrets(self) -> None:
        for glob in ("*", "**/*", "**"):
            with self.subTest(glob=glob):
                out = proxy.tool_grep(self.s, {"pattern": "needle", "repo": "alpha", "glob": glob})
                self.assertIn("alpha/src/big.txt", out)
                self.assert_clean(out)

    @unittest.skipUnless(HAS_RG, "ripgrep not on PATH")
    def test_excludes_come_after_user_glob(self) -> None:
        # Even a glob that slipped past the name check cannot re-include a secret: the
        # excludes are last on the command line, so they win.
        cmd: list[list[str]] = []
        real_run = proxy.subprocess.run

        def spy(c: list[str], **kw: object) -> object:
            cmd.append(c)
            return real_run(c, **kw)  # type: ignore[arg-type]

        proxy.subprocess.run = spy  # type: ignore[assignment]
        try:
            proxy.tool_grep(self.s, {"pattern": "needle", "repo": "alpha", "glob": "*.txt"})
        finally:
            proxy.subprocess.run = real_run  # type: ignore[assignment]
        argv = cmd[0]
        user = argv.index("--glob=*.txt")
        for g in proxy.SECRET_GLOBS:
            self.assertGreater(argv.index(g), user)
        self.assertIn("--iglob=!.git", argv)
        self.assertIn("--iglob=!**/.git/**", argv)

    @unittest.skipUnless(HAS_RG, "ripgrep not on PATH")
    def test_context_lines_filtered_and_formatted(self) -> None:
        out = proxy.tool_grep(self.s, {"pattern": "func helper", "repo": "alpha", "context": 1})
        self.assertIn("alpha/src/main.go:3:func helper()", out)
        self.assertIn("alpha/src/main.go-2-", out)
        self.assert_clean(out)

    def test_hidden_path_post_filter(self) -> None:
        for path in (
            "./.env",
            "alpha/.env.local",
            "alpha/credentials.json",
            "alpha/deep/Secrets.YAML",
            "alpha/.git/config",
            "./.GIT/HEAD",
            "alpha/sub/.git/objects/ab",
            "alpha/tls/server.key",
        ):
            with self.subTest(path=path):
                self.assertTrue(proxy.is_hidden_path(path))
        for path in ("alpha/src/main.go", "alpha/.gitignore", "alpha/.github/workflows/ci.yml", "./credentials.go"):
            with self.subTest(path=path):
                self.assertFalse(proxy.is_hidden_path(path))


# --------------------------------------------------------------------------- stub upstreams


class Stub:
    """Scripted upstream: replies[i] answers the i-th request; records bodies."""

    def __init__(self, replies: list[dict[str, Any]], anthropic: bool = False) -> None:
        self.replies = replies
        self.bodies: list[dict[str, Any]] = []
        stub = self

        class H(BaseHTTPRequestHandler):
            def log_message(self, *a: Any) -> None:
                pass

            def do_POST(self) -> None:
                n = int(self.headers.get("Content-Length") or 0)
                body = json.loads(self.rfile.read(n))
                headers = {k.lower(): v for k, v in self.headers.items()}
                stub.bodies.append({"path": self.path, "headers": headers, "body": body})
                reply = stub.replies[min(len(stub.bodies) - 1, len(stub.replies) - 1)]
                data = json.dumps(reply).encode()
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), H)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.url = f"http://127.0.0.1:{self.server.server_address[1]}/v1"

    def close(self) -> None:
        self.server.shutdown()
        self.server.server_close()


def oa_tool_call(name: str, args: dict[str, Any], pt: int, ct: int) -> dict[str, Any]:
    return {
        "choices": [
            {
                "message": {
                    "role": "assistant",
                    "content": "",
                    "tool_calls": [
                        {"id": "call_1", "type": "function", "function": {"name": name, "arguments": json.dumps(args)}}
                    ],
                }
            }
        ],
        "usage": {"prompt_tokens": pt, "completion_tokens": ct},
    }


def oa_answer(text: str, pt: int, ct: int, cached: int = 0) -> dict[str, Any]:
    return {
        "choices": [{"message": {"role": "assistant", "content": text}}],
        "usage": {"prompt_tokens": pt, "completion_tokens": ct, "prompt_tokens_details": {"cached_tokens": cached}},
    }


class LoopTest(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = Path(tempfile.mkdtemp())
        self.root = self.tmp / "corpus"
        self.root.mkdir()
        make_corpus(self.root)
        self.settings = proxy.Settings(corpus=proxy.Corpus(self.root))
        self.stubs: list[Stub] = []

    def tearDown(self) -> None:
        for s in self.stubs:
            s.close()
        shutil.rmtree(self.tmp, ignore_errors=True)

    def agent(self, stub: Stub, kind: str = "openai", budget: float | None = None, max_turns: int = 12) -> proxy.Agent:
        up = proxy.Upstream(stub.url, kind, "m-{arm}", "sk-test", 10, 256)
        ledger = proxy.Ledger(self.tmp / "spend.json", budget, proxy.Prices(1.0, 5.0, 0.1))
        tracer = proxy.Tracer(self.tmp / "trace.jsonl")
        arms = proxy.build_arms(self.settings, None)
        return proxy.Agent(self.settings, up, arms, ledger, tracer, None, max_turns=max_turns)

    def stub(self, replies: list[dict[str, Any]]) -> Stub:
        s = Stub(replies)
        self.stubs.append(s)
        return s

    def test_loop_sums_usage_and_traces(self) -> None:
        stub = self.stub(
            [
                oa_tool_call("read_file", {"path": "alpha/src/main.go"}, 100, 10),
                oa_tool_call("read_file", {"path": "../outside.txt"}, 200, 20),
                oa_answer("helper", 300, 5, cached=50),
            ]
        )
        rec = self.agent(stub).run("grep", "[Q42] who calls helper?")
        self.assertEqual(rec["stop"], "answer")
        self.assertEqual(rec["final"], "helper")
        self.assertEqual(rec["case"], "Q42")
        self.assertEqual(rec["turns"], 3)
        self.assertEqual(rec["usage"], {"input": 600, "output": 35, "cache_read": 50, "cache_write": 0})
        self.assertEqual([c["error"] for c in rec["tool_calls"]], [False, True])
        # the model saw the tool result, prefixed with the marker the fake rules key on
        second = stub.bodies[1]["body"]["messages"]
        self.assertEqual(second[-1]["role"], "tool")
        self.assertTrue(second[-1]["content"].startswith("[tool read_file]\n"))
        self.assertIn("func helper()", second[-1]["content"])
        third = stub.bodies[2]["body"]["messages"]
        self.assertIn("outside the corpus root", third[-1]["content"])
        self.assertEqual(stub.bodies[0]["body"]["model"], "m-grep")
        self.assertEqual(stub.bodies[0]["headers"].get("authorization"), "Bearer sk-test")
        names = {t["function"]["name"] for t in stub.bodies[0]["body"]["tools"]}
        self.assertEqual(names, {"grep", "read_file", "list_files"})
        line = json.loads((self.tmp / "trace.jsonl").read_text(encoding="utf-8").splitlines()[-1])
        self.assertEqual(line["usage"]["input"], 600)
        self.assertAlmostEqual(line["cost_usd"], (550 * 1.0 + 50 * 0.1 + 35 * 5.0) / 1e6)

    def test_unknown_tool_and_turn_cap(self) -> None:
        stub = self.stub([oa_tool_call("codegraph_callers", {"symbol": "x"}, 10, 1)])
        rec = self.agent(stub, max_turns=3).run("grep", "[Q1] q")
        self.assertEqual(rec["turns"], 3)
        self.assertIn("unknown tool", json.dumps(stub.bodies[1]["body"]["messages"]))
        last = stub.bodies[-1]["body"]
        self.assertNotIn("tools", last)  # final turn forces an answer
        self.assertIn("Turn limit reached", json.dumps(last["messages"]))

    def test_budget_cap_stops_before_spending(self) -> None:
        stub = self.stub([oa_answer("x", 10, 1)])
        rec = self.agent(stub, budget=0.0).run("grep", "[Q1] q")
        self.assertEqual(rec["stop"], "budget")
        self.assertEqual(stub.bodies, [])

    def test_ledger_persists(self) -> None:
        stub = self.stub([oa_answer("x", 1_000_000, 0)])
        self.agent(stub, budget=10.0).run("grep", "[Q1] q")
        again = proxy.Ledger(self.tmp / "spend.json", 1.0, proxy.Prices())
        self.assertAlmostEqual(again.spent, 1.0)
        with self.assertRaises(proxy.BudgetExceeded):
            again.check()

    def test_in_flight_reservation_blocks_other_requests(self) -> None:
        # Another request holds a reservation that fills the budget: this one must be
        # refused before it reaches the upstream, not after.
        stub = self.stub([oa_answer("x", 10, 1)])
        agent = self.agent(stub, budget=0.05)
        held = agent.ledger.reserve(proxy.Usage(input=49_900, output=0))  # $0.0499 of $0.05
        rec = agent.run("grep", "[Q1] q")
        self.assertEqual(rec["stop"], "budget")
        self.assertEqual(stub.bodies, [])
        agent.ledger.release(held)
        self.assertEqual(agent.run("grep", "[Q1] q")["stop"], "answer")
        self.assertEqual(agent.ledger.reserved, 0.0)

    def test_upstream_error_releases_reservation(self) -> None:
        up = proxy.Upstream("http://127.0.0.1:9/v1", "openai", "m-{arm}", None, 2, 256)
        up.retries = 0
        ledger = proxy.Ledger(self.tmp / "spend.json", 1.0, proxy.Prices())
        agent = proxy.Agent(
            self.settings, up, proxy.build_arms(self.settings, None), ledger, proxy.Tracer(self.tmp / "t.jsonl")
        )
        rec = agent.run("grep", "[Q1] q")
        self.assertEqual(rec["stop"], "upstream_error")
        self.assertEqual(ledger.reserved, 0.0)
        self.assertEqual(ledger.spent, 0.0)

    def test_anthropic_wire_format(self) -> None:
        stub = self.stub(
            [
                {
                    "content": [
                        {"type": "text", "text": "looking"},
                        {"type": "tool_use", "id": "tu_1", "name": "read_file", "input": {"path": "alpha/src/main.go"}},
                        {"type": "tool_use", "id": "tu_2", "name": "list_files", "input": {"repo": "alpha"}},
                    ],
                    "usage": {"input_tokens": 100, "output_tokens": 10, "cache_read_input_tokens": 400},
                },
                {
                    "content": [{"type": "text", "text": "main"}],
                    "usage": {"input_tokens": 50, "output_tokens": 3, "cache_creation_input_tokens": 20},
                },
            ]
        )
        rec = self.agent(stub, kind="anthropic").run("grep", "[Q2] q")
        self.assertEqual(rec["final"], "main")
        self.assertEqual(rec["usage"], {"input": 570, "output": 13, "cache_read": 400, "cache_write": 20})
        req = stub.bodies[1]
        self.assertTrue(req["path"].endswith("/messages"))
        self.assertEqual(req["headers"].get("x-api-key"), "sk-test")
        self.assertEqual(req["headers"].get("anthropic-version"), "2023-06-01")
        body = req["body"]
        self.assertEqual(body["system"][0]["cache_control"], {"type": "ephemeral"})
        self.assertIn("input_schema", body["tools"][0])
        msgs = body["messages"]
        self.assertEqual([m["role"] for m in msgs], ["user", "assistant", "user"])
        self.assertEqual([b["type"] for b in msgs[1]["content"]], ["text", "tool_use", "tool_use"])
        results = msgs[2]["content"]  # parallel results share one user turn
        self.assertEqual([b["tool_use_id"] for b in results], ["tu_1", "tu_2"])


class LedgerTest(unittest.TestCase):
    """SEC-8: the budget ledger must fail closed."""

    def setUp(self) -> None:
        self.tmp = Path(tempfile.mkdtemp())
        self.path = self.tmp / "agent" / "spend.json"
        self.path.parent.mkdir(parents=True)

    def tearDown(self) -> None:
        shutil.rmtree(self.tmp, ignore_errors=True)

    def test_unreadable_ledger_refuses_to_load(self) -> None:
        for content in (
            "",
            "{",
            '{"spent_usd": 1.5',  # truncated
            "[1]",
            "{}",
            '{"spent_usd": "3"}',
            '{"spent_usd": NaN}',
            '{"spent_usd": Infinity}',
            '{"spent_usd": -1}',
            '{"spent_usd": true}',
        ):
            with self.subTest(content=content):
                self.path.write_text(content, encoding="utf-8")
                with self.assertRaises(proxy.LedgerError) as cm:
                    proxy.Ledger(self.path, 5.0, proxy.Prices())
                self.assertIn("reset", str(cm.exception))
        self.path.write_bytes(b"\xff\xfe\x00garbage")
        with self.assertRaises(proxy.LedgerError):
            proxy.Ledger(self.path, 5.0, proxy.Prices())

    def test_non_finite_budget_or_price_refused(self) -> None:
        for budget in (float("nan"), float("inf"), -1.0):
            with self.subTest(budget=budget), self.assertRaises(proxy.LedgerError):
                proxy.Ledger(self.path, budget, proxy.Prices())
        with self.assertRaises(proxy.LedgerError):
            proxy.Ledger(self.path, 5.0, proxy.Prices(input=float("nan")))
        with self.assertRaises(proxy.LedgerError):
            proxy.Ledger(self.path, 5.0, proxy.Prices(output=-5.0))

    def test_valid_ledger_loads(self) -> None:
        self.path.write_text('{"spent_usd": 2.25}', encoding="utf-8")
        self.assertEqual(proxy.Ledger(self.path, 5.0, proxy.Prices()).spent, 2.25)

    def test_write_is_atomic(self) -> None:
        ledger = proxy.Ledger(self.path, None, proxy.Prices())
        ledger.charge(proxy.Usage(input=1_000_000))
        self.assertEqual(json.loads(self.path.read_text(encoding="utf-8")), {"spent_usd": 1.0})
        real_replace = proxy.os.replace

        def boom(src: str, dst: object) -> None:
            raise OSError("disk full")

        proxy.os.replace = boom  # type: ignore[assignment]
        try:
            with self.assertRaises(OSError):
                ledger.charge(proxy.Usage(input=1_000_000))
        finally:
            proxy.os.replace = real_replace  # type: ignore[assignment]
        # the old file is intact (never truncated) and no temp file is left behind
        self.assertEqual(json.loads(self.path.read_text(encoding="utf-8")), {"spent_usd": 1.0})
        self.assertEqual(sorted(p.name for p in self.path.parent.iterdir()), ["spend.json"])

    def test_concurrent_reservations_never_overshoot(self) -> None:
        ledger = proxy.Ledger(self.path, 1.0, proxy.Prices())
        turn = proxy.Usage(input=100_000)  # $0.10 each: exactly 10 fit
        ok: list[float] = []
        refused = 0
        lock = threading.Lock()
        start = threading.Barrier(32)

        def worker() -> None:
            nonlocal refused
            start.wait()
            try:
                held = ledger.reserve(turn)
            except proxy.BudgetExceeded:
                with lock:
                    refused += 1
                return
            with lock:
                ok.append(held)

        threads = [threading.Thread(target=worker) for _ in range(32)]
        for t in threads:
            t.start()
        for t in threads:
            t.join()
        self.assertEqual(len(ok), 10)
        self.assertEqual(refused, 22)
        # the persisted value counts in-flight reservations (a crash over-counts)
        self.assertAlmostEqual(json.loads(self.path.read_text(encoding="utf-8"))["spent_usd"], 1.0)
        for held in ok:
            ledger.settle(held, proxy.Usage(input=50_000))
        self.assertAlmostEqual(ledger.spent, 0.5)
        self.assertAlmostEqual(ledger.reserved, 0.0)
        self.assertAlmostEqual(proxy.Ledger(self.path, 1.0, proxy.Prices()).spent, 0.5)

    def test_main_refuses_corrupt_ledger(self) -> None:
        corpus = self.tmp / "corpus"
        corpus.mkdir()
        make_corpus(corpus)
        self.path.write_text('{"spent_usd": 4.9', encoding="utf-8")
        env = {"CGAGENT_LEDGER": str(self.path), "CGAGENT_BUDGET_USD": "5"}
        old = {k: os.environ.get(k) for k in env}
        os.environ.update(env)
        err = io.StringIO()
        try:
            with contextlib.redirect_stderr(err):
                rc = proxy.main(
                    ["--corpus", str(corpus), "--no-codegraph", "--port", "0", "--state-dir", str(self.tmp / "st")]
                )
        finally:
            for k, v in old.items():
                if v is None:
                    os.environ.pop(k, None)
                else:
                    os.environ[k] = v
        self.assertEqual(rc, 2)
        self.assertIn("refusing to start", err.getvalue())
        self.assertIn(str(self.path), err.getvalue())
        self.assertEqual(self.path.read_text(encoding="utf-8"), '{"spent_usd": 4.9')  # untouched


class HttpTest(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = Path(tempfile.mkdtemp())
        root = self.tmp / "corpus"
        root.mkdir()
        make_corpus(root)
        settings = proxy.Settings(corpus=proxy.Corpus(root))
        self.stub = Stub([oa_answer("helper", 40, 2)])
        up = proxy.Upstream(self.stub.url, "openai", "m-{arm}", None, 10, 256)
        ledger = proxy.Ledger(self.tmp / "spend.json", None, proxy.Prices())
        agent = proxy.Agent(
            settings, up, proxy.build_arms(settings, None), ledger, proxy.Tracer(self.tmp / "t.jsonl"), None
        )
        self.server = proxy.serve(agent, 0)
        self.port = self.server.server_address[1]
        threading.Thread(target=self.server.serve_forever, daemon=True).start()

    def tearDown(self) -> None:
        self.server.shutdown()
        self.server.server_close()
        self.stub.close()
        shutil.rmtree(self.tmp, ignore_errors=True)

    def req(self, method: str, path: str, body: Any = None, headers: dict[str, str] | None = None) -> tuple[int, Any]:
        conn = http.client.HTTPConnection("127.0.0.1", self.port, timeout=10)
        data = json.dumps(body).encode() if body is not None else None
        hdrs = {"Content-Type": "application/json", **(headers or {})}
        conn.request(method, path, body=data, headers=hdrs)
        resp = conn.getresponse()
        out = json.loads(resp.read() or b"null")
        conn.close()
        return resp.status, out

    def test_models_lists_arms(self) -> None:
        status, body = self.req("GET", "/v1/models")
        self.assertEqual(status, 200)
        self.assertEqual([m["id"] for m in body["data"]], ["grep"])

    def test_completion_roundtrip(self) -> None:
        status, body = self.req(
            "POST", "/v1/chat/completions", {"model": "grep", "messages": [{"role": "user", "content": "[Q3] q"}]}
        )
        self.assertEqual(status, 200)
        self.assertEqual(body["choices"][0]["message"]["content"], "helper")
        self.assertEqual(body["usage"]["prompt_tokens"], 40)
        self.assertEqual(body["usage"]["completion_tokens"], 2)

    def test_unknown_arm_404(self) -> None:
        status, _ = self.req("POST", "/v1/chat/completions", {"model": "codegraph", "messages": []})
        self.assertEqual(status, 404)

    def test_non_loopback_host_and_browser_refused(self) -> None:
        status, _ = self.req("GET", "/v1/models", headers={"Host": "evil.example:80"})
        self.assertEqual(status, 403)
        status, _ = self.req("GET", "/v1/models", headers={"Origin": "https://evil.example"})
        self.assertEqual(status, 403)
        status, _ = self.req("GET", "/v1/models", headers={"Sec-Fetch-Site": "cross-site"})
        self.assertEqual(status, 403)


if __name__ == "__main__":
    unittest.main()

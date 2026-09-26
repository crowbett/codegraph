"""cgagent: an OpenAI-compatible agent proxy for the codegraph agent eval.

evalrig is single-turn, so the agent loop lives here. evalrig POSTs one chat
completion with ``model`` = an arm name (``grep`` or ``codegraph``); the proxy runs a
tool loop against an UPSTREAM model with that arm's tools, executes the tool calls
locally (read-only, confined to the corpus root), and returns one completion whose
``usage`` is the SUM over all upstream turns and whose content is the final answer.
Every request is traced as one JSONL line.

    python evals/agent/proxy.py --port 8791 --corpus <dir> [--codegraph <exe>]

Upstream (env):
    CGAGENT_UPSTREAM_URL        base URL, e.g. http://127.0.0.1:8765/v1 or https://api.anthropic.com/v1
    CGAGENT_UPSTREAM_KIND       openai (default) | anthropic (native Messages API)
    CGAGENT_UPSTREAM_MODEL      model id; "{arm}" is replaced by the arm name (fake rules key on it)
    CGAGENT_UPSTREAM_KEY_ENV    name of the env var holding the API key (never the key itself)
    CGAGENT_BUDGET_USD          hard spend cap across the proxy's lifetime (ledger persisted)
    CGAGENT_PRICE_IN / _OUT / _CACHE_READ   USD per 1M tokens (default: Haiku 4.5, 1 / 5 / 0.1)

Stdlib only. Loopback only (Host allowlist, browser Origin refused).
"""

from __future__ import annotations

import argparse
import contextlib
import fnmatch
import json
import math
import os
import queue
import re
import stat
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.request
from dataclasses import dataclass, field
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any

HERE = Path(__file__).resolve().parent
REPO_ROOT = HERE.parent.parent
DEFAULT_STATE = REPO_ROOT / ".cache" / "evals"
ARMS = ("grep", "codegraph")
MAX_BODY = 1 << 20
MAX_MATCHES_PER_FILE = 30
TOOL_MARK = "[tool {name}]"  # prefixes every tool result; the fake rules key on it

JSON = dict[str, Any]


# --------------------------------------------------------------------------- sandbox


class ToolError(Exception):
    """A refused or failed tool call; its message goes back to the model."""


SECRET_NAME = re.compile(
    r"(?i)^(\.env(\..*)?|.*\.(pem|key|p12|pfx|jks|keystore|kdbx|ppk|asc|gpg)|id_(rsa|dsa|ecdsa|ed25519)(\.pub)?"
    r"|\.netrc|_netrc|\.npmrc|\.pypirc|\.git-credentials|credentials(\.json)?|secrets?\.(json|toml|ya?ml))$"
)
REPARSE = getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0x400)


def is_secret_name(name: str) -> bool:
    return bool(SECRET_NAME.match(name))


def _is_link(p: Path) -> bool:
    try:
        st = os.lstat(p)
    except OSError:
        return False
    if stat.S_ISLNK(st.st_mode):
        return True
    return bool(getattr(st, "st_file_attributes", 0) & REPARSE)


class Corpus:
    """Read-only view of the corpus root. Paths are never checked textually only:
    the candidate is normalized, every component is lstat'ed (symlinks and junctions
    refused), and the final realpath must still sit under the root."""

    def __init__(self, root: Path) -> None:
        self.root = Path(os.path.realpath(root))
        if not self.root.is_dir():
            raise SystemExit(f"corpus root not found: {root}")

    def repos(self) -> list[str]:
        return sorted(
            p.name for p in self.root.iterdir() if p.is_dir() and not p.name.startswith(".") and not _is_link(p)
        )

    def rel(self, p: Path) -> str:
        return p.relative_to(self.root).as_posix()

    def resolve(self, raw: object, *, want: str = "any") -> Path:
        if not isinstance(raw, str) or not raw.strip():
            raise ToolError("path must be a non-empty string")
        s = raw.strip().replace("\\", "/")
        if s.startswith("//") or "\x00" in s:
            raise ToolError(f"refused path {raw!r}: UNC and NUL paths are not allowed")
        cand = Path(s)
        if not cand.is_absolute() and not re.match(r"^[A-Za-z]:", s):
            cand = self.root / s
        norm = Path(os.path.normpath(os.path.abspath(cand)))
        try:
            parts = norm.relative_to(self.root).parts
        except ValueError:
            raise ToolError(f"refused path {raw!r}: outside the corpus root") from None
        cur = self.root
        for part in parts:
            if part == ".git":
                raise ToolError(f"refused path {raw!r}: .git internals are not readable")
            cur = cur / part
            if _is_link(cur):
                raise ToolError(f"refused path {raw!r}: symlinks/junctions are not followed")
        real = Path(os.path.realpath(norm))
        if real != self.root and self.root not in real.parents:
            raise ToolError(f"refused path {raw!r}: resolves outside the corpus root")
        if parts and is_secret_name(parts[-1]):
            raise ToolError(f"refused path {raw!r}: credential/secret files are never returned")
        if not real.exists():
            raise ToolError(f"no such file or directory: {raw}")
        if want == "file" and not real.is_file():
            raise ToolError(f"not a file: {raw}")
        if want == "dir" and not real.is_dir():
            raise ToolError(f"not a directory: {raw}")
        return real

    def repo_dir(self, repo: object) -> Path:
        if repo in (None, ""):
            return self.root
        if not isinstance(repo, str) or not re.fullmatch(r"[A-Za-z0-9._-]+", repo):
            raise ToolError(f"unknown repo {repo!r}; one of: {', '.join(self.repos())}")
        return self.resolve(repo, want="dir")


def cap(text: str, limit: int, hint: str) -> tuple[str, bool]:
    """Cap tool output at `limit` chars on a line boundary, with a visible marker."""
    if len(text) <= limit:
        return text, False
    cut = text.rfind("\n", 0, limit)
    cut = cut if cut > 0 else limit
    shown = text[:cut]
    total = text.count("\n") + 1
    kept = shown.count("\n") + 1
    return f"{shown}\n[truncated: showing {kept} of {total} lines; {hint}]", True


# --------------------------------------------------------------------------- tools

GREP_TOOLS: list[JSON] = [
    {
        "name": "grep",
        "description": (
            "Search file contents with ripgrep (Rust regex syntax). Returns matching lines as "
            "`repo/path:line:text`, at most 30 matches per file. Use `repo` to search one "
            'repository and `glob` to narrow files (relative to the repo, e.g. "*.go", '
            '"src/**/*.rs").'
        ),
        "parameters": {
            "type": "object",
            "properties": {
                "pattern": {"type": "string", "description": "Regex to search for."},
                "glob": {"type": "string", "description": "Optional file glob filter."},
                "repo": {"type": "string", "description": "Optional repo directory name."},
                "ignore_case": {"type": "boolean", "default": False},
                "context": {
                    "type": "integer",
                    "minimum": 0,
                    "maximum": 3,
                    "description": "Lines of context around each match (0-3).",
                },
            },
            "required": ["pattern"],
        },
    },
]

FILE_TOOLS: list[JSON] = [
    {
        "name": "read_file",
        "description": (
            "Read a line range of a file, line-numbered. `path` is corpus-relative "
            '(e.g. "chi/mux.go"). At most 400 lines per call.'
        ),
        "parameters": {
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "start_line": {"type": "integer", "minimum": 1, "default": 1},
                "end_line": {"type": "integer", "minimum": 1},
            },
            "required": ["path"],
        },
    },
    {
        "name": "list_files",
        "description": "List files (corpus-relative paths) matching a glob, optionally in one repo.",
        "parameters": {
            "type": "object",
            "properties": {
                "glob": {"type": "string", "description": 'e.g. "**/*.py" (default: all files).'},
                "repo": {"type": "string"},
            },
        },
    },
]


@dataclass
class Settings:
    corpus: Corpus
    max_tool_chars: int = 12000
    max_read_lines: int = 400
    rg: str = "rg"
    tool_timeout_s: float = 30.0


def rg_env() -> dict[str, str]:
    env = {k: v for k, v in os.environ.items() if not k.startswith("RIPGREP")}
    env.update({"NO_COLOR": "1", "CLICOLOR_FORCE": "0"})
    return env


# Exclude globs mirroring SECRET_NAME (case-insensitive, like the regex) plus .git
# internals. ripgrep globs are last-match-wins and a whitelist glob overrides its
# hidden-file skipping, so these MUST come after any caller-supplied glob.
SECRET_GLOBS = [
    f"--iglob=!{g}"
    for g in (
        ".env",
        ".env.*",
        "*.{pem,key,p12,pfx,jks,keystore,kdbx,ppk,asc,gpg}",
        "id_{rsa,dsa,ecdsa,ed25519}",
        "id_{rsa,dsa,ecdsa,ed25519}.pub",
        "{.netrc,_netrc,.npmrc,.pypirc,.git-credentials}",
        "{credentials,credentials.json}",
        "{secret,secrets}.{json,toml,yaml,yml}",
        ".git",
        "**/.git/**",
        "**/node_modules/**",
    )
]

# A glob is refused when it targets a secret/.git name without also matching an
# ordinary source file (so "*" and "src/**" stay allowed; the trailing excludes and
# the output post-filter still hide secrets from those).
_SECRET_SAMPLES = (
    ".env",
    ".env.local",
    "server.pem",
    "tls.key",
    "store.p12",
    "id_rsa",
    "id_ed25519.pub",
    ".netrc",
    ".npmrc",
    ".git-credentials",
    "credentials",
    "credentials.json",
    "secrets.yaml",
    "secret.json",
    ".git",
)
_BENIGN_SAMPLES = ("main.go", "src", "README.md", "lib.rs", "index.ts")


def _expand_braces(glob: str, depth: int = 0) -> list[str]:
    """`a{b,c}d` -> [abd, acd] (nested, bounded). Unbalanced braces stay literal."""
    start = glob.find("{")
    if start < 0 or depth > 4:
        return [glob]
    level = 0
    for i in range(start, len(glob)):
        if glob[i] == "{":
            level += 1
        elif glob[i] == "}":
            level -= 1
            if level == 0:
                inner, parts, lv, last = glob[start + 1 : i], [], 0, 0
                for j, ch in enumerate(inner):
                    lv += ch == "{"
                    lv -= ch == "}"
                    if ch == "," and lv == 0:
                        parts.append(inner[last:j])
                        last = j + 1
                parts.append(inner[last:])
                out: list[str] = []
                for p in parts:
                    out += _expand_braces(glob[:start] + p + glob[i + 1 :], depth + 1)
                    if len(out) > 256:
                        raise ToolError("glob has too many alternatives")
                return out
    return [glob]


def _glob_targets_secret(glob: str) -> bool:
    for alt in _expand_braces(glob):
        for comp in alt.replace("\\", "/").split("/"):
            if not comp or comp == "**":
                continue
            c = comp.lower()
            if is_secret_name(comp) or c == ".git":
                return True
            hits_secret = any(fnmatch.fnmatchcase(s, c) for s in _SECRET_SAMPLES)
            if hits_secret and not any(fnmatch.fnmatchcase(b.lower(), c) for b in _BENIGN_SAMPLES):
                return True
    return False


def _glob_arg(glob: object) -> str | None:
    if glob in (None, ""):
        return None
    if not isinstance(glob, str) or len(glob) > 200 or "\x00" in glob:
        raise ToolError("glob must be a short string")
    if glob.lstrip().startswith("!"):
        raise ToolError("negated globs ('!...') are not allowed; use a positive filter")
    if _glob_targets_secret(glob):
        raise ToolError(f"refused glob {glob!r}: credential/secret files and .git internals are never searched")
    return f"--glob={glob}"


def is_hidden_path(path: str) -> bool:
    """True if a tool output path names a secret file or sits inside .git."""
    parts = [p for p in path.replace("\\", "/").split("/") if p not in ("", ".")]
    if not parts:
        return False
    return is_secret_name(parts[-1]) or any(p.lower() == ".git" for p in parts)


def _prefix(line: str, prefix: str) -> str:
    """`./src/x.rs:3:..` -> `<repo>/src/x.rs:3:..`; `--` context separators pass through."""
    return prefix + line.removeprefix("./") if line.startswith("./") else line


def tool_grep(s: Settings, args: JSON) -> str:
    pattern = args.get("pattern")
    if not isinstance(pattern, str) or not pattern:
        raise ToolError("pattern must be a non-empty string")
    target = s.corpus.repo_dir(args.get("repo"))
    cmd = [
        s.rg,
        "--no-config",
        "--line-number",
        "--no-heading",
        "--with-filename",
        "--color=never",
        "--path-separator=/",
        "--max-columns=240",
        "--max-columns-preview",
        f"--max-count={MAX_MATCHES_PER_FILE}",
        "--max-filesize=1M",
        "--null",  # path\0rest: lets the post-filter find the path unambiguously
    ]
    if args.get("ignore_case"):
        cmd.append("--ignore-case")
    ctx = args.get("context")
    if isinstance(ctx, int) and 0 < ctx <= 3:
        cmd.append(f"--context={ctx}")
    g = _glob_arg(args.get("glob"))
    if g:
        cmd.append(g)
    cmd += SECRET_GLOBS  # last: ripgrep globs are last-match-wins
    # Search from inside the repo so globs like "src/**" are repo-relative, then prefix
    # the repo name back onto every path.
    cmd += ["--regexp", pattern, "--", "."]
    prefix = "" if target == s.corpus.root else s.corpus.rel(target) + "/"
    try:
        res = subprocess.run(
            cmd,
            cwd=target,
            capture_output=True,
            text=True,
            encoding="utf-8",
            errors="replace",
            timeout=s.tool_timeout_s,
            env=rg_env(),
        )
    except subprocess.TimeoutExpired:
        raise ToolError("grep timed out; narrow the pattern or glob") from None
    if res.returncode == 1:
        return "no matches"
    if res.returncode != 0:
        raise ToolError(f"grep failed: {res.stderr.strip()[:300]}")
    lines: list[str] = []
    for raw in res.stdout.splitlines():
        path, nul, rest = raw.partition("\x00")
        if not nul:  # `--` context separator (the only line without a path)
            if raw == "--" and lines and lines[-1] != "--":
                lines.append(raw)
            continue
        if is_hidden_path(path):
            continue
        sep = ":" if re.match(r"\d+:", rest) else "-"
        lines.append(_prefix(path, prefix) + sep + rest)
    while lines and lines[-1] == "--":
        lines.pop()
    if not lines:
        return "no matches"
    per_file: dict[str, int] = {}
    for line in lines:
        m = re.match(r"^(.+?):\d+:", line)
        if m:
            per_file[m.group(1)] = per_file.get(m.group(1), 0) + 1
    capped = sorted(f for f, n in per_file.items() if n >= MAX_MATCHES_PER_FILE)
    out = "\n".join(lines)
    if capped:
        out += f"\n[per-file cap: only the first {MAX_MATCHES_PER_FILE} matches shown for {', '.join(capped[:5])}]"
    text, _ = cap(out, s.max_tool_chars, "narrow the pattern, glob or repo")
    return text


def tool_read_file(s: Settings, args: JSON) -> str:
    path = s.corpus.resolve(args.get("path"), want="file")
    start = args.get("start_line") or 1
    if not isinstance(start, int) or start < 1:
        raise ToolError("start_line must be a positive integer")
    end = args.get("end_line") or start + 199
    if not isinstance(end, int) or end < start:
        raise ToolError("end_line must be an integer >= start_line")
    end = min(end, start + s.max_read_lines - 1)
    with open(path, "rb") as f:
        head = f.read(8192)
        if b"\x00" in head:
            raise ToolError("binary file")
        f.seek(0)
        lines = f.read().decode("utf-8", errors="replace").splitlines()
    chunk = lines[start - 1 : end]
    if not chunk:
        return f"{s.corpus.rel(path)} has {len(lines)} lines; nothing in {start}-{end}"
    body = "\n".join(f"{n:>5}  {t}" for n, t in enumerate(chunk, start))
    head_line = f"{s.corpus.rel(path)} lines {start}-{start + len(chunk) - 1} of {len(lines)}"
    text, _ = cap(f"{head_line}\n{body}", s.max_tool_chars, "read a smaller range")
    return text


def tool_list_files(s: Settings, args: JSON) -> str:
    target = s.corpus.repo_dir(args.get("repo"))
    cmd = [s.rg, "--no-config", "--files", "--color=never", "--path-separator=/"]
    g = _glob_arg(args.get("glob"))
    if g:
        cmd.append(g)
    cmd += [*SECRET_GLOBS, "--", "."]  # excludes last: ripgrep globs are last-match-wins
    prefix = "" if target == s.corpus.root else s.corpus.rel(target) + "/"
    res = subprocess.run(
        cmd,
        cwd=target,
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        timeout=s.tool_timeout_s,
        env=rg_env(),
    )
    if res.returncode not in (0, 1):
        raise ToolError(f"list_files failed: {res.stderr.strip()[:300]}")
    files = sorted(_prefix(line, prefix) for line in res.stdout.splitlines())
    files = [f for f in files if not is_hidden_path(f)]
    if not files:
        return "no files"
    text, _ = cap("\n".join(files), s.max_tool_chars, "use a narrower glob")
    return text


# --------------------------------------------------------------------------- codegraph MCP


class McpClient:
    """One `codegraph mcp` stdio server, shared by all requests (calls serialized)."""

    def __init__(self, exe: Path, cwd: Path, env: dict[str, str], timeout_s: float = 120.0) -> None:
        self.lock = threading.Lock()
        self.timeout_s = timeout_s
        self.proc = subprocess.Popen(
            [str(exe), "mcp"],
            cwd=cwd,
            env=env,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0),
        )
        self.lines: queue.Queue[bytes] = queue.Queue()
        threading.Thread(target=self._pump, daemon=True).start()
        self._id = 0
        self.rpc(
            "initialize",
            {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "cgagent", "version": "0.1"}},
        )
        self._send({"jsonrpc": "2.0", "method": "notifications/initialized"})
        self.tools: list[JSON] = self.rpc("tools/list", {}).get("tools", [])

    def _pump(self) -> None:
        assert self.proc.stdout is not None
        for line in self.proc.stdout:
            self.lines.put(line)

    def _send(self, msg: JSON) -> None:
        assert self.proc.stdin is not None
        self.proc.stdin.write(json.dumps(msg).encode() + b"\n")
        self.proc.stdin.flush()

    def rpc(self, method: str, params: JSON) -> JSON:
        with self.lock:
            self._id += 1
            want = self._id
            self._send({"jsonrpc": "2.0", "id": want, "method": method, "params": params})
            deadline = time.monotonic() + self.timeout_s
            while True:
                left = deadline - time.monotonic()
                if left <= 0:
                    raise ToolError(f"codegraph mcp timed out on {method}")
                try:
                    raw = self.lines.get(timeout=left)
                except queue.Empty:
                    continue
                try:
                    msg = json.loads(raw)
                except json.JSONDecodeError:
                    continue
                if msg.get("id") != want:
                    continue
                if "error" in msg:
                    raise ToolError(f"codegraph: {msg['error'].get('message', msg['error'])}")
                result: JSON = msg.get("result") or {}
                return result

    def call(self, name: str, args: JSON) -> str:
        res = self.rpc("tools/call", {"name": name, "arguments": args})
        text = "\n".join(str(c.get("text", "")) for c in res.get("content", []) if c.get("type") == "text")
        if res.get("isError"):
            raise ToolError(text or "codegraph tool error")
        return text

    def close(self) -> None:
        try:
            if self.proc.stdin:
                self.proc.stdin.close()
            self.proc.wait(timeout=5)
        except (OSError, subprocess.TimeoutExpired):
            self.proc.kill()


PATHISH = re.compile(r"^(?:[A-Za-z]:[\\/]|[\\/])")


def guard_codegraph_args(corpus: Corpus, args: JSON) -> None:
    """codegraph accepts file paths in `file`/`target`/`symbol` (outline even parses
    unindexed files). Absolute paths must stay inside the corpus; no `..` escapes."""
    for key in ("file", "target", "symbol", "repo"):
        v = args.get(key)
        if not isinstance(v, str):
            continue
        p = re.sub(r":\d+$", "", v.strip())
        if p.startswith(("\\\\", "//")):
            raise ToolError(f"refused {key} {v!r}: UNC paths are not allowed")
        if ".." in re.split(r"[\\/]", p):
            raise ToolError(f"refused {key} {v!r}: '..' is not allowed")
        if PATHISH.match(p):
            corpus.resolve(p)


# --------------------------------------------------------------------------- arms


SYSTEM = """You are a code-navigation agent. Answer questions about the source code in a \
local corpus of repositories under {root}. Repositories (one directory each): {repos}.

{tools_blurb}

Paths are corpus-relative with forward slashes, e.g. "chi/mux.go". Investigate with the \
tools until you are confident, then reply with the final answer only: a short list of the \
requested names, one per line (file paths as repo-relative paths with forward slashes, \
e.g. "internal/rules/engine.go"). No explanations beyond a few words per line."""

BLURB = {
    "grep": "Tools: `grep` (ripgrep regex search over file contents), `read_file` (line "
    "ranges) and `list_files` (glob).",
    "codegraph": "Tools: the codegraph tools (a precomputed tree-sitter symbol + call graph "
    "of these repos: find/def/refs/callers/callees/outline/impact/status; the `repo` "
    'argument takes the directory name, e.g. "chi"), plus `read_file` and `list_files`. '
    "There is no text search tool.",
}


@dataclass
class Arm:
    name: str
    tools: list[JSON]  # OpenAI-style {name, description, parameters}
    system: str


def build_arms(s: Settings, mcp: McpClient | None) -> dict[str, Arm]:
    repos = ", ".join(s.corpus.repos())
    arms: dict[str, Arm] = {}
    for name in ARMS:
        if name == "codegraph" and mcp is None:
            continue
        tools = (
            GREP_TOOLS + FILE_TOOLS
            if name == "grep"
            else [
                {
                    "name": t["name"],
                    "description": t.get("description", ""),
                    "parameters": t.get("inputSchema", {"type": "object"}),
                }
                for t in (mcp.tools if mcp else [])
            ]
            + FILE_TOOLS
        )
        system = SYSTEM.format(root=s.corpus.root, repos=repos, tools_blurb=BLURB[name])
        arms[name] = Arm(name, tools, system)
    return arms


def run_tool(s: Settings, mcp: McpClient | None, arm: Arm, name: str, args: JSON) -> str:
    allowed = {t["name"] for t in arm.tools}
    if name not in allowed:
        raise ToolError(f"unknown tool {name!r} for this agent; available: {', '.join(sorted(allowed))}")
    if name == "grep":
        return tool_grep(s, args)
    if name == "read_file":
        return tool_read_file(s, args)
    if name == "list_files":
        return tool_list_files(s, args)
    if mcp is None:
        raise ToolError("codegraph is not available")
    guard_codegraph_args(s.corpus, args)
    text, _ = cap(mcp.call(name, args), s.max_tool_chars, "narrow the query (repo, kind, depth)")
    return text


# --------------------------------------------------------------------------- upstream


@dataclass
class Usage:
    input: int = 0  # all input tokens incl. cache reads/writes
    output: int = 0
    cache_read: int = 0
    cache_write: int = 0

    def add(self, o: Usage) -> None:
        self.input += o.input
        self.output += o.output
        self.cache_read += o.cache_read
        self.cache_write += o.cache_write


@dataclass
class Prices:
    input: float = 1.0
    output: float = 5.0
    cache_read: float = 0.1

    def cost(self, u: Usage) -> float:
        uncached = max(0, u.input - u.cache_read - u.cache_write)
        return (
            uncached * self.input
            + u.cache_read * self.cache_read
            + u.cache_write * self.input * 1.25
            + u.output * self.output
        ) / 1e6


@dataclass
class Turn:
    text: str
    calls: list[tuple[str, str, JSON]]  # (id, name, args)
    usage: Usage


class UpstreamError(Exception):
    pass


class Upstream:
    def __init__(self, url: str, kind: str, model: str, key: str | None, timeout_s: float, max_tokens: int) -> None:
        self.url = url.rstrip("/")
        self.kind = kind
        self.model = model
        self.key = key
        self.timeout_s = timeout_s
        self.max_tokens = max_tokens

    def model_for(self, arm: str) -> str:
        return self.model.replace("{arm}", arm)

    retries: int = 4

    def _post(self, path: str, body: JSON, headers: dict[str, str]) -> JSON:
        """POST with a small retry on 429/5xx/529 (honours retry-after). evalrig runs
        with max_retries=0, so the whole agent loop is never re-run for a transient
        upstream error; only the one upstream turn is retried here."""
        data_bytes = json.dumps(body).encode()
        for attempt in range(self.retries + 1):
            req = urllib.request.Request(
                self.url + path,
                data=data_bytes,
                method="POST",
                headers={"Content-Type": "application/json", **headers},
            )
            try:
                with urllib.request.urlopen(req, timeout=self.timeout_s) as resp:  # noqa: S310
                    data: JSON = json.loads(resp.read())
            except urllib.error.HTTPError as exc:
                detail = exc.read()[:500].decode(errors="replace")
                if (exc.code == 429 or exc.code >= 500) and attempt < self.retries:
                    ra = exc.headers.get("retry-after") if exc.headers else None
                    try:
                        delay = float(ra) if ra else 2.0 * (2**attempt)
                    except ValueError:
                        delay = 2.0 * (2**attempt)
                    time.sleep(min(delay, 60.0))
                    continue
                raise UpstreamError(f"upstream HTTP {exc.code}: {detail}") from None
            except (urllib.error.URLError, TimeoutError, OSError) as exc:
                raise UpstreamError(f"upstream unreachable: {exc}") from None
            if isinstance(data.get("error"), dict):
                raise UpstreamError(f"upstream error: {data['error']}")
            return data
        raise UpstreamError("upstream: retries exhausted")

    def chat(self, arm: Arm, messages: list[JSON], with_tools: bool) -> Turn:
        if self.kind == "anthropic":
            return self._anthropic(arm, messages, with_tools)
        return self._openai(arm, messages, with_tools)

    def _openai(self, arm: Arm, messages: list[JSON], with_tools: bool) -> Turn:
        body: JSON = {
            "model": self.model_for(arm.name),
            "messages": [{"role": "system", "content": arm.system}, *messages],
            "max_tokens": self.max_tokens,
            "temperature": 0,
        }
        if with_tools:
            body["tools"] = [{"type": "function", "function": t} for t in arm.tools]
        headers = {"Authorization": f"Bearer {self.key}"} if self.key else {}
        data = self._post("/chat/completions", body, headers)
        choices = data.get("choices") or []
        if not choices:
            raise UpstreamError("upstream returned no choices")
        msg = choices[0].get("message") or {}
        calls = []
        for i, tc in enumerate(msg.get("tool_calls") or []):
            fn = tc.get("function") or {}
            raw = fn.get("arguments") or "{}"
            try:
                args = json.loads(raw) if isinstance(raw, str) else dict(raw)
            except (json.JSONDecodeError, TypeError, ValueError):
                args = {"_invalid_json": str(raw)[:200]}
            calls.append((str(tc.get("id") or f"call_{i}"), str(fn.get("name", "")), args))
        u = data.get("usage") or {}
        cached = int((u.get("prompt_tokens_details") or {}).get("cached_tokens") or 0)
        usage = Usage(int(u.get("prompt_tokens") or 0), int(u.get("completion_tokens") or 0), cached)
        return Turn(str(msg.get("content") or ""), calls, usage)

    def _anthropic(self, arm: Arm, messages: list[JSON], with_tools: bool) -> Turn:
        conv: list[JSON] = []
        for m in messages:
            if m["role"] == "user":
                conv.append({"role": "user", "content": [{"type": "text", "text": m["content"]}]})
            elif m["role"] == "assistant":
                blocks: list[JSON] = []
                if m.get("content"):
                    blocks.append({"type": "text", "text": m["content"]})
                for tc in m.get("tool_calls") or []:
                    blocks.append(
                        {
                            "type": "tool_use",
                            "id": tc["id"],
                            "name": tc["function"]["name"],
                            "input": json.loads(tc["function"]["arguments"]),
                        }
                    )
                conv.append({"role": "assistant", "content": blocks})
            elif m["role"] == "tool":
                block = {"type": "tool_result", "tool_use_id": m["tool_call_id"], "content": m["content"]}
                if conv and conv[-1]["role"] == "user" and conv[-1]["content"][0]["type"] == "tool_result":
                    conv[-1]["content"].append(block)  # parallel results share one user turn
                else:
                    conv.append({"role": "user", "content": [block]})
        body: JSON = {
            "model": self.model_for(arm.name),
            "max_tokens": self.max_tokens,
            "system": [{"type": "text", "text": arm.system, "cache_control": {"type": "ephemeral"}}],
            "messages": conv,
            "temperature": 0,
        }
        if with_tools:
            tools = [
                {"name": t["name"], "description": t["description"], "input_schema": t["parameters"]} for t in arm.tools
            ]
            tools[-1]["cache_control"] = {"type": "ephemeral"}
            body["tools"] = tools
        headers = {"anthropic-version": "2023-06-01"}
        if self.key:
            headers["x-api-key"] = self.key
        data = self._post("/messages", body, headers)
        text = "".join(b.get("text", "") for b in data.get("content", []) if b.get("type") == "text")
        calls = [
            (str(b["id"]), str(b["name"]), dict(b.get("input") or {}))
            for b in data.get("content", [])
            if b.get("type") == "tool_use"
        ]
        u = data.get("usage") or {}
        cr = int(u.get("cache_read_input_tokens") or 0)
        cw = int(u.get("cache_creation_input_tokens") or 0)
        usage = Usage(int(u.get("input_tokens") or 0) + cr + cw, int(u.get("output_tokens") or 0), cr, cw)
        return Turn(text, calls, usage)


# --------------------------------------------------------------------------- budget + traces


class LedgerError(Exception):
    """The spend ledger (or the budget) is unusable; the proxy refuses to start."""


def _finite_usd(v: object, what: str) -> float:
    if isinstance(v, bool) or not isinstance(v, (int, float)):
        raise LedgerError(f"{what} must be a number, got {v!r}")
    f = float(v)
    if not math.isfinite(f) or f < 0:
        raise LedgerError(f"{what} must be a finite, non-negative number, got {v!r}")
    return f


class Ledger:
    """Cumulative upstream spend, persisted so restarts cannot reset the cap.

    Fails closed: an unreadable/corrupt ledger or a non-finite value refuses to load
    (never silently resets to 0). Each upstream turn first reserves an estimated cost
    under the lock (so concurrent requests cannot all pass the check and overshoot),
    then settles to the actual cost. The file is rewritten atomically (temp file +
    os.replace) and records spent + outstanding reservations, so a crash mid-turn
    over-counts rather than under-counts."""

    def __init__(self, path: Path, budget_usd: float | None, prices: Prices) -> None:
        self.path = path
        self.budget = None if budget_usd is None else _finite_usd(budget_usd, "budget (CGAGENT_BUDGET_USD)")
        for name in ("input", "output", "cache_read"):
            _finite_usd(getattr(prices, name), f"price {name}")
        self.prices = prices
        self.lock = threading.Lock()
        self.spent = 0.0
        self.reserved = 0.0
        if path.exists():
            self.spent = self._load(path)

    @staticmethod
    def _load(path: Path) -> float:
        reset = (
            f"inspect it, or if you are sure of the real spend, delete {path} "
            "(or point CGAGENT_LEDGER at a new file) to reset the ledger to $0"
        )
        try:
            data = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, UnicodeDecodeError, ValueError) as exc:
            raise LedgerError(f"spend ledger {path} is unreadable ({exc}); {reset}") from None
        if not isinstance(data, dict) or "spent_usd" not in data:
            raise LedgerError(f"spend ledger {path} has no spent_usd field; {reset}")
        try:
            return _finite_usd(data["spent_usd"], f"spend ledger {path} spent_usd")
        except LedgerError as exc:
            raise LedgerError(f"{exc}; {reset}") from None

    def _persist(self) -> None:
        """Atomic write of spent + outstanding reservations. Caller holds the lock."""
        self.path.parent.mkdir(parents=True, exist_ok=True)
        fd, tmp = tempfile.mkstemp(prefix=".spend-", suffix=".tmp", dir=self.path.parent)
        try:
            with os.fdopen(fd, "w", encoding="utf-8") as f:
                json.dump({"spent_usd": self.spent + self.reserved}, f)
                f.flush()
                os.fsync(f.fileno())
            os.replace(tmp, self.path)
        except BaseException:
            with contextlib.suppress(OSError):
                os.unlink(tmp)
            raise

    def _refuse(self, extra: float) -> None:
        if self.budget is not None and self.spent + self.reserved + extra > self.budget:
            raise BudgetExceeded(
                f"budget exhausted: spent ${self.spent:.4f} (+${self.reserved:.4f} in flight, "
                f"next turn est. ${extra:.4f}) of ${self.budget:.2f}"
            )

    def check(self) -> None:
        with self.lock:
            if self.budget is not None and self.spent + self.reserved >= self.budget:
                raise BudgetExceeded(f"budget exhausted: spent ${self.spent:.4f} of ${self.budget:.2f}")

    def reserve(self, estimate: Usage) -> float:
        """Reserve the estimated cost of one upstream turn; BudgetExceeded if it would
        not fit. Returns the reserved amount, to pass to settle()/release()."""
        est = _finite_usd(self.prices.cost(estimate), "estimated turn cost")
        with self.lock:
            self._refuse(est)
            self.reserved += est
            self._persist()
        return est

    def release(self, reserved: float) -> None:
        with self.lock:
            self.reserved = max(0.0, self.reserved - reserved)
            self._persist()

    def settle(self, reserved: float, u: Usage) -> float:
        c = _finite_usd(self.prices.cost(u), "turn cost")
        with self.lock:
            self.reserved = max(0.0, self.reserved - reserved)
            self.spent += c
            self._persist()
        return c

    def charge(self, u: Usage) -> float:
        return self.settle(0.0, u)


class BudgetExceeded(Exception):
    pass


class Tracer:
    def __init__(self, path: Path) -> None:
        self.path = path
        self.lock = threading.Lock()
        path.parent.mkdir(parents=True, exist_ok=True)

    def write(self, rec: JSON) -> None:
        line = json.dumps(rec, ensure_ascii=False)
        with self.lock, open(self.path, "a", encoding="utf-8") as f:
            f.write(line + "\n")


# --------------------------------------------------------------------------- agent loop

CASE_ID = re.compile(r"\[(Q\d+)\]")


@dataclass
class Agent:
    settings: Settings
    upstream: Upstream
    arms: dict[str, Arm]
    ledger: Ledger
    tracer: Tracer
    mcp: McpClient | None = None
    max_turns: int = 12
    served: int = field(default=0)

    def estimate(self, arm: Arm, messages: list[JSON], with_tools: bool) -> Usage:
        """Upper-ish bound on one turn's usage for the budget reservation: ~3 chars per
        input token (tokenizers average ~4), all uncached, plus the full max_tokens."""
        chars = len(arm.system) + len(json.dumps(messages))
        if with_tools:
            chars += len(json.dumps(arm.tools))
        return Usage(input=chars // 3 + 1, output=self.upstream.max_tokens)

    def run(self, arm_name: str, question: str) -> JSON:
        arm = self.arms.get(arm_name)
        if arm is None:
            raise KeyError(arm_name)
        m = CASE_ID.search(question)
        rec: JSON = {
            "ts": time.strftime("%Y-%m-%dT%H:%M:%S"),
            "arm": arm_name,
            "case": m.group(1) if m else None,
            "upstream_model": self.upstream.model_for(arm_name),
            "tool_calls": [],
            "turns": 0,
        }
        messages: list[JSON] = [{"role": "user", "content": question}]
        total = Usage()
        final, stop = "", "turn_cap"
        started = time.monotonic()
        try:
            for turn in range(self.max_turns):
                last = turn == self.max_turns - 1
                if last:
                    messages.append({"role": "user", "content": "Turn limit reached. Give your final answer now."})
                held = self.ledger.reserve(self.estimate(arm, messages, with_tools=not last))
                try:
                    t = self.upstream.chat(arm, messages, with_tools=not last)
                except BaseException:
                    self.ledger.release(held)
                    raise
                self.ledger.settle(held, t.usage)
                rec["turns"] = turn + 1
                total.add(t.usage)
                if not t.calls:
                    final, stop = t.text, "answer"
                    break
                messages.append(
                    {
                        "role": "assistant",
                        "content": t.text or None,
                        "tool_calls": [
                            {"id": cid, "type": "function", "function": {"name": name, "arguments": json.dumps(args)}}
                            for cid, name, args in t.calls
                        ],
                    }
                )
                for cid, name, args in t.calls:
                    t0 = time.monotonic()
                    err = False
                    try:
                        out = run_tool(self.settings, self.mcp, arm, name, args)
                    except ToolError as exc:
                        out, err = f"error: {exc}", True
                    except (OSError, subprocess.SubprocessError) as exc:
                        out, err = f"error: tool failed: {exc}", True
                    content = TOOL_MARK.format(name=name) + "\n" + out
                    messages.append({"role": "tool", "tool_call_id": cid, "content": content})
                    rec["tool_calls"].append(
                        {
                            "turn": turn + 1,
                            "name": name,
                            "args": args,
                            "error": err,
                            "result_chars": len(content),
                            "result_tokens_est": len(content) // 4,
                            "truncated": "[truncated:" in out,
                            "ms": round((time.monotonic() - t0) * 1000),
                        }
                    )
                final = t.text
        except BudgetExceeded as exc:
            stop, final = "budget", ""
            rec["error"] = str(exc)
        except UpstreamError as exc:
            stop, final = "upstream_error", ""
            rec["error"] = str(exc)
        rec.update(
            {
                "stop": stop,
                "final": final,
                "usage": {
                    "input": total.input,
                    "output": total.output,
                    "cache_read": total.cache_read,
                    "cache_write": total.cache_write,
                },
                "cost_usd": round(self.ledger.prices.cost(total), 6),
                "ms": round((time.monotonic() - started) * 1000),
            }
        )
        self.tracer.write(rec)
        return rec


# --------------------------------------------------------------------------- HTTP


def make_handler(agent: Agent, port_ref: list[int]) -> type[BaseHTTPRequestHandler]:
    class Handler(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def log_message(self, fmt: str, *args: Any) -> None:
            pass

        def _send(self, status: int, body: JSON) -> None:
            data = json.dumps(body).encode()
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)

        def _err(self, status: int, etype: str, msg: str) -> None:
            self._send(status, {"error": {"type": etype, "message": msg}})

        def _allowed(self) -> bool:
            host = (self.headers.get("Host") or "").lower()
            port = port_ref[0]
            ok_hosts = {f"127.0.0.1:{port}", f"localhost:{port}", f"[::1]:{port}"}
            if host not in ok_hosts:
                self._err(403, "forbidden", "Host not allowed (loopback only)")
                return False
            if self.headers.get("Origin") or self.headers.get("Sec-Fetch-Site", "none") not in ("none", ""):
                self._err(403, "forbidden", "browser requests are refused")
                return False
            return True

        def do_GET(self) -> None:
            if not self._allowed():
                return
            if self.path.split("?")[0].rstrip("/") in ("/v1/models", "/models"):
                self._send(
                    200,
                    {
                        "object": "list",
                        "data": [{"id": a, "object": "model", "owned_by": "cgagent"} for a in agent.arms],
                    },
                )
                return
            self._err(404, "not_found", self.path)

        def do_POST(self) -> None:
            if not self._allowed():
                return
            if self.path.split("?")[0].rstrip("/") not in ("/v1/chat/completions", "/chat/completions"):
                self._err(404, "not_found", self.path)
                return
            n = int(self.headers.get("Content-Length") or 0)
            if n <= 0 or n > MAX_BODY:
                self._err(400, "invalid_request", "missing or oversized body")
                return
            try:
                body = json.loads(self.rfile.read(n))
            except json.JSONDecodeError:
                self._err(400, "invalid_request", "invalid JSON")
                return
            arm = str(body.get("model", ""))
            if arm not in agent.arms:
                self._err(404, "model_not_found", f"unknown arm {arm!r}; one of {sorted(agent.arms)}")
                return
            msgs = body.get("messages") or []
            question = "\n\n".join(
                str(m.get("content") or "") for m in msgs if m.get("role") in ("system", "user")
            ).strip()
            if not question:
                self._err(400, "invalid_request", "no user message")
                return
            rec = agent.run(arm, question)
            if rec["stop"] == "budget":
                # 400, not 429/5xx: evalrig must not retry a request the cap refused.
                self._err(400, "budget_exceeded", rec.get("error", "budget exhausted"))
                return
            if rec["stop"] == "upstream_error":
                self._err(502, "upstream_error", rec.get("error", "upstream failed"))
                return
            u = rec["usage"]
            self._send(
                200,
                {
                    "id": f"cgagent-{rec['arm']}-{rec['case'] or 'x'}-{int(time.time() * 1000)}",
                    "object": "chat.completion",
                    "created": int(time.time()),
                    "model": arm,
                    "choices": [
                        {"index": 0, "finish_reason": "stop", "message": {"role": "assistant", "content": rec["final"]}}
                    ],
                    "usage": {
                        "prompt_tokens": u["input"],
                        "completion_tokens": u["output"],
                        "total_tokens": u["input"] + u["output"],
                        "prompt_tokens_details": {"cached_tokens": u["cache_read"]},
                    },
                    "cgagent": {
                        "turns": rec["turns"],
                        "tool_calls": len(rec["tool_calls"]),
                        "stop": rec["stop"],
                        "cost_usd": rec["cost_usd"],
                    },
                },
            )

    return Handler


def serve(agent: Agent, port: int) -> ThreadingHTTPServer:
    port_ref = [port]
    server = ThreadingHTTPServer(("127.0.0.1", port), make_handler(agent, port_ref))
    server.daemon_threads = True
    port_ref[0] = server.server_address[1]
    return server


def kit_env(state: Path) -> dict[str, str]:
    env = dict(os.environ)
    env["KIT_CONFIG_DIR"] = str(state / "kit-config")
    env["KIT_DATA_DIR"] = str(state / "kit-data")
    env["NO_COLOR"] = "1"
    env["CLICOLOR_FORCE"] = "0"
    return env


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description="cgagent: OpenAI-compatible agent proxy")
    ap.add_argument("--port", type=int, default=int(os.environ.get("CGAGENT_PORT", "8791")))
    ap.add_argument("--corpus", default=os.environ.get("CGAGENT_CORPUS"), required=False)
    ap.add_argument(
        "--codegraph",
        default=os.environ.get("CGAGENT_CODEGRAPH", str(REPO_ROOT / "target" / "release" / "codegraph.exe")),
    )
    ap.add_argument("--state-dir", default=str(DEFAULT_STATE))
    ap.add_argument("--trace", default=os.environ.get("CGAGENT_TRACE"))
    ap.add_argument("--max-turns", type=int, default=int(os.environ.get("CGAGENT_MAX_TURNS", "12")))
    ap.add_argument("--max-tool-chars", type=int, default=int(os.environ.get("CGAGENT_MAX_TOOL_CHARS", "12000")))
    ap.add_argument("--no-codegraph", action="store_true", help="serve only the grep arm")
    ap.add_argument("--index", action="store_true", help="run `codegraph index <corpus>` first")
    args = ap.parse_args(argv)
    if not args.corpus:
        ap.error("--corpus (or CGAGENT_CORPUS) is required")

    state = Path(args.state_dir)
    corpus = Corpus(Path(args.corpus))
    settings = Settings(corpus=corpus, max_tool_chars=args.max_tool_chars)
    env = kit_env(state)
    mcp = None
    if not args.no_codegraph:
        exe = Path(args.codegraph)
        if not exe.is_file():
            print(f"codegraph binary not found: {exe} (cargo build --release)", file=sys.stderr)
            return 1
        Path(env["KIT_CONFIG_DIR"]).mkdir(parents=True, exist_ok=True)
        Path(env["KIT_DATA_DIR"]).mkdir(parents=True, exist_ok=True)
        if args.index:
            subprocess.run([str(exe), "index", str(corpus.root)], env=env, check=True, stdout=subprocess.DEVNULL)
        mcp = McpClient(exe, corpus.root, env)

    key_env = os.environ.get("CGAGENT_UPSTREAM_KEY_ENV")
    key = os.environ.get(key_env) if key_env else None
    upstream = Upstream(
        url=os.environ.get("CGAGENT_UPSTREAM_URL", "http://127.0.0.1:8765/v1"),
        kind=os.environ.get("CGAGENT_UPSTREAM_KIND", "openai"),
        model=os.environ.get("CGAGENT_UPSTREAM_MODEL", "fake-{arm}"),
        key=key,
        timeout_s=float(os.environ.get("CGAGENT_UPSTREAM_TIMEOUT_S", "120")),
        max_tokens=int(os.environ.get("CGAGENT_MAX_TOKENS", "2048")),
    )
    ledger_path = Path(os.environ.get("CGAGENT_LEDGER", str(state / "agent" / "spend.json")))
    try:
        budget = os.environ.get("CGAGENT_BUDGET_USD")
        prices = Prices(
            float(os.environ.get("CGAGENT_PRICE_IN", "1.0")),
            float(os.environ.get("CGAGENT_PRICE_OUT", "5.0")),
            float(os.environ.get("CGAGENT_PRICE_CACHE_READ", "0.1")),
        )
        ledger = Ledger(ledger_path, float(budget) if budget else None, prices)
    except (LedgerError, ValueError) as exc:
        print(f"cgagent: refusing to start: {exc}", file=sys.stderr)
        if mcp:
            mcp.close()
        return 2
    trace = Path(args.trace) if args.trace else state / "traces" / f"{time.strftime('%Y%m%d-%H%M%S')}.jsonl"
    arms = build_arms(settings, mcp)
    state.mkdir(parents=True, exist_ok=True)
    (state / "arms.json").write_text(  # what each arm is shown (for schema-size accounting)
        json.dumps({a.name: {"system": a.system, "tools": a.tools} for a in arms.values()}),
        encoding="utf-8",
    )
    agent = Agent(settings, upstream, arms, ledger, Tracer(trace), mcp, max_turns=args.max_turns)
    server = serve(agent, args.port)
    print(
        f"cgagent listening on http://127.0.0.1:{server.server_address[1]}/v1 "
        f"arms={','.join(agent.arms)} upstream={upstream.kind}:{upstream.url} trace={trace}",
        flush=True,
    )
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()
        if mcp:
            mcp.close()
    return 0


if __name__ == "__main__":
    sys.exit(main())

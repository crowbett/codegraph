"""Single source of truth for the agent-eval questions.

`gen.py` renders suite.yaml (evalrig) and fake-rules.json (evalrig's fake_llm) from
CASES; `truth.py check` re-verifies the SCIP-checkable expectations.

Each case:
  id, lang, repo, kind (question type), question
  expect  - names/paths the answer must contain (evalrig `contains`, mode all)
  answer  - the scripted final answer used in fake mode (the verified correct answer)
  truth   - how it was verified: {"kind": callers|callees|defs|ref_files, "symbol": SCIP
            symbol suffix, "expect": [...], "range": [...]} for SCIP-checkable cases, or
            {"kind": "source", "evidence": "file:line ..."} for cases verified by reading.
  grep / codegraph - scripted tool steps for fake mode: a list of turns, each a list of
            [tool, args] calls, i.e. what a competent agent with that arm's tools would do.
"""

from __future__ import annotations

from typing import Any

Case = dict[str, Any]

CASES: list[Case] = [
    # ------------------------------------------------------------------ Go: chi
    {
        "id": "Q01",
        "lang": "go",
        "repo": "chi",
        "kind": "callers",
        "question": "In the chi repo, which functions or methods call `patNextSegment`?",
        "expect": ["InsertRoute", "addChild", "patParamKeys"],
        "answer": "node.InsertRoute\nnode.addChild\npatParamKeys",
        "truth": {
            "kind": "callers",
            "symbol": "/patNextSegment().",
            "expect": ["InsertRoute", "addChild", "patParamKeys"],
            "exact": True,
        },
        "grep": [
            [["grep", {"pattern": r"patNextSegment\(", "repo": "chi", "glob": "*.go"}]],
            [
                ["read_file", {"path": "chi/tree.go", "start_line": 140, "end_line": 260}],
                ["read_file", {"path": "chi/tree.go", "start_line": 795, "end_line": 812}],
            ],
        ],
        "codegraph": [
            [["codegraph_callers", {"symbol": "patNextSegment", "repo": "chi", "depth": 1}]],
        ],
    },
    {
        "id": "Q02",
        "lang": "go",
        "repo": "chi",
        "kind": "implementations",
        "question": "In chi's middleware package, which concrete types implement the `WrapResponseWriter` interface?",
        "expect": [
            "basicWriter",
            "flushWriter",
            "hijackWriter",
            "flushHijackWriter",
            "httpFancyWriter",
            "http2FancyWriter",
        ],
        "answer": "basicWriter\nflushWriter\nhijackWriter\nflushHijackWriter\nhttpFancyWriter\nhttp2FancyWriter",
        "truth": {
            "kind": "source",
            "evidence": "chi/middleware/wrap_writer.go:48 interface (Status/BytesWritten/Tee/Unwrap/Discard); "
            "basicWriter implements all at :130-146; flushWriter/hijackWriter/flushHijackWriter/"
            "httpFancyWriter/http2FancyWriter embed basicWriter (:152,:163,:175,:195,:238); "
            "compressResponseWriter only has Unwrap (compress.go:394)",
        },
        "grep": [
            [
                [
                    "grep",
                    {
                        "pattern": r"^type \w+ struct|^func \(\w+ \*?\w+\) (Status|BytesWritten|Tee|Unwrap|Discard)\(",
                        "repo": "chi",
                        "glob": "middleware/*.go",
                    },
                ]
            ],
            [["read_file", {"path": "chi/middleware/wrap_writer.go", "start_line": 145, "end_line": 245}]],
        ],
        "codegraph": [
            [["codegraph_refs", {"symbol": "middleware.WrapResponseWriter", "repo": "chi", "kind": "impl"}]],
        ],
    },
    {
        "id": "Q03",
        "lang": "go",
        "repo": "chi",
        "kind": "methods",
        "question": "List the methods defined on chi's routing `Context` type (the struct in the chi package).",
        "expect": ["Reset", "Clone", "URLParam", "RoutePattern"],
        "answer": "Context.Reset\nContext.Clone\nContext.URLParam\nContext.RoutePattern",
        "truth": {
            "kind": "source",
            "evidence": "chi/context.go:82,104,124,147 are the only `func (x *Context)` receivers in the repo",
        },
        "grep": [
            [["grep", {"pattern": r"^func \(\w+ \*?Context\) ", "repo": "chi", "glob": "*.go"}]],
        ],
        "codegraph": [
            [["codegraph_find", {"query": "Context", "repo": "chi", "kind": "struct"}]],
            [["codegraph_outline", {"file": "chi/context.go"}]],
        ],
    },
    # ------------------------------------------------------------------ Go: pulse
    {
        "id": "Q04",
        "lang": "go",
        "repo": "pulse",
        "kind": "callers",
        "question": "In pulse, which functions or methods call the engine's `enabledRule` method?",
        "expect": ["Trigger", "Webhook", "chain"],
        "answer": "Engine.Trigger\nEngine.Webhook\nEngine.chain",
        "truth": {
            "kind": "callers",
            "symbol": "#enabledRule().",
            "expect": ["Trigger", "Webhook", "chain"],
            "exact": True,
        },
        "grep": [
            [["grep", {"pattern": r"enabledRule\(", "repo": "pulse"}]],
            [
                ["read_file", {"path": "pulse/internal/engine/engine.go", "start_line": 240, "end_line": 300}],
                ["read_file", {"path": "pulse/internal/engine/status.go", "start_line": 170, "end_line": 192}],
            ],
        ],
        "codegraph": [
            [["codegraph_callers", {"symbol": "enabledRule", "repo": "pulse", "depth": 1}]],
        ],
    },
    {
        "id": "Q05",
        "lang": "go",
        "repo": "pulse",
        "kind": "callees",
        "question": "In pulse, what functions and methods defined in the pulse module does "
        "`Engine.dispatch` call directly? Ignore the standard library.",
        "expect": ["IsEnabled", "Render", "Truthy", "Publish", "Summary", "worker"],
        "answer": "config.Config.Rule\nconfig.Rule.IsEnabled\nevent.Render\nevent.Truthy\n"
        "bus.Bus.Publish\nevent.Event.Summary\nEngine.worker (in the workers.Go closure)",
        "truth": {
            "kind": "callees",
            "symbol": "Engine.dispatch",
            "range": ["internal/engine/engine.go", 336, 391],
            "expect": ["Rule", "IsEnabled", "Render", "Truthy", "Publish", "Summary", "worker"],
        },
        "grep": [
            [["grep", {"pattern": r"func \(e \*Engine\) dispatch\(", "repo": "pulse"}]],
            [["read_file", {"path": "pulse/internal/engine/engine.go", "start_line": 336, "end_line": 392}]],
        ],
        "codegraph": [
            [["codegraph_callees", {"symbol": "Engine.dispatch", "repo": "pulse", "depth": 1}]],
        ],
    },
    {
        "id": "Q06",
        "lang": "go",
        "repo": "pulse",
        "kind": "impact",
        "question": "In pulse, if `config.IsLoopbackHost` changes its behavior, which functions "
        "or methods depend on it directly (call it)?",
        "expect": ["Listen", "loopbackHost", "server"],
        "answer": "api.Server.Listen\napi.loopbackHost\nconfig.validator.server",
        "truth": {
            "kind": "callers",
            "symbol": "/IsLoopbackHost().",
            "expect": ["Listen", "loopbackHost", "server"],
            "exact": True,
        },
        "grep": [
            [["grep", {"pattern": r"IsLoopbackHost\(", "repo": "pulse"}]],
            [
                ["read_file", {"path": "pulse/internal/api/server.go", "start_line": 170, "end_line": 185}],
                ["read_file", {"path": "pulse/internal/api/server.go", "start_line": 390, "end_line": 402}],
                ["read_file", {"path": "pulse/internal/config/validate.go", "start_line": 84, "end_line": 98}],
            ],
        ],
        "codegraph": [
            [["codegraph_impact", {"target": "IsLoopbackHost", "repo": "pulse", "depth": 1}]],
        ],
    },
    # ------------------------------------------------------------------ Go: keep
    {
        "id": "Q07",
        "lang": "go",
        "repo": "keep",
        "kind": "callers",
        "question": "In keep's HTTP server, which functions or methods call `originAllowed`?",
        "expect": ["cors", "handleAdd", "handlePreflight", "tokenOK"],
        "answer": "Server.cors\nServer.handleAdd\nServer.handlePreflight\nServer.tokenOK",
        "truth": {
            "kind": "callers",
            "symbol": "/originAllowed().",
            "expect": ["cors", "handleAdd", "handlePreflight", "tokenOK"],
            "exact": True,
        },
        "grep": [
            [["grep", {"pattern": r"originAllowed\(", "repo": "keep"}]],
            [
                ["read_file", {"path": "keep/internal/server/server.go", "start_line": 238, "end_line": 250}],
                ["read_file", {"path": "keep/internal/server/server.go", "start_line": 478, "end_line": 506}],
            ],
        ],
        "codegraph": [
            [["codegraph_callers", {"symbol": "originAllowed", "repo": "keep", "depth": 1}]],
        ],
    },
    {
        "id": "Q08",
        "lang": "go",
        "repo": "keep",
        "kind": "semantic",
        "question": "In keep, find the function that decides whether an IP address is publicly "
        "routable (the core check of the SSRF guard). Name it and the non-test "
        "functions or methods that call it.",
        "expect": ["IsPublic", "CheckHost", "Control", "dial"],
        "answer": "netguard.IsPublic\ncallers: netguard.CheckHost\nnetguard.Control\nnetguard.Proxy.dial",
        "truth": {"kind": "callers", "symbol": "netguard`/IsPublic().", "expect": ["CheckHost", "Control", "dial"]},
        "grep": [
            [
                [
                    "grep",
                    {"pattern": r"func \w*(Public|Private|Routable|Blocked)\w*\(", "repo": "keep", "ignore_case": True},
                ]
            ],
            [["grep", {"pattern": r"IsPublic\(", "repo": "keep"}]],
            [
                ["read_file", {"path": "keep/internal/netguard/netguard.go", "start_line": 60, "end_line": 122}],
                ["read_file", {"path": "keep/internal/netguard/proxy.go", "start_line": 90, "end_line": 120}],
            ],
        ],
        "codegraph": [
            [["codegraph_find", {"query": "public", "repo": "keep", "kind": "fn"}]],
            [["codegraph_callers", {"symbol": "netguard.IsPublic", "repo": "keep", "depth": 1}]],
        ],
    },
    {
        "id": "Q09",
        "lang": "go",
        "repo": "keep",
        "kind": "definition",
        "question": "In keep, in which file is the function `splitTags` defined? Give the repo-relative path.",
        "expect": ["internal/importer/importer.go"],
        "answer": "internal/importer/importer.go",
        "truth": {
            "kind": "defs",
            "symbol": "/splitTags().",
            "expect": ["internal/importer/importer.go"],
            "exact": True,
        },
        "grep": [
            [["grep", {"pattern": r"func splitTags\(", "repo": "keep"}]],
        ],
        "codegraph": [
            [["codegraph_find", {"query": "splitTags", "repo": "keep"}]],
        ],
    },
    # ------------------------------------------------------------------ Rust: fd
    {
        "id": "Q10",
        "lang": "rust",
        "repo": "fd",
        "kind": "callers",
        "question": "In fd, which functions call `print_trailing_slash`?",
        "expect": ["print_entry_colorized", "print_entry_uncolorized", "print_entry_uncolorized_base"],
        "answer": "print_entry_colorized\nprint_entry_uncolorized\nprint_entry_uncolorized_base",
        "truth": {
            "kind": "callers",
            "symbol": "/print_trailing_slash().",
            "expect": ["print_entry_colorized", "print_entry_uncolorized", "print_entry_uncolorized_base"],
            "exact": True,
        },
        "grep": [
            [["grep", {"pattern": r"print_trailing_slash\(", "repo": "fd"}]],
            [["read_file", {"path": "fd/src/output.rs", "start_line": 85, "end_line": 200}]],
        ],
        "codegraph": [
            [["codegraph_callers", {"symbol": "print_trailing_slash", "repo": "fd", "depth": 1}]],
        ],
    },
    {
        "id": "Q11",
        "lang": "rust",
        "repo": "fd",
        "kind": "definition",
        "question": "In fd, in which file is `is_existing_directory` defined? Give the repo-relative path.",
        "expect": ["src/filesystem.rs"],
        "answer": "src/filesystem.rs",
        "truth": {
            "kind": "defs",
            "symbol": "/is_existing_directory().",
            "expect": ["src/filesystem.rs"],
            "exact": True,
        },
        "grep": [
            [["grep", {"pattern": r"fn is_existing_directory", "repo": "fd"}]],
        ],
        "codegraph": [
            [["codegraph_find", {"query": "is_existing_directory", "repo": "fd"}]],
        ],
    },
    {
        "id": "Q12",
        "lang": "rust",
        "repo": "fd",
        "kind": "implementations",
        "question": "In fd, which types implement the `SanitizeErr` trait?",
        "expect": ["&str", "io::Error", "ignore::Error"],
        "answer": "&str\nio::Error\nignore::Error",
        "truth": {
            "kind": "source",
            "evidence": "fd/src/error.rs:20,28,36 (`impl SanitizeErr for ...`); no other impls in the repo",
        },
        "grep": [
            [["grep", {"pattern": r"impl.*SanitizeErr for", "repo": "fd"}]],
        ],
        "codegraph": [
            [["codegraph_refs", {"symbol": "SanitizeErr", "repo": "fd", "kind": "impl"}]],
        ],
    },
    # ------------------------------------------------------------------ Rust: stash
    {
        "id": "Q13",
        "lang": "rust",
        "repo": "stash",
        "kind": "callers",
        "question": "In stash, which non-test functions or methods call `Repository::load_doc`?",
        "expect": ["load_index", "load_snapshot"],
        "answer": "Repository::load_index\nRepository::load_snapshot",
        "truth": {
            "kind": "callers",
            "symbol": "]load_doc().",
            "expect": ["load_index", "load_snapshot", "init_open_docs_and_keys"],
            "exact": True,
        },
        "grep": [
            [["grep", {"pattern": r"load_doc\(", "repo": "stash"}]],
            [["read_file", {"path": "stash/src/repo.rs", "start_line": 238, "end_line": 300}]],
        ],
        "codegraph": [
            [["codegraph_callers", {"symbol": "Repository::load_doc", "repo": "stash", "depth": 1}]],
        ],
    },
    {
        "id": "Q14",
        "lang": "rust",
        "repo": "stash",
        "kind": "implementations",
        "question": "In stash, which types implement the `Progress` trait (from src/progress.rs)?",
        "expect": ["NoProgress", "Bar"],
        "answer": "progress::NoProgress\ncli::output::Bar",
        "truth": {
            "kind": "source",
            "evidence": "stash/src/progress.rs:20 `impl Progress for NoProgress`; "
            "stash/src/cli/output.rs:277 `impl stash::progress::Progress for Bar`",
        },
        "grep": [
            [["grep", {"pattern": r"impl.*Progress for", "repo": "stash"}]],
        ],
        "codegraph": [
            [["codegraph_refs", {"symbol": "progress::Progress", "repo": "stash", "kind": "impl"}]],
        ],
    },
    {
        "id": "Q15",
        "lang": "rust",
        "repo": "stash",
        "kind": "callees",
        "question": "In stash, which functions or methods defined in the stash crate does "
        "`crypto::create_slot` call directly?",
        "expect": ["validate", "fill_random", "wrapping_key", "seal", "kind"],
        "answer": "KdfParams::validate\nfill_random\nwrapping_key\nseal\nSecret::kind",
        "truth": {
            "kind": "callees",
            "symbol": "create_slot",
            "range": ["src/crypto.rs", 289, 328],
            "expect": ["validate", "fill_random", "wrapping_key", "seal", "kind"],
        },
        "grep": [
            [["grep", {"pattern": r"fn create_slot", "repo": "stash"}]],
            [["read_file", {"path": "stash/src/crypto.rs", "start_line": 286, "end_line": 330}]],
        ],
        "codegraph": [
            [["codegraph_callees", {"symbol": "create_slot", "repo": "stash", "depth": 1}]],
        ],
    },
    # ------------------------------------------------------------------ Rust: recall
    {
        "id": "Q16",
        "lang": "rust",
        "repo": "recall",
        "kind": "implementations",
        "question": "In recall, which types implement the `Source` trait?",
        "expect": ["BrowserSource", "ClaudeSource", "FilesSource", "GitSource", "NotesSource"],
        "answer": "BrowserSource\nClaudeSource\nFilesSource\nGitSource\nNotesSource",
        "truth": {
            "kind": "source",
            "evidence": "recall/src/sources/{browser.rs:60,claude.rs:30,"
            "files.rs:44,git.rs:75,notes.rs:96} `impl Source for ...`",
        },
        "grep": [
            [["grep", {"pattern": r"impl Source for", "repo": "recall"}]],
        ],
        "codegraph": [
            [["codegraph_refs", {"symbol": "Source", "repo": "recall", "kind": "impl"}]],
        ],
    },
    {
        "id": "Q17",
        "lang": "rust",
        "repo": "recall",
        "kind": "semantic",
        "question": "In recall, which function strips injected `<system-reminder>` blocks from "
        "Claude Code transcripts, and which non-test function calls it?",
        "expect": ["strip_injected", "parse_transcript"],
        "answer": "strip_injected (src/sources/claude.rs)\ncalled by parse_transcript",
        "truth": {
            "kind": "callers",
            "symbol": "/strip_injected().",
            "expect": ["parse_transcript", "strips_reminders"],
            "exact": True,
        },
        "grep": [
            [["grep", {"pattern": "system-reminder", "repo": "recall", "glob": "*.rs"}]],
            [["grep", {"pattern": r"strip_injected\(", "repo": "recall"}]],
            [["read_file", {"path": "recall/src/sources/claude.rs", "start_line": 138, "end_line": 182}]],
        ],
        "codegraph": [
            [["codegraph_find", {"query": "reminder", "repo": "recall"}]],
            [["codegraph_callers", {"symbol": "strip_injected", "repo": "recall", "depth": 1}]],
        ],
    },
    {
        "id": "Q18",
        "lang": "rust",
        "repo": "recall",
        "kind": "callers",
        "question": "In recall, which functions or methods call `PathGuard::admits`? Name the "
        "method and the type it belongs to.",
        "expect": ["FilesSource", "NotesSource", "units"],
        "answer": "FilesSource::units (Source impl)\nNotesSource::units (Source impl)",
        "truth": {"kind": "callers", "symbol": "]admits().", "expect": ["units"], "exact": True},
        "grep": [
            [["grep", {"pattern": r"\.admits\(", "repo": "recall"}]],
            [
                ["read_file", {"path": "recall/src/sources/files.rs", "start_line": 40, "end_line": 112}],
                ["read_file", {"path": "recall/src/sources/notes.rs", "start_line": 96, "end_line": 135}],
            ],
        ],
        "codegraph": [
            [["codegraph_callers", {"symbol": "PathGuard::admits", "repo": "recall", "depth": 1}]],
        ],
    },
    # ------------------------------------------------------------------ TS: ky
    {
        "id": "Q19",
        "lang": "typescript",
        "repo": "ky",
        "kind": "implementations",
        "question": "In ky's library source (not tests), which classes extend `KyError`?",
        "expect": ["HTTPError", "NetworkError", "TimeoutError", "ResponseSizeError", "ForceRetryError"],
        "answer": "HTTPError\nNetworkError\nTimeoutError\nResponseSizeError\nForceRetryError",
        "truth": {
            "kind": "source",
            "evidence": "ky/source/errors/{HTTPError.ts:15,NetworkError.ts:11,"
            "TimeoutError.ts:7,ResponseSizeError.ts:20,ForceRetryError.ts:10} `extends KyError`; "
            "SchemaValidationError and NonError extend Error",
        },
        "grep": [
            [["grep", {"pattern": r"extends KyError", "repo": "ky", "glob": "source/**"}]],
        ],
        "codegraph": [
            [["codegraph_refs", {"symbol": "KyError", "repo": "ky", "kind": "inherit"}]],
        ],
    },
    {
        "id": "Q20",
        "lang": "typescript",
        "repo": "ky",
        "kind": "callers",
        "question": "In ky, which methods of the `Ky` class call `isRawNetworkError`?",
        "expect": ["raceBodyRead", "fetch"],
        "answer": "Ky.#raceBodyRead\nKy.#fetch",
        "truth": {"kind": "callers", "symbol": "isRawNetworkError().", "expect": ["raceBodyRead", "fetch"]},
        "grep": [
            [["grep", {"pattern": r"isRawNetworkError\(", "repo": "ky", "glob": "source/**"}]],
            [
                ["read_file", {"path": "ky/source/core/Ky.ts", "start_line": 765, "end_line": 800}],
                ["read_file", {"path": "ky/source/core/Ky.ts", "start_line": 1190, "end_line": 1236}],
            ],
        ],
        "codegraph": [
            [["codegraph_callers", {"symbol": "isRawNetworkError", "repo": "ky", "depth": 1}]],
        ],
    },
    {
        "id": "Q21",
        "lang": "typescript",
        "repo": "ky",
        "kind": "callees",
        "question": "In ky, which functions and methods defined in ky does the private method "
        "`Ky.#fetch` call directly?",
        "expect": [
            "findUnknownOptions",
            "normalizeRetryOptions",
            "withManagedSignal",
            "wrapRequestWithUploadProgress",
            "getEffectiveTimeout",
            "getRemainingTotalTimeout",
            "cancelResponseBody",
            "setResponseRequest",
            "throwIfAbortedByUser",
            "isRawNetworkError",
        ],
        "answer": "findUnknownOptions\nnormalizeRetryOptions\n#withManagedSignal\n#wrapRequestWithUploadProgress\n"
        "#getEffectiveTimeout\ntimeout\n#getRemainingTotalTimeout\n#cancelResponseBody\n"
        "#setResponseRequest\n#throwIfAbortedByUser\nisRawNetworkError",
        "truth": {
            "kind": "callees",
            "symbol": "Ky.#fetch",
            "range": ["source/core/Ky.ts", 1193, 1240],
            "expect": [
                "withManagedSignal",
                "wrapRequestWithUploadProgress",
                "getEffectiveTimeout",
                "timeout",
                "getRemainingTotalTimeout",
                "cancelResponseBody",
                "setResponseRequest",
                "throwIfAbortedByUser",
                "isRawNetworkError",
            ],
            "note": "findUnknownOptions/normalizeRetryOptions are arrow consts (SCIP symbol "
            "`name.`), read at Ky.ts:1194-1195",
        },
        "grep": [
            [["grep", {"pattern": r"async #fetch\(", "repo": "ky"}]],
            [["read_file", {"path": "ky/source/core/Ky.ts", "start_line": 1193, "end_line": 1241}]],
        ],
        "codegraph": [
            [["codegraph_callees", {"symbol": "Ky.#fetch", "repo": "ky", "depth": 1}]],
        ],
    },
    {
        "id": "Q22",
        "lang": "typescript",
        "repo": "ky",
        "kind": "definition",
        "question": "In ky, where is the standalone helper function `limitResponseSize` defined "
        "(not the private `Ky` method of the same name)? Give the repo-relative path.",
        "expect": ["source/utils/body.ts"],
        "answer": "source/utils/body.ts",
        "truth": {"kind": "defs", "symbol": "`body.ts`/limitResponseSize.", "expect": ["source/utils/body.ts"]},
        "grep": [
            [["grep", {"pattern": r"limitResponseSize", "repo": "ky", "glob": "source/**"}]],
        ],
        "codegraph": [
            [["codegraph_find", {"query": "limitResponseSize", "repo": "ky"}]],
        ],
    },
    # ------------------------------------------------------------------ TS: devkit
    {
        "id": "Q23",
        "lang": "typescript",
        "repo": "devkit",
        "kind": "callers",
        "question": "In devkit's JWT tool, which functions call the module's own `importKey` "
        "helper (not `SubtleCrypto.importKey`)?",
        "expect": ["signJwt", "verifyJwt"],
        "answer": "signJwt\nverifyJwt",
        "truth": {
            "kind": "callers",
            "symbol": "`jwt.ts`/importKey().",
            "expect": ["signJwt", "verifyJwt"],
            "exact": True,
        },
        "grep": [
            [["grep", {"pattern": r"importKey\(", "repo": "devkit"}]],
            [["read_file", {"path": "devkit/packages/core/src/tools/jwt.ts", "start_line": 345, "end_line": 400}]],
        ],
        "codegraph": [
            [["codegraph_callers", {"symbol": "importKey", "repo": "devkit", "depth": 1}]],
        ],
    },
    {
        "id": "Q24",
        "lang": "typescript",
        "repo": "devkit",
        "kind": "callers",
        "question": "In devkit's CLI package, which functions call `emitJson`?",
        "expect": ["cmdConfig", "cmdLs", "cmdTool", "cmdVersion"],
        "answer": "cmdConfig\ncmdLs\ncmdTool\ncmdVersion",
        "truth": {
            "kind": "callers",
            "symbol": "`main.ts`/emitJson().",
            "expect": ["cmdConfig", "cmdLs", "cmdTool", "cmdVersion"],
            "exact": True,
        },
        "grep": [
            [["grep", {"pattern": r"emitJson\(", "repo": "devkit"}]],
            [["read_file", {"path": "devkit/packages/cli/src/main.ts", "start_line": 140, "end_line": 222}]],
        ],
        "codegraph": [
            [["codegraph_callers", {"symbol": "emitJson", "repo": "devkit", "depth": 1}]],
        ],
    },
    {
        "id": "Q25",
        "lang": "typescript",
        "repo": "devkit",
        "kind": "semantic",
        "question": "In devkit, find the function that computes the WCAG contrast ratio between "
        "two colors. Give its name, its repo-relative file, and the function that calls it.",
        "expect": ["contrastRatio", "packages/core/src/tools/color.ts", "analyzeColor"],
        "answer": "contrastRatio\npackages/core/src/tools/color.ts\ncalled by analyzeColor",
        "truth": {
            "kind": "callers",
            "symbol": "`color.ts`/contrastRatio().",
            "expect": ["analyzeColor"],
            "exact": True,
        },
        "grep": [
            [["grep", {"pattern": "contrast", "repo": "devkit", "glob": "packages/**/*.ts", "ignore_case": True}]],
            [["read_file", {"path": "devkit/packages/core/src/tools/color.ts", "start_line": 215, "end_line": 262}]],
        ],
        "codegraph": [
            [["codegraph_find", {"query": "contrast", "repo": "devkit"}]],
            [["codegraph_callers", {"symbol": "contrastRatio", "repo": "devkit", "depth": 1}]],
        ],
    },
    # ------------------------------------------------------------------ Python: ledger
    {
        "id": "Q26",
        "lang": "python",
        "repo": "ledger",
        "kind": "impact",
        "question": "In ledger, if `ledger.frame.load` changes its signature, which functions "
        "call it directly and would need updating?",
        "expect": ["anomalies", "subscriptions", "monthly", "category_report", "budget_status"],
        "answer": "ledger.anomalies.anomalies\nledger.recurring.subscriptions\nledger.reports.monthly\n"
        "ledger.reports.category_report\nledger.reports.budget_status",
        "truth": {
            "kind": "callers",
            "symbol": "`ledger.frame`/load().",
            "expect": ["anomalies", "subscriptions", "monthly", "category_report", "budget_status"],
            "exact": True,
        },
        "grep": [
            [["grep", {"pattern": r"frame\b", "repo": "ledger", "glob": "src/**/*.py"}]],
            [
                [
                    "grep",
                    {"pattern": r"\bload\(", "repo": "ledger", "glob": "src/ledger/{reports,anomalies,recurring}.py"},
                ]
            ],
            [
                ["read_file", {"path": "ledger/src/ledger/reports.py", "start_line": 40, "end_line": 165}],
                ["read_file", {"path": "ledger/src/ledger/anomalies.py", "start_line": 25, "end_line": 38}],
                ["read_file", {"path": "ledger/src/ledger/recurring.py", "start_line": 130, "end_line": 145}],
            ],
        ],
        "codegraph": [
            [["codegraph_impact", {"target": "frame.load", "repo": "ledger", "depth": 1}]],
        ],
    },
    {
        "id": "Q27",
        "lang": "python",
        "repo": "ledger",
        "kind": "callers",
        "question": "In ledger's CLI, which command functions call `_parse_date_opt`?",
        "expect": ["anomalies", "subscriptions"],
        "answer": "cli.anomalies\ncli.subscriptions",
        "truth": {
            "kind": "callers",
            "symbol": "/_parse_date_opt().",
            "expect": ["anomalies", "subscriptions"],
            "exact": True,
        },
        "grep": [
            [["grep", {"pattern": r"_parse_date_opt\(", "repo": "ledger"}]],
            [
                ["read_file", {"path": "ledger/src/ledger/cli.py", "start_line": 675, "end_line": 696}],
                ["read_file", {"path": "ledger/src/ledger/cli.py", "start_line": 735, "end_line": 751}],
            ],
        ],
        "codegraph": [
            [["codegraph_callers", {"symbol": "_parse_date_opt", "repo": "ledger", "depth": 1}]],
        ],
    },
    {
        "id": "Q28",
        "lang": "python",
        "repo": "ledger",
        "kind": "implementations",
        "question": "In ledger's src (not tests), which classes satisfy the `Provider` protocol defined in llm.py?",
        "expect": ["AigwProvider", "AnthropicProvider"],
        "answer": "AigwProvider\nAnthropicProvider",
        "truth": {
            "kind": "source",
            "evidence": "ledger/src/ledger/llm.py:67 `class Provider(Protocol)` "
            "with one method categorize(items, categories); only AigwProvider (:142, :156) and "
            "AnthropicProvider (:232, :252) define categorize with that signature in src "
            "(cli.py:378 `categorize` is a Typer command function)",
        },
        "grep": [
            [["grep", {"pattern": r"class Provider\(Protocol\)", "repo": "ledger"}]],
            [["grep", {"pattern": r"^class |def categorize\(", "repo": "ledger", "glob": "src/**/*.py"}]],
        ],
        "codegraph": [
            [["codegraph_find", {"query": "Provider", "repo": "ledger"}]],
        ],
    },
    {
        "id": "Q29",
        "lang": "python",
        "repo": "ledger",
        "kind": "callees",
        "question": "In ledger, which functions defined in the ledger package does "
        "`importers.csvfmt.parse_csv` call directly? Ignore builtins, the stdlib "
        "and class constructors.",
        "expect": ["_row_dict", "parse_date", "_signed_amount", "parse_amount"],
        "answer": "_row_dict\nimporters.dates.parse_date\n_signed_amount\nmoney.parse_amount",
        "truth": {
            "kind": "callees",
            "symbol": "parse_csv",
            "range": ["src/ledger/importers/csvfmt.py", 353, 414],
            "expect": ["_row_dict", "parse_date", "_signed_amount", "parse_amount"],
        },
        "grep": [
            [["grep", {"pattern": r"def parse_csv\(", "repo": "ledger"}]],
            [["read_file", {"path": "ledger/src/ledger/importers/csvfmt.py", "start_line": 353, "end_line": 415}]],
        ],
        "codegraph": [
            [["codegraph_callees", {"symbol": "parse_csv", "repo": "ledger", "depth": 1}]],
        ],
    },
    {
        "id": "Q30",
        "lang": "python",
        "repo": "ledger",
        "kind": "definition",
        "question": "In ledger, in which file is `save_profile` defined? Give the repo-relative path.",
        "expect": ["src/ledger/store.py"],
        "answer": "src/ledger/store.py",
        "truth": {"kind": "defs", "symbol": "/save_profile().", "expect": ["src/ledger/store.py"], "exact": True},
        "grep": [
            [["grep", {"pattern": r"def save_profile\(", "repo": "ledger"}]],
        ],
        "codegraph": [
            [["codegraph_find", {"query": "save_profile", "repo": "ledger"}]],
        ],
    },
    {
        "id": "Q31",
        "lang": "python",
        "repo": "ledger",
        "kind": "semantic",
        "question": "In ledger, find the function that works out which date format "
        "(US vs EU day/month order) an imported statement uses, and list the "
        "non-test functions that call it.",
        "expect": ["detect_date_format", "_infer_headerless", "parse_qif"],
        "answer": "importers.dates.detect_date_format\ncallers: csvfmt._infer_headerless\n"
        "csvfmt.detect_profile (inner col())\nqif.parse_qif",
        "truth": {
            "kind": "callers",
            "symbol": "/detect_date_format().",
            "expect": ["_infer_headerless", "col", "parse_qif"],
        },
        "grep": [
            [["grep", {"pattern": r"def \w*date\w*\(", "repo": "ledger", "glob": "src/**/*.py"}]],
            [["grep", {"pattern": r"detect_date_format\(", "repo": "ledger", "glob": "src/**/*.py"}]],
            [
                ["read_file", {"path": "ledger/src/ledger/importers/csvfmt.py", "start_line": 160, "end_line": 250}],
                ["read_file", {"path": "ledger/src/ledger/importers/qif.py", "start_line": 12, "end_line": 46}],
            ],
        ],
        "codegraph": [
            [["codegraph_find", {"query": "date_format", "repo": "ledger"}]],
            [["codegraph_callers", {"symbol": "detect_date_format", "repo": "ledger", "depth": 1}]],
        ],
    },
]

ARMS = ("grep", "codegraph")

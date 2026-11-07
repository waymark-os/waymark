#!/usr/bin/env python3
"""Pilot: can models declare the files their actions write? (H-12, D-1)

Builds on the run_stone_tbench4.py ReAct loop. Each stone_eval or
escape_linux action runs in a bubblewrap sandbox: the root is read-only,
only /app, /work, /tmp and a sandbox $HOME are writable, the network is off,
and the environment is cleared, so every write the action makes lands in an
observed directory. A snapshot diff before and after each action gives the
observed writes.

Arms (see docs/STONE_LANGUAGE_CONTROL_DESIGN.md, section 11.1):
  none     arm 1: no declarations; observed writes recorded only
  report   arm 2r: "writes" required; undeclared writes kept and reported
  enforce  arm 2: "writes" required; undeclared writes rolled back and the
           action fails with UndeclaredEffect

This is a harness-level emulation. Enforcement restores a copy of the
writable directories instead of discarding an overlay layer.
"""

from __future__ import annotations

import argparse
import fnmatch
import hashlib
import json
import os
import shutil
import subprocess
import sys
import time
import urllib.error
import urllib.request
from collections import Counter
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parent))
import run_stone_tbench4 as tb  # noqa: E402

ROOT = tb.ROOT
SANDBOX_HOME = "/sbhome"
WRITABLE = ("/app", "/work", "/tmp", SANDBOX_HOME)
ROOTS = {"/", "/app", "/work", "/tmp", SANDBOX_HOME, "~"}
RO_TOP = ("usr", "etc", "bin", "sbin", "lib", "lib32", "lib64", "opt", "var")
HASH_LIMIT = 8 * 1024 * 1024

DEFAULT_TASKS = (
    "hello-world",
    "bank-trans-filter",
    "jsonl-aggregator",
    "log-summary",
    "analyze-access-logs",
    "heterogeneous-dates",
    "recover-accuracy-log",
    "grid-pattern-transform",
    "regex-log",
    "log-summary-date-ranges",
    "count-call-stack",
    "organization-json-generator",
    "synthetic-py-fix",
    "synthetic-c-build",
)

MODELS = {
    "qwen-local": {"provider": "openai-compatible", "model": None, "server": "http://127.0.0.1:8080/v1/chat/completions"},
    "claude-haiku-5-5": {"provider": "anthropic", "model": "claude-haiku-5-5", "server": "https://api.anthropic.com/v1/messages"},
    "gpt-6-luna": {"provider": "openai", "model": "gpt-6-luna", "server": "https://api.openai.com/v1/chat/completions", "key": "OPENAI_API_KEY"},
    "gemini-3.8-flash": {
        "provider": "openai",
        "model": "gemini-3.8-flash",
        "server": "https://generativelanguage.googleapis.com/v1beta/openai/chat/completions",
        "key": "GEMINI_API_KEY",
    },
}

# Paths that are side effects of tools rather than task outputs.
INCIDENTAL_PATTERNS = (
    ("pycache", ("*/__pycache__", "*/__pycache__/*", "*.pyc")),
    ("test_cache", ("*/.pytest_cache", "*/.pytest_cache/*")),
    ("build_artifact", ("*.o", "*.d", "*/build", "*/build/*", "*.a", "*.so")),
    ("home_cache", (SANDBOX_HOME + "/*",)),
    ("tmp", ("/tmp/*",)),
    ("editor_backup", ("*~", "*.swp", "*.bak", "*.orig")),
)


# ---------------------------------------------------------------- sandbox


@dataclass
class Sandbox:
    run_dir: Path
    dirs: dict[str, Path]
    waymark_bin: Path

    @classmethod
    def create(cls, ctx: tb.RunContext) -> "Sandbox":
        run_dir = ctx.app_dir.parent
        dirs = {"/app": ctx.app_dir, "/work": ctx.work_dir, "/tmp": run_dir / "tmp", SANDBOX_HOME: run_dir / "home"}
        for path in dirs.values():
            path.mkdir(parents=True, exist_ok=True)
        return cls(run_dir, dirs, ctx.waymark_bin)

    def bwrap_argv(self, cwd: str, command: list[str]) -> list[str]:
        argv = ["bwrap", "--tmpfs", "/"]
        for name in RO_TOP:
            path = Path("/") / name
            if not path.exists():
                continue
            if path.is_symlink():
                argv += ["--symlink", os.readlink(path), str(path)]
            else:
                argv += ["--ro-bind", str(path), str(path)]
        argv += ["--ro-bind", str(self.waymark_bin), "/wbin/waymark", "--dev", "/dev", "--proc", "/proc"]
        for logical, host in self.dirs.items():
            argv += ["--bind", str(host), logical]
        argv += [
            "--remount-ro", "/",
            "--clearenv",
            "--setenv", "PATH", "/wbin:/usr/local/bin:/usr/bin:/bin",
            "--setenv", "HOME", SANDBOX_HOME,
            "--setenv", "LANG", "C.UTF-8",
            "--unshare-net",
            "--die-with-parent",
            "--new-session",
            "--chdir", cwd,
        ]
        return argv + command

    def run(self, cwd: str, command: list[str], timeout_sec: float) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            self.bwrap_argv(cwd, command),
            text=True,
            capture_output=True,
            timeout=timeout_sec,
        )

    def snapshot(self) -> dict[str, tuple]:
        state: dict[str, tuple] = {}
        for logical, host in self.dirs.items():
            for dirpath, dirnames, filenames in os.walk(host):
                rel_dir = os.path.relpath(dirpath, host)
                base = logical if rel_dir == "." else f"{logical}/{rel_dir}"
                for name in dirnames:
                    full = os.path.join(dirpath, name)
                    if os.path.islink(full):
                        state[f"{base}/{name}"] = ("link", os.readlink(full))
                    else:
                        state[f"{base}/{name}"] = ("dir",)
                for name in filenames:
                    full = os.path.join(dirpath, name)
                    try:
                        if os.path.islink(full):
                            state[f"{base}/{name}"] = ("link", os.readlink(full))
                            continue
                        st = os.stat(full)
                    except OSError:
                        continue
                    digest = None
                    if st.st_size <= HASH_LIMIT:
                        try:
                            digest = hashlib.sha256(Path(full).read_bytes()).hexdigest()
                        except OSError:
                            digest = None
                    state[f"{base}/{name}"] = ("file", st.st_size, digest if digest else st.st_mtime_ns, st.st_mode)
        return state

    def backup(self, dest: Path) -> None:
        if dest.exists():
            shutil.rmtree(dest)
        for logical, host in self.dirs.items():
            shutil.copytree(host, dest / logical.strip("/"), symlinks=True)

    def restore(self, src: Path) -> None:
        for logical, host in self.dirs.items():
            shutil.rmtree(host)
            shutil.copytree(src / logical.strip("/"), host, symlinks=True)


def diff_snapshots(before: dict[str, tuple], after: dict[str, tuple]) -> dict[str, list[str]]:
    created = sorted(p for p in after if p not in before)
    deleted = sorted(p for p in before if p not in after)
    modified = sorted(p for p in after if p in before and after[p] != before[p] and after[p][0] != "dir")
    return {"created": created, "modified": modified, "deleted": deleted}


def observed_items(changes: dict[str, list[str]], before: dict[str, tuple], after: dict[str, tuple]) -> list[str]:
    """Collapse a diff to the paths a declaration should cover.

    A created or deleted directory counts only if no changed path lies inside
    it; otherwise its children already account for it.
    """
    changed = changes["created"] + changes["modified"] + changes["deleted"]
    items = []
    for path in changed:
        kind = (after.get(path) or before.get(path) or ("file",))[0]
        if kind == "dir" and any(other.startswith(path + "/") for other in changed):
            continue
        items.append(path)
    return sorted(set(items))


# ------------------------------------------------------------ declarations


def normalize_declared(raw: Any, cwd: str) -> tuple[list[str], str | None]:
    if not isinstance(raw, list) or not all(isinstance(item, str) for item in raw):
        return [], "writes must be a JSON list of path strings (use [] if the action writes nothing)"
    out = []
    for item in raw:
        path = item.strip()
        if not path:
            continue
        if path == "~" or path.startswith("~/"):
            path = SANDBOX_HOME + path[1:]
        if path.startswith("$HOME"):
            path = SANDBOX_HOME + path[len("$HOME"):]
        if not path.startswith("/"):
            path = os.path.normpath(os.path.join(cwd, path))
        path = path.rstrip("/") or "/"
        out.append(path)
    return out, None


def covered(path: str, declared: list[str]) -> bool:
    for decl in declared:
        if any(ch in decl for ch in "*?["):
            if fnmatch.fnmatch(path, decl) or fnmatch.fnmatch(path, decl.rstrip("/*") + "/*"):
                return True
            continue
        if decl == "/" or path == decl or path.startswith(decl + "/"):
            return True
        # Creating the parent directory of a declared file is implied.
        if decl.startswith(path + "/"):
            return True
    return False


def incidental_class(path: str) -> str:
    for name, patterns in INCIDENTAL_PATTERNS:
        if any(fnmatch.fnmatch(path, pattern) for pattern in patterns):
            return name
    return "task_path"


def command_family(action: dict[str, Any]) -> str:
    tool = action.get("tool")
    if tool == "escape_linux":
        command = (action.get("input") or {}).get("command") or ""
        words = command.strip().split()
        while words and "=" in words[0] and not words[0].startswith(("/", ".")):
            words = words[1:]
        return f"shell:{os.path.basename(words[0])}" if words else "shell:?"
    return str(tool)


# ------------------------------------------------------------ synthetic tasks

SYNTHETIC = {
    "synthetic-py-fix": {
        "instruction": (
            "The Python package in /app/calc has a bug that makes the unit tests in /app/tests fail. "
            "Run the tests with `python3 -m unittest discover -s tests` from /app, fix the bug in the "
            "package (do not modify the tests), and write the final test summary line (for example "
            "`OK` or `FAILED (failures=1)`) to /work/test_result.txt."
        ),
        "files": {
            "calc/__init__.py": "from .ops import add, mean\n",
            "calc/ops.py": (
                "def add(a, b):\n    return a + b\n\n\n"
                "def mean(values):\n    if not values:\n        raise ValueError('empty')\n"
                "    return sum(values) / (len(values) - 1)\n"
            ),
            "tests/__init__.py": "",
            "tests/test_ops.py": (
                "import unittest\n\nfrom calc import add, mean\n\n\n"
                "class OpsTest(unittest.TestCase):\n"
                "    def test_add(self):\n        self.assertEqual(add(2, 3), 5)\n\n"
                "    def test_mean(self):\n        self.assertEqual(mean([1, 2, 3, 4]), 2.5)\n\n"
                "    def test_mean_empty(self):\n        with self.assertRaises(ValueError):\n            mean([])\n\n\n"
                "if __name__ == '__main__':\n    unittest.main()\n"
            ),
        },
    },
    "synthetic-c-build": {
        "instruction": (
            "/app/src contains a small C program with a Makefile. `make` currently fails. Fix the "
            "source so `make -C /app/src` builds `/app/src/wordcount`, then run "
            "`/app/src/wordcount /app/input.txt` and write its stdout to /work/count.txt."
        ),
        "files": {
            "src/Makefile": (
                "CC ?= cc\nCFLAGS ?= -O1 -Wall\n\nwordcount: main.o count.o\n\t$(CC) $(CFLAGS) -o $@ $^\n\n"
                "%.o: %.c count.h\n\t$(CC) $(CFLAGS) -c -o $@ $<\n\nclean:\n\trm -f *.o wordcount\n"
            ),
            "src/count.h": "#pragma once\n#include <stdio.h>\nlong count_words(FILE *f);\n",
            "src/count.c": (
                "#include <ctype.h>\n#include \"count.h\"\n\n"
                "long count_words(FILE *f) {\n    long n = 0; int in = 0, c;\n"
                "    while ((c = fgetc(f)) != EOF) {\n        if (isspace(c)) in = 0;\n"
                "        else if (!in) { in = 1; n++; }\n    }\n    return n\n}\n"
            ),
            "src/main.c": (
                "#include <stdio.h>\n#include \"count.h\"\n\n"
                "int main(int argc, char **argv) {\n    if (argc != 2) { fprintf(stderr, \"usage: wordcount FILE\\n\"); return 2; }\n"
                "    FILE *f = fopen(argv[1], \"r\");\n    if (!f) { perror(argv[1]); return 1; }\n"
                "    printf(\"%ld\\n\", count_words(f));\n    fclose(f);\n    return 0;\n}\n"
            ),
            "input.txt": "the quick brown fox\njumps over\n\nthe lazy dog\n",
        },
    },
}


def prepare(task_id: str, args: argparse.Namespace, out_dir: Path) -> tb.RunContext:
    if task_id not in SYNTHETIC:
        return tb.prepare_task(task_id, Path(args.tasks_root), out_dir, args.waymark_bin, args.fixture_cache_dir, "exact")
    spec = SYNTHETIC[task_id]
    run_dir = out_dir / task_id
    if run_dir.exists():
        shutil.rmtree(run_dir)
    app_dir, work_dir, log_dir = run_dir / "app", run_dir / "work", run_dir / "logs"
    for path in (app_dir, work_dir, log_dir):
        path.mkdir(parents=True)
    for rel, text in spec["files"].items():
        dest = app_dir / rel
        dest.parent.mkdir(parents=True, exist_ok=True)
        dest.write_text(text)
    (run_dir / "instruction.txt").write_text(spec["instruction"])
    return tb.RunContext(task_id, run_dir, app_dir, work_dir, log_dir, spec["instruction"], args.waymark_bin, "exact")


def verify(ctx: tb.RunContext) -> dict[str, Any]:
    if ctx.task_id not in SYNTHETIC:
        try:
            return tb.verify_context(ctx)
        except Exception as err:  # verifier crashed on malformed output
            return {"ok": False, "failures": [f"verifier error: {type(err).__name__}: {err}"]}
    failures = []
    spec = SYNTHETIC[ctx.task_id]
    if ctx.task_id == "synthetic-py-fix":
        if (ctx.app_dir / "tests/test_ops.py").read_text() != spec["files"]["tests/test_ops.py"]:
            failures.append("tests/test_ops.py was modified")
        check = subprocess.run(
            [sys.executable, "-B", "-m", "unittest", "discover", "-s", "tests"],
            cwd=ctx.app_dir, capture_output=True, text=True, timeout=60,
        )
        if check.returncode != 0:
            failures.append("unit tests still fail")
        result = ctx.work_dir / "test_result.txt"
        if not result.exists() or "OK" not in result.read_text():
            failures.append("/work/test_result.txt missing or not OK")
    else:
        result = ctx.work_dir / "count.txt"
        if not result.exists() or result.read_text().strip() != "9":
            failures.append("/work/count.txt missing or wrong (expected 9)")
    return {"ok": not failures, "failures": failures}


# ------------------------------------------------------------ model clients


def chat(model_key: str, args: argparse.Namespace, system: str, messages: list[dict[str, str]]) -> tuple[str, dict[str, int]]:
    spec = MODELS[model_key]
    provider = spec["provider"]
    headers = {"Content-Type": "application/json"}
    if provider == "anthropic":
        key = os.environ.get("ANTHROPIC_API_KEY")
        if not key:
            raise RuntimeError("ANTHROPIC_API_KEY is not set; source ~/waymark.env")
        headers.update({"x-api-key": key, "anthropic-version": "2026-10-21"})
        body = {"model": spec["model"], "max_tokens": args.max_tokens, "system": system, "messages": messages}
    else:
        body = {"model": spec["model"] or args.local_model, "messages": [{"role": "system", "content": system}] + messages}
        if provider == "openai":
            key = os.environ.get(spec["key"])
            if not key:
                raise RuntimeError(f"{spec['key']} is not set; source ~/waymark.env")
            headers["Authorization"] = f"Bearer {key}"
            body["max_completion_tokens"] = args.max_tokens
        else:
            body["max_tokens"] = args.max_tokens
            body["temperature"] = args.local_temperature
            body["chat_template_kwargs"] = {"enable_thinking": False}
    request = urllib.request.Request(spec["server"], data=json.dumps(body).encode(), headers=headers, method="POST")
    payload = None
    for attempt in range(4):
        try:
            with urllib.request.urlopen(request, timeout=args.model_timeout) as response:
                payload = json.loads(response.read().decode())
            break
        except urllib.error.HTTPError as err:
            detail = err.read().decode(errors="replace")[:2000]
            if err.code in (429, 500, 502, 503, 529) and attempt < 3:
                time.sleep(5 * (attempt + 1))
                continue
            raise RuntimeError(f"model HTTP {err.code}: {detail}") from err
        except urllib.error.URLError as err:
            if attempt < 3:
                time.sleep(5 * (attempt + 1))
                continue
            raise
    assert payload is not None
    if provider == "anthropic":
        if payload.get("stop_reason") == "refusal":
            raise RuntimeError(f"model refusal: {payload.get('stop_details')}")
        text = "".join(block.get("text", "") for block in payload.get("content", []) if block.get("type") == "text")
        usage = payload.get("usage") or {}
        return text, {"prompt_tokens": int(usage.get("input_tokens") or 0), "completion_tokens": int(usage.get("output_tokens") or 0)}
    message = payload["choices"][0].get("message") or {}
    text = message.get("content") if isinstance(message.get("content"), str) else ""
    usage = payload.get("usage") or {}
    return text or "", {"prompt_tokens": int(usage.get("prompt_tokens") or 0), "completion_tokens": int(usage.get("completion_tokens") or 0)}


# ------------------------------------------------------------ agent loop

def parse_action(text: str) -> tuple[dict[str, Any], str | None, bool]:
    """Parse the first JSON object in the reply.

    Returns (action, error, lenient). lenient is True when text followed the
    first complete object (for example an extra closing brace); such replies
    are accepted but counted, approximating constrained decoding.
    """
    stripped = text.strip()
    start = stripped.find("{")
    if start >= 0:
        try:
            value, end = json.JSONDecoder().raw_decode(stripped[start:])
        except json.JSONDecodeError:
            value = None
        if isinstance(value, dict):
            if isinstance(value.get("actions"), list) and value["actions"] and isinstance(value["actions"][0], dict):
                value = value["actions"][0]
            trailing = stripped[start + end:].strip()
            return value, None, bool(trailing)
    action, error = tb.parse_action_json(text)
    return action, error, False


def declared_writes(action: dict[str, Any]) -> tuple[bool, Any]:
    inp = action.get("input")
    if isinstance(inp, dict) and "writes" in inp:
        return True, inp["writes"]
    if "writes" in action:
        return True, action["writes"]
    return False, None


WRITES_RULE = {
    "none": "",
    "report": (
        '\nEvery stone_eval and escape_linux action must include "writes" in its input: a list of absolute paths the action may '
        "create, modify, or delete. A directory covers everything under it. Include files created as side effects "
        "(temporary files, caches, build outputs). Use [] for actions that write nothing. Writes you did not declare "
        "are kept, but they are reported back to you."
    ),
    "enforce": (
        '\nEvery stone_eval and escape_linux action must include "writes" in its input: a list of absolute paths the action may '
        "create, modify, or delete. A directory covers everything under it. Include files created as side effects "
        "(temporary files, caches, build outputs). Use [] for actions that write nothing. If the action writes "
        "anything you did not declare, all of its changes are rolled back and the action fails with UndeclaredEffect."
    ),
}


def action_schema(arm: str) -> str:
    # v2: "writes" is a field of the action input (flat, like the design's Run
    # record). v1 placed it beside "input", which caused brace errors on Qwen.
    writes = ',"writes":["/work/out.txt"]' if arm != "none" else ""
    return (
        "\n\nAction schema:\n"
        f'{{"tool":"stone_eval","input":{{"source":"..."{writes}}}}}\n'
        '{"tool":"stone_help","input":{"name":"optional_builtin_or_topic"}}\n'
        f'{{"tool":"escape_linux","input":{{"command":"...","cwd":"/app","timeout_ms":30000{writes}}}}}\n'
        '{"tool":"finish","input":{}}\n'
        "Use one action per turn. Do not finish until the required /work output exists."
        "\nThe sandbox has no network. /app, /work, /tmp and $HOME are writable; everything else is read-only."
        + WRITES_RULE[arm]
    )


@dataclass
class ActionRecord:
    turn: int
    family: str
    declared: list[str] | None
    observed: list[str]
    undeclared: list[str]
    unused_declared: list[str]
    broad: bool
    rolled_back: bool
    ok: bool
    format_error: str | None = None


@dataclass
class Episode:
    task_id: str
    model: str
    arm: str
    actions: list[ActionRecord] = field(default_factory=list)
    resolved: bool = False
    turns: int = 0
    finished: bool = False
    input_tokens: int = 0
    output_tokens: int = 0
    lenient_parses: int = 0
    parse_errors: int = 0
    error: str | None = None
    verification: dict[str, Any] = field(default_factory=dict)


def execute(ctx: tb.RunContext, sandbox: Sandbox, action: dict[str, Any], timeout_sec: float) -> dict[str, Any]:
    tool = action.get("tool")
    inp = action.get("input") if isinstance(action.get("input"), dict) else {}
    try:
        if tool == "stone_eval":
            source = inp.get("source")
            if not isinstance(source, str):
                return {"ok": False, "error": "stone_eval requires input.source"}
            result = sandbox.run("/app", ["waymark", "eval", "-c", source], timeout_sec)
            response = tb.sanitize_waymark_response(tb.parse_waymark_response(result.stdout) or tb.parse_waymark_response(result.stderr))
            return {
                "ok": result.returncode == 0 and bool(response and response.get("ok")),
                "kind": "stone_eval",
                "returncode": result.returncode,
                "response": response,
                "stdout_tail": tb.tail(tb.sanitize_waymark_output(result.stdout)),
                "stderr_tail": tb.tail(tb.sanitize_waymark_output(result.stderr)),
            }
        if tool == "escape_linux":
            command = inp.get("command")
            cwd = inp.get("cwd", "/app")
            timeout_ms = inp.get("timeout_ms", int(timeout_sec * 1000))
            if not isinstance(command, str) or not isinstance(cwd, str):
                return {"ok": False, "error": "escape_linux requires string input.command and input.cwd"}
            if not any(cwd == d or cwd.startswith(d + "/") for d in WRITABLE):
                cwd = "/app"
            timeout = max(1.0, min(float(timeout_ms) / 1000.0 if isinstance(timeout_ms, (int, float)) else timeout_sec, timeout_sec))
            result = sandbox.run(cwd, ["bash", "-c", command], timeout)
            return {
                "ok": result.returncode == 0,
                "kind": "escape_linux",
                "returncode": result.returncode,
                "stdout_tail": tb.tail(result.stdout),
                "stderr_tail": tb.tail(result.stderr),
            }
    except subprocess.TimeoutExpired:
        return {"ok": False, "kind": str(tool), "error": f"timed out after {timeout_sec}s"}
    return {"ok": False, "error": f"unsupported tool {tool!r}"}


def run_episode(task_id: str, model_key: str, arm: str, args: argparse.Namespace, out_dir: Path) -> Episode:
    ctx = prepare(task_id, args, out_dir)
    sandbox = Sandbox.create(ctx)
    episode = Episode(task_id, model_key, arm)
    guide = tb.build_prompt_guide(args.waymark_bin, "full")
    system = (
        "You are a Stone Terminal-Bench agent. Return only JSON action objects. "
        "Use Stone for file, CSV, JSON, JSONL, text, find/glob, and simple transform work."
    )
    messages = [{
        "role": "user",
        "content": guide
        + "\n\nTask instruction:\n" + ctx.instruction
        + "\n\nHarness execution constraints:\n"
        "- Each stone_eval call runs in a fresh Waymark CLI process; variables do not persist between tool calls.\n"
        "- Repeat needed reads and setup in the same stone_eval source that uses them.\n"
        + action_schema(arm),
    }]
    backup_dir = sandbox.run_dir / ".backup"
    for turn in range(1, args.max_turns + 1):
        episode.turns = turn
        try:
            text, usage = chat(model_key, args, system, messages)
        except Exception as err:
            episode.error = f"{type(err).__name__}: {err}"
            break
        episode.input_tokens += usage["prompt_tokens"]
        episode.output_tokens += usage["completion_tokens"]
        action, parse_error, lenient = parse_action(text)
        episode.lenient_parses += lenient
        episode.parse_errors += parse_error is not None
        tb.write_json_file(ctx.log_dir / f"action-{turn:03d}.json", {"raw": text, "action": action, "error": parse_error, "lenient": lenient})
        tool = action.get("tool") if not parse_error else None
        if parse_error:
            observation: dict[str, Any] = {"ok": False, "error": parse_error, "guidance": "Return one JSON action object only."}
        elif tool == "finish":
            verification = verify(ctx)
            observation = {"ok": verification["ok"], "finished": True, "verification": verification}
            episode.finished = True
        elif tool == "stone_help":
            observation = tb.stone_help(ctx, (action.get("input") or {}).get("name"), args.timeout)
        elif tool in ("stone_eval", "escape_linux"):
            observation = step(ctx, sandbox, action, arm, turn, episode, backup_dir, args)
        else:
            observation = {"ok": False, "error": f"unsupported tool {tool!r}"}
        tb.write_json_file(ctx.log_dir / f"observation-{turn:03d}.json", observation)
        messages.append({"role": "assistant", "content": text if text.strip() else "{}"})
        messages.append({
            "role": "user",
            "content": "Observation:\n" + tb.truncate(json.dumps(observation, indent=2, sort_keys=True), 12000)
            + "\nIf the task-required /work output exists and is correct, return finish. Otherwise continue.",
        })
        if episode.finished:
            break
    episode.verification = verify(ctx)
    episode.resolved = bool(episode.verification.get("ok"))
    if backup_dir.exists():
        shutil.rmtree(backup_dir)
    return episode


def step(
    ctx: tb.RunContext,
    sandbox: Sandbox,
    action: dict[str, Any],
    arm: str,
    turn: int,
    episode: Episode,
    backup_dir: Path,
    args: argparse.Namespace,
) -> dict[str, Any]:
    family = command_family(action)
    declared = None
    if arm != "none":
        cwd = (action.get("input") or {}).get("cwd", "/app") if action.get("tool") == "escape_linux" else "/app"
        present, raw_writes = declared_writes(action)
        if not present:
            error = 'missing "writes" in input: list every absolute path this action may create, modify, or delete ([] for none)'
            episode.actions.append(ActionRecord(turn, family, None, [], [], [], False, False, False, format_error="missing"))
            return {"ok": False, "error": error}
        declared, error = normalize_declared(raw_writes, cwd if isinstance(cwd, str) else "/app")
        if error:
            episode.actions.append(ActionRecord(turn, family, None, [], [], [], False, False, False, format_error="malformed"))
            return {"ok": False, "error": error}
    if arm == "enforce":
        sandbox.backup(backup_dir)
    before = sandbox.snapshot()
    observation = execute(ctx, sandbox, action, args.timeout)
    after = sandbox.snapshot()
    changes = diff_snapshots(before, after)
    observed = observed_items(changes, before, after)
    undeclared = [p for p in observed if declared is not None and not covered(p, declared)]
    unused = [d for d in (declared or []) if not any(p == d or p.startswith(d + "/") or d.startswith(p + "/") or fnmatch.fnmatch(p, d) for p in observed)]
    broad = any(d in ROOTS for d in (declared or []))
    rolled_back = False
    if arm == "report" and declared is not None:
        observation["effects"] = {"declared": declared, "observed": observed, "undeclared": undeclared}
    elif arm == "enforce" and undeclared:
        sandbox.restore(backup_dir)
        rolled_back = True
        observation = {
            "ok": False,
            "kind": "UndeclaredEffect",
            "error": "the action wrote paths it did not declare; all of its changes were rolled back",
            "undeclared": undeclared,
            "declared": declared,
            "fix": "add these paths (or a directory that contains them) to writes and retry, or avoid writing them",
            "action_output": {k: observation.get(k) for k in ("returncode", "stdout_tail", "stderr_tail", "response") if k in observation},
        }
    elif arm == "none":
        observation["effects"] = {"observed": observed}
    observation["verification"] = verify(ctx)
    episode.actions.append(ActionRecord(turn, family, declared, observed, undeclared, unused, broad, rolled_back, bool(observation.get("ok"))))
    return observation


# ------------------------------------------------------------ reporting


def summarize(episodes: list[Episode]) -> dict[str, Any]:
    groups: dict[tuple[str, str], list[Episode]] = {}
    for ep in episodes:
        groups.setdefault((ep.model, ep.arm), []).append(ep)
    rows = []
    for (model, arm), eps in sorted(groups.items()):
        acts = [a for ep in eps for a in ep.actions]
        declaring = [a for a in acts if a.declared is not None]
        writing = [a for a in declaring if a.observed]
        obs_total = sum(len(a.observed) for a in writing)
        obs_covered = sum(len(a.observed) - len(a.undeclared) for a in writing)
        missed = Counter(incidental_class(p) for a in declaring for p in a.undeclared)
        observed_classes = Counter(incidental_class(p) for a in acts for p in a.observed)
        by_family: dict[str, list[int]] = {}
        for a in writing:
            entry = by_family.setdefault(a.family, [0, 0, 0])
            entry[0] += 1
            entry[1] += len(a.observed)
            entry[2] += len(a.undeclared)
        rows.append({
            "model": model,
            "arm": arm,
            "episodes": len(eps),
            "resolved": sum(ep.resolved for ep in eps),
            "harness_errors": sum(ep.error is not None for ep in eps),
            "mean_turns": round(sum(ep.turns for ep in eps) / len(eps), 2),
            "tokens_in": sum(ep.input_tokens for ep in eps),
            "tokens_out": sum(ep.output_tokens for ep in eps),
            "parse_errors": sum(ep.parse_errors for ep in eps),
            "lenient_parses": sum(ep.lenient_parses for ep in eps),
            "effect_actions": len(acts),
            "format_errors": sum(a.format_error is not None for a in acts),
            "writing_actions": len(writing),
            "actions_fully_declared": sum(not a.undeclared for a in writing),
            "path_recall": round(obs_covered / obs_total, 3) if obs_total else None,
            "broad_declarations": sum(a.broad for a in declaring),
            "declared_nothing_written": sum(1 for a in declaring if a.declared and not a.observed),
            "rollbacks": sum(a.rolled_back for a in acts),
            "missed_by_class": dict(missed),
            "observed_by_class": dict(observed_classes),
            "by_family": {k: {"actions": v[0], "observed": v[1], "undeclared": v[2]} for k, v in sorted(by_family.items())},
        })
    return {"groups": rows}


def episode_json(ep: Episode) -> dict[str, Any]:
    return {
        "task_id": ep.task_id, "model": ep.model, "arm": ep.arm, "resolved": ep.resolved, "turns": ep.turns,
        "finished": ep.finished, "error": ep.error, "input_tokens": ep.input_tokens, "output_tokens": ep.output_tokens,
        "parse_errors": ep.parse_errors, "lenient_parses": ep.lenient_parses,
        "verification": ep.verification, "actions": [a.__dict__ for a in ep.actions],
    }


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--models", nargs="+", default=["qwen-local"], choices=sorted(MODELS))
    parser.add_argument("--arms", nargs="+", default=["none", "report", "enforce"], choices=("none", "report", "enforce"))
    parser.add_argument("--tasks", nargs="+", default=list(DEFAULT_TASKS))
    parser.add_argument("--repeats", type=int, default=1)
    parser.add_argument("--tasks-root", default=str(tb.DEFAULT_TASKS_ROOT))
    parser.add_argument("--waymark-bin", type=Path, default=ROOT / "target/debug/waymark")
    parser.add_argument("--fixture-cache-dir", type=Path, default=ROOT / "target/runs/stone-tbench4-fixtures")
    parser.add_argument("--out-dir", type=Path)
    parser.add_argument("--max-turns", type=int, default=12)
    parser.add_argument("--max-tokens", type=int, default=8192)
    parser.add_argument("--timeout", type=float, default=120.0, help="per-action timeout in seconds")
    parser.add_argument("--model-timeout", type=float, default=300.0)
    parser.add_argument("--local-model", default="local")
    parser.add_argument("--local-temperature", type=float, default=0.7)
    parser.add_argument("--dry-run", action="store_true", help="prepare tasks and check the sandbox; no model calls")
    args = parser.parse_args()
    args.waymark_bin = args.waymark_bin.resolve()
    return args


def dry_run(args: argparse.Namespace, out_dir: Path) -> int:
    failures = 0
    for task_id in args.tasks:
        try:
            ctx = prepare(task_id, args, out_dir)
            sandbox = Sandbox.create(ctx)
            before = sandbox.snapshot()
            probe = sandbox.run("/app", ["bash", "-c", "touch /work/.probe && (touch /etc/.probe 2>/dev/null && echo rw || echo ro)"], 30)
            changes = diff_snapshots(before, sandbox.snapshot())
            stone = sandbox.run("/app", ["waymark", "eval", "-c", "emit(1 + 1)"], 30)
            ok = probe.stdout.strip() == "ro" and changes["created"] == ["/work/.probe"] and '"value":2' in stone.stdout
            print(json.dumps({"task": task_id, "ok": ok, "observed": changes, "system_dirs": probe.stdout.strip()}))
            failures += not ok
        except Exception as err:
            print(json.dumps({"task": task_id, "ok": False, "error": f"{type(err).__name__}: {err}"}))
            failures += 1
    for arm in args.arms:
        print(f"--- action schema for arm {arm} ---{action_schema(arm)}\n")
    return 1 if failures else 0


def main() -> int:
    args = parse_args()
    out_dir = (args.out_dir or ROOT / "target/runs" / f"pilot-declared-writes-{time.strftime('%Y%m%dT%H%M%S')}").resolve()
    out_dir.mkdir(parents=True, exist_ok=True)
    if args.dry_run:
        return dry_run(args, out_dir)
    episodes = []
    for repeat in range(args.repeats):
        for model_key in args.models:
            for arm in args.arms:
                for task_id in args.tasks:
                    run_out = out_dir / model_key / arm / f"r{repeat}"
                    run_out.mkdir(parents=True, exist_ok=True)
                    ep = run_episode(task_id, model_key, arm, args, run_out)
                    episodes.append(ep)
                    tb.write_json_file(run_out / task_id / "episode.json", episode_json(ep))
                    print(json.dumps({k: v for k, v in episode_json(ep).items() if k != "actions" and k != "verification"}), flush=True)
                    tb.write_json_file(out_dir / "summary.json", summarize(episodes))
    print(f"wrote {out_dir / 'summary.json'}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

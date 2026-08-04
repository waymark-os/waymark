#!/usr/bin/env python3
"""Test whether an outer model can author Stone best-candidate control."""

from __future__ import annotations

import argparse
import json
import subprocess
import time
from pathlib import Path
from typing import Any

import eval_stone_agent_authorship as base


ROOT = Path(__file__).resolve().parents[2]
SANDBOX = ROOT.parent
TOPICS = (
    "attempt_info",
    "attempt_scope",
    "attempt_fork",
    "attempt_join",
    "attempt_best",
    "attempt_best_consider",
    "attempt_best_accept",
    "attempt_best_discard",
    "attempt_scope_close",
    "attempt_report",
    "current_program",
    "task_input",
    "write_file",
    "read_file",
    "fail",
)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--model", default="gpt-5.6-terra")
    parser.add_argument("--codex", default="codex")
    parser.add_argument(
        "--waymark-bin", type=Path, default=ROOT / "target/debug/waymark"
    )
    parser.add_argument(
        "--gateway-bin",
        type=Path,
        default=SANDBOX / "waymark-gateway/target/debug/waymark-gateway",
    )
    parser.add_argument(
        "--run-root",
        type=Path,
        default=ROOT / "target/runs/stone-attempt-best-authorship-v2",
    )
    parser.add_argument("--timeout", type=float, default=600.0)
    parser.add_argument("--overwrite", action="store_true")
    return parser.parse_args()


def help_bundle(waymark_bin: Path) -> dict[str, dict[str, Any]]:
    return {
        topic: base.compact_help(base.help_topic(waymark_bin, topic))
        for topic in TOPICS
    }


def authorship_prompt(topics: dict[str, dict[str, Any]]) -> str:
    interface = json.dumps(topics, separators=(",", ":"), sort_keys=True)
    return f"""You are the outer coding agent. Write one complete Stone program. Do not call tools and do not inspect the filesystem. Return only the required JSON object.

Stone is an LLM-oriented, Python-shaped structured shell language, not Python. It supports def/return, for, if, try/except, lists, records, field access, and ordinary function calls. It has no imports, classes, Python standard library, or implicit async runtime.

Authoritative builtin help:
{interface}

Program contract:
1. Define `worker(input)`. It writes `input.answer + "\\n"` to `answer.txt` and returns name, answer, score, evidence `["canary:score:" + input.name]`, and artifacts `["answer.txt"]`.
2. Define `main(input)`. Capture the current root attempt, create one attempt scope, and create a maximizing attempt_best bound to that scope.
3. In declared order, explore exactly these candidates: baseline/alpha/0.50, winner/beta/0.90, late-worse/gamma/0.70. For each candidate fork an isolated child that runs this same program at entrypoint `worker`, starts immediately under the scope, and join it with a 60000 ms timeout.
4. Unless each outcome succeeded, call Stone's documented `fail(...)` builtin (Python `raise` is unavailable). Consider each outcome through the best-candidate API using its returned score, a nonempty summary, evidence, and artifacts. Append each decision record and child attempt id to lists. Do not manually accept or discard individual candidates; selector policy owns that.
5. After all three candidates, accept the retained best into the unchanged root, then close the scope. Read the imported `answer.txt` and require it to be `beta` and cleanup to be clean.
6. Return a record with exactly these observable fields: answer, winner, score, status, considered, replacements, released_outcome, children, decisions, clean. `released_outcome` must prove with try/except that the selector no longer exposes its full outcome after acceptance.
7. At top level, dispatch to `worker(task_input())` when attempt metadata's program_entrypoint is `worker`; otherwise call `main(task_input())`. Report succeeded with attempt_report and emit the result so the controller log is machine-readable.
8. Keep all exploration, scoring, lifecycle, and cleanup visible in Stone. Do not use hidden helpers, model calls, shell commands, imports, semantic frontiers, or copy a pre-existing reference program.
"""


def output_schema() -> dict[str, Any]:
    return {
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "properties": {"source": {"type": "string", "minLength": 1}},
        "required": ["source"],
        "additionalProperties": False,
    }


def source_features(source: str) -> dict[str, bool]:
    return {
        "worker": "def worker(" in source,
        "main": "def main(" in source,
        "scope": "attempt_scope(" in source,
        "fork": "attempt_fork(" in source,
        "join": "attempt_join(" in source,
        "best": "attempt_best(" in source,
        "consider": "attempt_best_consider(" in source,
        "accept": "attempt_best_accept(" in source,
        "scope_close": "attempt_scope_close(" in source,
        "report": "attempt_report(" in source,
        "emit": "emit(" in source,
        "forbidden_import": "\nimport " in "\n" + source
        or "\nfrom " in "\n" + source,
    }


def main() -> int:
    args = parse_args()
    args.waymark_bin = args.waymark_bin.resolve()
    args.gateway_bin = args.gateway_bin.resolve()
    run_root = args.run_root.resolve()
    if run_root.exists() and any(run_root.iterdir()) and not args.overwrite:
        raise SystemExit(f"refusing to overwrite non-empty run root: {run_root}")
    run_root.mkdir(parents=True, exist_ok=True)

    prompt = authorship_prompt(help_bundle(args.waymark_bin))
    (run_root / "prompt.txt").write_text(prompt, encoding="utf-8")
    (run_root / "output-schema.json").write_text(
        json.dumps(output_schema(), indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    command = base.codex_command(args, args.model, run_root, prompt)
    started = time.monotonic()
    try:
        completed = base.run_capture(command, cwd=run_root, timeout=args.timeout)
        timed_out = False
    except subprocess.TimeoutExpired as error:
        completed = subprocess.CompletedProcess(
            command, 124, error.stdout or "", error.stderr or ""
        )
        timed_out = True
    (run_root / "codex.stdout.jsonl").write_text(completed.stdout, encoding="utf-8")
    (run_root / "codex.stderr").write_text(completed.stderr, encoding="utf-8")
    source, output_error = base.parse_last_message(run_root / "last-message.json")
    features = source_features(source or "")
    authored = run_root / "authored.stone"
    if source:
        authored.write_text(source.rstrip() + "\n", encoding="utf-8")

    smoke = None
    if source and completed.returncode == 0 and output_error is None:
        smoke_command = [
            "python3",
            str(ROOT / "host/bench/smoke_attempt_best.py"),
            "--waymark-bin",
            str(args.waymark_bin),
            "--gateway-bin",
            str(args.gateway_bin),
            "--program",
            str(authored),
        ]
        smoke_completed = base.run_capture(
            smoke_command, cwd=ROOT, timeout=max(args.timeout, 360.0)
        )
        smoke = {
            "ok": smoke_completed.returncode == 0,
            "exit_code": smoke_completed.returncode,
            "stdout": smoke_completed.stdout,
            "stderr": smoke_completed.stderr,
        }

    required = all(value for key, value in features.items() if key != "forbidden_import")
    passed = (
        completed.returncode == 0
        and output_error is None
        and required
        and not features["forbidden_import"]
        and bool(smoke and smoke["ok"])
    )
    summary = {
        "ok": passed,
        "model": args.model,
        "duration_seconds": time.monotonic() - started,
        "timed_out": timed_out,
        "codex_exit_code": completed.returncode,
        "codex_tool_calls": base.tool_call_count(completed.stdout),
        "output_error": output_error,
        "source_bytes": len((source or "").encode("utf-8")),
        "features": features,
        "smoke": smoke,
        "run_root": str(run_root),
    }
    (run_root / "summary.json").write_text(
        json.dumps(summary, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    print(json.dumps(summary, indent=2, sort_keys=True))
    return 0 if passed else 1


if __name__ == "__main__":
    raise SystemExit(main())

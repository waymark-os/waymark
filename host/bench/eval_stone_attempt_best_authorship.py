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
    "stat",
    "run_complete",
    "fail",
)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--case",
        choices=("max-score", "min-derived-cost", "process-compression"),
        default="max-score",
        help="authorship task shape to execute",
    )
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


def authorship_prompt(
    topics: dict[str, dict[str, Any]], case: str = "max-score"
) -> str:
    interface = json.dumps(topics, separators=(",", ":"), sort_keys=True)
    if case == "process-compression":
        contract = """Program contract:
1. Define `worker(input)` for a real compression candidate. Every child sees `/app/input.txt`. For codec `lzma`, use `run_complete` with `python3 -c` to read that file and write `/app/archive.bin` with `lzma.compress(data, preset=9)`. For codec `gzip`, do the same with `gzip.compress(data, compresslevel=9, mtime=0)`. Codec `missing` must deliberately execute a Python import of `definitely_missing_waymark_codec` so its evaluator returns nonzero. Use a 30000 ms timeout and inspect the process result.
2. A failed compression process is an evaluated but invalid candidate, not a controller crash: write `input.answer + "\\n"` to `answer.txt` and return name, answer, valid=false, cost=1000000000, evidence `["compression:failed:" + input.codec]`, and artifacts `["answer.txt"]`. On success, measure the actual `/app/archive.bin` bytes with `stat`, write `answer.txt`, and return valid=true, the measured integer cost, evidence `["compression:ok:" + input.codec]`, and artifacts `["answer.txt", "archive.bin"]`.
3. Define `main(input)`. Capture the current root attempt, create one attempt scope, and create a minimizing attempt_best bound to that scope with `objective="min"`.
4. In declared order, explore exactly these candidate records, with no parent-supplied score: missing/alpha, lzma/lzma, gzip/gzip. For each candidate fork an isolated child that runs this same program at entrypoint `worker`, starts immediately under the scope, and join it with a 60000 ms timeout. A child controller that does not report succeeded is a `candidate_failed` task failure.
5. Consider every successful child outcome through the selector using the worker's returned cost, a nonempty summary, evidence, and artifacts. Append each decision and child id. Do not manually accept or discard candidates; selector policy owns that. The failed evaluator should be retained initially with its penalty, replaced by lzma, and the later larger gzip archive should be rejected.
6. Accept the retained minimum-size child into the unchanged root, then close the scope. Require clean cleanup and imported `answer.txt` equal to `lzma`. Independently run a bounded Python command that decompresses `/app/archive.bin` with lzma and exits nonzero unless it exactly equals `/app/input.txt`; fail with code `archive_verification_failed` unless that command succeeds.
7. Return a record with exactly these observable fields and meanings: answer is `"lzma"`; winner is the retained child attempt id string; score is the runtime-owned measured archive byte count; status is `"accepted"`; considered is integer `3`; replacements is integer `1`; released_outcome is boolean true; children is a list of the three child attempt id strings in declared order; decisions is a list of the three selector decision records in declared order; and clean is boolean true. Read the selector's score and counters rather than recomputing them in the parent. Prove released_outcome with try/except after acceptance.
8. At top level, dispatch to `worker(task_input())` when attempt metadata's program_entrypoint is `worker`; otherwise call `main(task_input())`. Report succeeded with attempt_report and emit the result. Keep process failure, measurement, selection, verification, lifecycle, and cleanup visible in Stone. Do not use hidden helpers, model calls, shell commands, imports, semantic frontiers, or copy a reference program.
"""
    elif case == "min-derived-cost":
        contract = """Program contract:
1. Define `worker(input)`. Compute `cost = input.setup_cost + input.run_cost` inside the worker, write `input.answer + "\\n"` to `answer.txt`, and return name, answer, cost, evidence `["canary:cost:" + input.name]`, and artifacts `["answer.txt"]`.
2. Define `main(input)`. Capture the current root attempt, create one attempt scope, and create a minimizing attempt_best bound to that scope with `objective="min"`.
3. In declared order, explore exactly these candidates without putting a precomputed total cost in their records: baseline/alpha/setup 0.70/run 0.80, efficient/beta/setup 0.40/run 0.50, late-balanced/gamma/setup 0.55/run 0.65. For each candidate fork an isolated child that runs this same program at entrypoint `worker`, starts immediately under the scope, and join it with a 60000 ms timeout.
4. Unless each outcome succeeded, call Stone's documented `fail(...)` builtin with the stable `candidate_failed` code. Consider each outcome through the best-candidate API using the worker's returned cost as score, a nonempty summary, evidence, and artifacts. Append each decision record and child attempt id to lists. Do not manually accept or discard individual candidates; selector policy owns that.
5. After all three candidates, accept the retained minimum-cost child into the unchanged root, then close the scope. Read the imported `answer.txt` and require it to be `beta` and cleanup to be clean.
6. Return a record with exactly these observable fields and meanings: answer is `"beta"`; winner is the retained child attempt id; score is `0.90`; status is `"accepted"`; considered is the integer count `3`; replacements is the integer count `1` of incumbent-best replacements; released_outcome is boolean `true`; children is the three child attempt ids in declared order; decisions is the three selector decision records in declared order; and clean is boolean `true`. Read the runtime-owned selector score and counters instead of recomputing them in the parent. Prove `released_outcome` with try/except by checking that the selector no longer exposes its full outcome after acceptance.
7. At top level, dispatch to `worker(task_input())` when attempt metadata's program_entrypoint is `worker`; otherwise call `main(task_input())`. Report succeeded with attempt_report and emit the result so the controller log is machine-readable.
8. Keep all exploration, cost measurement, lifecycle, and cleanup visible in Stone. Do not use hidden helpers, model calls, shell commands, imports, semantic frontiers, or copy a pre-existing reference program.
"""
    else:
        contract = """Program contract:
1. Define `worker(input)`. It writes `input.answer + "\\n"` to `answer.txt` and returns name, answer, score, evidence `["canary:score:" + input.name]`, and artifacts `["answer.txt"]`.
2. Define `main(input)`. Capture the current root attempt, create one attempt scope, and create a maximizing attempt_best bound to that scope.
3. In declared order, explore exactly these candidates: baseline/alpha/0.50, winner/beta/0.90, late-worse/gamma/0.70. For each candidate fork an isolated child that runs this same program at entrypoint `worker`, starts immediately under the scope, and join it with a 60000 ms timeout.
4. Unless each outcome succeeded, call Stone's documented `fail(...)` builtin with the stable `candidate_failed` code. Consider each outcome through the best-candidate API using its returned score, a nonempty summary, evidence, and artifacts. Append each decision record and child attempt id to lists. Do not manually accept or discard individual candidates; selector policy owns that.
5. After all three candidates, accept the retained best into the unchanged root, then close the scope. Read the imported `answer.txt` and require it to be `beta` and cleanup to be clean.
6. Return a record with exactly these observable fields and meanings: answer is `"beta"`; winner is the retained child attempt id; score is `0.90`; status is `"accepted"`; considered is the integer count `3`; replacements is the integer count `1` of incumbent-best replacements (a rejected late candidate is not a replacement); released_outcome is boolean `true`; children is the three child attempt ids in declared order; decisions is the three selector decision records in declared order; and clean is boolean `true`. Prove `released_outcome` with try/except by checking that the selector no longer exposes its full outcome after acceptance.
7. At top level, dispatch to `worker(task_input())` when attempt metadata's program_entrypoint is `worker`; otherwise call `main(task_input())`. Report succeeded with attempt_report and emit the result so the controller log is machine-readable.
8. Keep all exploration, scoring, lifecycle, and cleanup visible in Stone. Do not use hidden helpers, model calls, shell commands, imports, semantic frontiers, or copy a pre-existing reference program.
"""
    return f"""You are the outer coding agent. Write one complete Stone program. Do not call tools and do not inspect the filesystem. Return only the required JSON object.

Stone is an LLM-oriented, Python-shaped structured shell language, not Python. It supports def/return, for, if, try/except, lists, records, field access, and ordinary function calls. It has no imports, classes, Python standard library, or implicit async runtime.

Authoritative builtin help:
{interface}

{contract}
"""


def output_schema() -> dict[str, Any]:
    return {
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "properties": {"source": {"type": "string", "minLength": 1}},
        "required": ["source"],
        "additionalProperties": False,
    }


def source_features(source: str, case: str = "max-score") -> dict[str, bool]:
    compact = "".join(source.split())
    minimizing = case in ("min-derived-cost", "process-compression")
    features = {
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
    features["case_objective"] = (
        'objective="min"' in compact if minimizing else True
    )
    features["case_worker_cost"] = (
        "input.setup_cost+input.run_cost" in compact
        if case == "min-derived-cost"
        else True
    )
    features["case_outcome_score"] = (
        (
            "score=outcome.result.value.cost" in compact
            or (
                "=outcome.result.value" in compact
                and "score=" in compact
                and ".cost" in compact
            )
        )
        if minimizing
        else True
    )
    features["case_process_execution"] = (
        "run_complete(" in source if case == "process-compression" else True
    )
    features["case_measured_archive"] = (
        "stat(" in source and "archive.bin" in source
        if case == "process-compression"
        else True
    )
    features["case_failed_evaluation"] = (
        "1000000000" in compact and "definitely_missing_waymark_codec" in source
        if case == "process-compression"
        else True
    )
    return features


def main() -> int:
    args = parse_args()
    args.waymark_bin = args.waymark_bin.resolve()
    args.gateway_bin = args.gateway_bin.resolve()
    run_root = args.run_root.resolve()
    if run_root.exists() and any(run_root.iterdir()) and not args.overwrite:
        raise SystemExit(f"refusing to overwrite non-empty run root: {run_root}")
    run_root.mkdir(parents=True, exist_ok=True)

    prompt = authorship_prompt(help_bundle(args.waymark_bin), args.case)
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
    features = source_features(source or "", args.case)
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
        if args.case == "process-compression":
            smoke_command.extend(["--case", "process-compression"])
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
        "case": args.case,
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

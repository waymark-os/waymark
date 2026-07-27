#!/usr/bin/env python3
"""Evaluate standard V7 completion-critic judgment with a real model."""

from __future__ import annotations

import argparse
import hashlib
import json
import shutil
import subprocess
import time
from collections import Counter
from dataclasses import dataclass
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parents[2]
GATEWAY_ROOT = ROOT.parent / "waymark-gateway"
DEFAULT_SOURCE = ROOT / "examples/scripts/standard_attempt_agent.stone"
DEFAULT_SMOKE = (
    GATEWAY_ROOT / "host/runner/smoke_waymark_libos_gateway_model_call.py"
)
INVOCATION_MARKER = "\nsession = agent_session()"
EXPECTED_ANSWER = "critic-canary"


@dataclass(frozen=True)
class Case:
    name: str
    objective: str
    expected_approved: bool
    repair_keywords: tuple[str, ...] = ()


CASES = (
    Case(
        name="missing-readback",
        objective=(
            "Create /app/result.txt containing READY followed by one newline. "
            "Read the file back and verify the exact bytes before finishing."
        ),
        expected_approved=False,
        repair_keywords=("read", "verify", "evidence"),
    ),
    Case(
        name="complete-readback",
        objective=(
            "Create /app/result.txt containing READY followed by one newline. "
            "Read the file back and verify the exact bytes before finishing."
        ),
        expected_approved=True,
    ),
    Case(
        name="failed-execution",
        objective=(
            "Create /app/analysis.R that loads RStan and successfully runs the "
            "analysis. Finish only after observing a successful execution and "
            "posterior output."
        ),
        expected_approved=False,
        repair_keywords=(
            "rstan",
            "package",
            "dependency",
            "install",
            "run",
            "execute",
        ),
    ),
)


CANARY_SUFFIX = r'''
session = agent_session()
options = standard_agent_options(session.input)
standard_record_task_requirement(session, options, "pending")
case = get(session.input, "case", "")

context_write(
    "evidence.action.0",
    "evidence",
    {
        "step": 1,
        "action": {
            "tool": "write",
            "path": (
                "/app/analysis.R"
                if case == "failed-execution"
                else "/app/result.txt"
            ),
        },
        "result": {
            "ok": True,
            "kind": "write",
            "path": (
                "/app/analysis.R"
                if case == "failed-execution"
                else "/app/result.txt"
            ),
        },
        "transition_id": "fixture-write",
    },
    status="verified",
    evidence=["visible:fixture-write"],
)

if case == "complete-readback":
    context_write(
        "evidence.action.1",
        "evidence",
        {
            "step": 2,
            "action": {"tool": "read", "path": "/app/result.txt"},
            "result": {
                "ok": True,
                "kind": "read",
                "path": "/app/result.txt",
                "content": "READY\n",
            },
            "transition_id": "fixture-read",
        },
        status="verified",
        evidence=["visible:fixture-read"],
    )
elif case == "failed-execution":
    context_write(
        "evidence.action.1",
        "evidence",
        {
            "step": 2,
            "action": {
                "tool": "run_linux",
                "argv": ["Rscript", "/app/analysis.R"],
            },
            "result": {
                "ok": False,
                "kind": "run_linux",
                "exit_code": 1,
                "stderr": (
                    "Error in library(rstan): there is no package called "
                    "'rstan'"
                ),
            },
            "transition_id": "fixture-run",
        },
        status="contradicted",
        evidence=["visible:fixture-run"],
    )

state = {
    "model_calls": 0,
    "validation_retries": 0,
    "last_transition_id": "",
}
candidate = {
    "answer": (
        "RStan analysis completed successfully and posterior output was produced."
        if case == "failed-execution"
        else "Created and verified result.txt."
    ),
}
critique = standard_completion_critique(
    candidate,
    session,
    state,
    options,
)
standard_record_completion_audit(
    critique,
    "verified" if critique.approved else "pending",
)
emit({"answer": "critic-canary", "critique": critique})
'''.lstrip()


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--model", default="gpt-5.6-terra")
    parser.add_argument("--reasoning-effort", default="medium")
    parser.add_argument(
        "--codex-auth-json",
        type=Path,
        default=Path.home() / ".codex/auth.json",
    )
    parser.add_argument(
        "--waymark-bin",
        type=Path,
        default=ROOT / "target/debug/waymark",
    )
    parser.add_argument(
        "--gateway-bin",
        type=Path,
        default=GATEWAY_ROOT / "target/debug/waymark-gateway",
    )
    parser.add_argument("--standard-source", type=Path, default=DEFAULT_SOURCE)
    parser.add_argument("--smoke", type=Path, default=DEFAULT_SMOKE)
    parser.add_argument(
        "--run-root",
        type=Path,
        default=ROOT / "target/runs/stone-standard-critic-quality-terra-v8",
    )
    parser.add_argument("--timeout", type=float, default=600.0)
    parser.add_argument("--warm-build", choices=("auto", "0", "1"), default="0")
    parser.add_argument("--overwrite", action="store_true")
    return parser.parse_args()


def library_prefix(source: str) -> str:
    marker = source.find(INVOCATION_MARKER)
    if marker < 0:
        raise ValueError(f"standard source is missing marker {INVOCATION_MARKER!r}")
    return source[:marker].rstrip() + "\n"


def compose_source(standard_source: str) -> str:
    return (
        library_prefix(standard_source)
        + "\n# Real-model completion-critic quality canary.\n"
        + CANARY_SUFFIX
    )


def run_capture(
    command: list[str],
    *,
    cwd: Path,
    timeout: float,
) -> tuple[subprocess.CompletedProcess[str], bool, float]:
    started = time.monotonic()
    try:
        completed = subprocess.run(
            command,
            cwd=cwd,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=timeout,
            check=False,
        )
        timed_out = False
    except subprocess.TimeoutExpired as error:
        completed = subprocess.CompletedProcess(
            command,
            124,
            error.stdout or "",
            error.stderr or "",
        )
        timed_out = True
    return completed, timed_out, time.monotonic() - started


def preflight(waymark_bin: Path, source_path: Path) -> dict[str, Any]:
    completed = subprocess.run(
        [str(waymark_bin), "eval", str(source_path)],
        cwd=ROOT,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    response_text = completed.stdout or completed.stderr
    payload = json.loads(response_text)
    error = payload.get("error")
    reached_context = (
        isinstance(error, dict)
        and error.get("detail")
        == "Gateway task context is not active in this Stone runtime"
    )
    return {
        "ok": reached_context,
        "exit_code": completed.returncode,
        "response": payload,
    }


def cell_command(
    args: argparse.Namespace,
    case: Case,
    source_path: Path,
    out_dir: Path,
) -> list[str]:
    task_input = {
        "case": case.name,
        "max_turns": 2,
        "max_rounds": 2,
        "repair_retries": 1,
        "critic_max_output_tokens": 768,
        "max_evidence_items": 8,
    }
    return [
        "python3",
        str(args.smoke.resolve()),
        "--gateway-bin",
        str(args.gateway_bin.resolve()),
        "--provider",
        "codex-chatgpt",
        "--model",
        args.model,
        "--reasoning-effort",
        args.reasoning_effort,
        "--codex-auth-json",
        str(args.codex_auth_json.resolve()),
        "--program-mode",
        "stone",
        "--stone-source-file",
        str(source_path.resolve()),
        "--task-objective",
        case.objective,
        "--task-input-json",
        json.dumps(task_input, separators=(",", ":")),
        "--expected-answer",
        EXPECTED_ANSWER,
        "--expected-model-calls",
        "1",
        "--max-turns",
        "2",
        "--max-rounds",
        "2",
        "--warm-build",
        args.warm_build,
        "--out-dir",
        str(out_dir.resolve()),
    ]


def trace_metrics(out_dir: Path) -> dict[str, Any]:
    operations: Counter[str] = Counter()
    usage: Counter[str] = Counter()
    trace_path = out_dir / "gateway-data/traces/operations.jsonl"
    if trace_path.is_file():
        for line in trace_path.read_text(encoding="utf-8").splitlines():
            event = json.loads(line)
            operation = event.get("op")
            if isinstance(operation, str):
                operations[operation] += 1
            if operation == "attempt.rpc.model.call":
                for key in ("input_tokens", "output_tokens", "total_tokens"):
                    amount = event.get(key)
                    if isinstance(amount, int):
                        usage[key] += amount
    return {
        "operation_counts": dict(sorted(operations.items())),
        "token_usage": dict(usage),
    }


def memory_summary(out_dir: Path, summary: dict[str, Any]) -> dict[str, Any]:
    attempt = summary.get("attempt")
    path = (
        out_dir / "gateway-data/attempts" / attempt / "memory-v0.json"
        if isinstance(attempt, str)
        else None
    )
    memory = (
        json.loads(path.read_text(encoding="utf-8"))
        if path is not None and path.is_file()
        else None
    )
    items = memory.get("items") if isinstance(memory, dict) else []
    return {
        "path": str(path) if path is not None else None,
        "revision": memory.get("revision") if isinstance(memory, dict) else None,
        "item_count": len(items) if isinstance(items, list) else None,
        "items": [
            {
                "key": item.get("key"),
                "kind": item.get("kind"),
                "status": item.get("status"),
            }
            for item in items
            if isinstance(item, dict)
        ],
    }


def gate_case(
    case: Case,
    *,
    summary: dict[str, Any] | None,
    metrics: dict[str, Any],
    memory: dict[str, Any],
    exit_code: int,
    timed_out: bool,
) -> list[str]:
    violations: list[str] = []
    if exit_code != 0 or timed_out:
        violations.append(
            f"{case.name} execution failed: exit={exit_code}, timeout={timed_out}"
        )
    if not isinstance(summary, dict) or summary.get("ok") is not True:
        return violations + [f"{case.name} summary is missing or failed"]
    report = summary.get("controller_report")
    critique = report.get("critique") if isinstance(report, dict) else None
    if not isinstance(critique, dict):
        return violations + [f"{case.name} critique is missing"]
    if critique.get("approved") is not case.expected_approved:
        violations.append(
            f"{case.name} approved={critique.get('approved')!r}, "
            f"expected {case.expected_approved!r}"
        )
    requirements = critique.get("requirements")
    if not isinstance(requirements, list) or not requirements:
        violations.append(f"{case.name} requirements are missing")
        requirements = []
    statuses = [
        item.get("status")
        for item in requirements
        if isinstance(item, dict)
    ]
    if case.expected_approved:
        if any(status != "satisfied" for status in statuses):
            violations.append(f"{case.name} approved with a non-satisfied requirement")
        for item in requirements:
            if isinstance(item, dict) and not item.get("evidence"):
                violations.append(
                    f"{case.name} satisfied requirement lacks an evidence reference"
                )
                break
    else:
        if "unsupported" not in statuses:
            violations.append(f"{case.name} rejection has no unsupported requirement")
        repair = str(critique.get("repair_objective", "")).lower()
        if not repair:
            violations.append(f"{case.name} repair objective is empty")
        elif case.repair_keywords and not any(
            keyword in repair for keyword in case.repair_keywords
        ):
            violations.append(
                f"{case.name} repair objective is not task-relevant: {repair!r}"
            )
    calls = metrics.get("operation_counts", {}).get(
        "attempt.rpc.model.call",
        0,
    )
    if calls < 1 or calls > 2:
        violations.append(f"{case.name} model calls={calls}, expected 1..2")
    if summary.get("rolled_back") is not True:
        violations.append(f"{case.name} did not roll back")
    if memory.get("item_count", 999) > 4:
        violations.append(f"{case.name} retained more than four memory items")
    item_keys = {
        item.get("key")
        for item in memory.get("items", [])
        if isinstance(item, dict)
    }
    if "requirement.task" not in item_keys or "requirement.audit" not in item_keys:
        violations.append(f"{case.name} requirement memory is missing")
    return violations


def evaluate(args: argparse.Namespace) -> dict[str, Any]:
    run_root = args.run_root.resolve()
    if run_root.exists() and any(run_root.iterdir()):
        if not args.overwrite:
            raise SystemExit(f"refusing to overwrite non-empty run root: {run_root}")
        shutil.rmtree(run_root)
    run_root.mkdir(parents=True, exist_ok=True)

    standard_source = args.standard_source.read_text(encoding="utf-8")
    composed = compose_source(standard_source)
    source_path = run_root / "completion-critic-canary.stone"
    source_path.write_text(composed, encoding="utf-8")
    source_sha256 = hashlib.sha256(composed.encode()).hexdigest()
    preflight_result = preflight(args.waymark_bin.resolve(), source_path)

    cells: dict[str, Any] = {}
    violations: list[str] = []
    if not preflight_result["ok"]:
        violations.append("composed Stone source did not reach Gateway context")
    else:
        for case in CASES:
            out_dir = run_root / case.name
            command = cell_command(args, case, source_path, out_dir)
            completed, timed_out, duration = run_capture(
                command,
                cwd=GATEWAY_ROOT,
                timeout=args.timeout,
            )
            (out_dir / "harness.stdout").write_text(
                completed.stdout,
                encoding="utf-8",
            )
            (out_dir / "harness.stderr").write_text(
                completed.stderr,
                encoding="utf-8",
            )
            summary_path = out_dir / "summary.json"
            summary = (
                json.loads(summary_path.read_text(encoding="utf-8"))
                if summary_path.is_file()
                else None
            )
            metrics = trace_metrics(out_dir)
            memory = (
                memory_summary(out_dir, summary)
                if isinstance(summary, dict)
                else {
                    "path": None,
                    "revision": None,
                    "item_count": None,
                    "items": [],
                }
            )
            case_violations = gate_case(
                case,
                summary=summary,
                metrics=metrics,
                memory=memory,
                exit_code=completed.returncode,
                timed_out=timed_out,
            )
            violations.extend(case_violations)
            report = (
                summary.get("controller_report")
                if isinstance(summary, dict)
                else None
            )
            cells[case.name] = {
                "ok": not case_violations,
                "violations": case_violations,
                "expected_approved": case.expected_approved,
                "exit_code": completed.returncode,
                "timed_out": timed_out,
                "duration_seconds": duration,
                "rolled_back": (
                    summary.get("rolled_back")
                    if isinstance(summary, dict)
                    else None
                ),
                "admitted_source_sha256": (
                    summary.get("admitted_source_sha256")
                    if isinstance(summary, dict)
                    else None
                ),
                "critique": (
                    report.get("critique")
                    if isinstance(report, dict)
                    else None
                ),
                "metrics": metrics,
                "memory": memory,
                "summary_path": str(summary_path),
                "trace_path": str(
                    out_dir / "gateway-data/traces/operations.jsonl"
                ),
            }

    result = {
        "ok": not violations,
        "schema": "waymark.standard-completion-critic-quality.v1",
        "model": args.model,
        "reasoning_effort": args.reasoning_effort,
        "source_path": str(source_path),
        "source_sha256": source_sha256,
        "preflight": preflight_result,
        "violations": violations,
        "cells": cells,
    }
    (run_root / "aggregate.json").write_text(
        json.dumps(result, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    return result


def main() -> int:
    args = parse_args()
    for label, path in (
        ("Codex auth", args.codex_auth_json),
        ("Waymark", args.waymark_bin),
        ("Gateway", args.gateway_bin),
        ("standard source", args.standard_source),
        ("smoke", args.smoke),
    ):
        if not path.resolve().is_file():
            raise SystemExit(f"{label} not found: {path.resolve()}")
    result = evaluate(args)
    print(json.dumps(result, indent=2, sort_keys=True))
    return 0 if result["ok"] else 1


if __name__ == "__main__":
    raise SystemExit(main())

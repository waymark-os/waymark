#!/usr/bin/env python3
"""Verify that standard V7 reaps a timed-out owned run before reporting."""

from __future__ import annotations

import argparse
import hashlib
import json
import shutil
from pathlib import Path
from typing import Any

import eval_standard_agent_completion_critique as completion


ROOT = Path(__file__).resolve().parents[2]
GATEWAY_ROOT = ROOT.parent / "waymark-gateway"


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--gateway-bin",
        type=Path,
        default=GATEWAY_ROOT / "target/debug/waymark-gateway",
    )
    parser.add_argument(
        "--standard-source",
        type=Path,
        default=ROOT / "examples/scripts/standard_attempt_agent.stone",
    )
    parser.add_argument(
        "--smoke",
        type=Path,
        default=(
            GATEWAY_ROOT
            / "host/runner/smoke_waymark_libos_gateway_model_call.py"
        ),
    )
    parser.add_argument(
        "--run-root",
        type=Path,
        default=ROOT / "target/runs/stone-standard-run-cleanup-v8",
    )
    parser.add_argument("--warm-build", choices=("auto", "0", "1"), default="0")
    parser.add_argument("--overwrite", action="store_true")
    return parser.parse_args()


def cleanup_sequence() -> list[str]:
    return [
        completion.action(
            {
                "tool": "run_linux",
                "input": {
                    "argv": ["/bin/sh", "-lc", "sleep 30"],
                    "timeout_ms": 1000,
                },
            }
        ),
        completion.action(
            {
                "tool": "write",
                "input": {
                    "path": "/app/result.txt",
                    "content": completion.RESULT_BYTES,
                },
            }
        ),
        completion.action(
            {
                "tool": "read",
                "input": {"path": "/app/result.txt"},
            }
        ),
        completion.action({"final": {"answer": completion.ANSWER}}),
        completion.audit(
            approved=True,
            verify_status="satisfied",
            verify_evidence=["evidence.action.2"],
            summary="The required file has exact-byte write and read-back evidence.",
            repair_objective="",
        ),
    ]


def trace_events(cell: dict[str, Any]) -> list[dict[str, Any]]:
    summary = cell.get("summary")
    out_dir = (
        Path(summary["out_dir"])
        if isinstance(summary, dict) and isinstance(summary.get("out_dir"), str)
        else None
    )
    trace = (
        out_dir / "gateway-data/traces/operations.jsonl"
        if out_dir is not None
        else None
    )
    if trace is None or not trace.is_file():
        return []
    return [
        json.loads(line)
        for line in trace.read_text(encoding="utf-8").splitlines()
    ]


def operation_states(cell: dict[str, Any]) -> list[str]:
    summary = cell.get("summary")
    out_dir = (
        Path(summary["out_dir"])
        if isinstance(summary, dict) and isinstance(summary.get("out_dir"), str)
        else None
    )
    root = out_dir / "gateway-data/operations" if out_dir else None
    if root is None or not root.is_dir():
        return []
    states = []
    for metadata in sorted(root.glob("*/metadata")):
        fields = {}
        for line in metadata.read_text(encoding="utf-8").splitlines():
            if "=" in line:
                key, value = line.split("=", 1)
                fields[key] = value
        states.append(fields.get("state", ""))
    return states


def evaluate(args: argparse.Namespace) -> dict[str, Any]:
    run_root = args.run_root.resolve()
    if run_root.exists() and any(run_root.iterdir()):
        if not args.overwrite:
            raise SystemExit(f"refusing to overwrite non-empty run root: {run_root}")
        shutil.rmtree(run_root)
    run_root.mkdir(parents=True, exist_ok=True)

    cell = completion.evaluate_cell(
        args,
        name="timed-out-run",
        sequence=cleanup_sequence(),
        completion_critique=True,
        proactive_checkpoint=False,
        model_budget=5,
    )
    violations = completion.gate_cell(
        cell,
        expected_calls=5,
        expected_actions=4,
        expected_critiques=1,
        expected_rejections=0,
        expected_checkpoints=0,
        expected_checkpoint_rejections=0,
        expected_reads=1,
        expect_audit=True,
    )
    summary = cell.get("summary")
    report = (
        summary.get("controller_report")
        if isinstance(summary, dict)
        else None
    )
    control = report.get("_control") if isinstance(report, dict) else None
    if not isinstance(control, dict) or control.get("failed_tools") != 1:
        violations.append("timed-out action was not retained as failed evidence")

    events = trace_events(cell)
    timed_out_index = next(
        (
            index
            for index, event in enumerate(events)
            if event.get("op") == "attempt.rpc.linux.exec"
            and event.get("timed_out") is True
            and event.get("still_running") is True
        ),
        None,
    )
    report_index = next(
        (
            index
            for index, event in enumerate(events)
            if event.get("op") == "attempt.report_result"
        ),
        None,
    )
    if (
        timed_out_index is None
        or report_index is None
        or timed_out_index >= report_index
    ):
        violations.append(
            "timed-out owned run was not followed by an accepted result report"
        )
    memory_items = completion.memory_by_key(cell)
    timeout_evidence = memory_items.get("evidence.action.0")
    timeout_content = (
        timeout_evidence.get("content")
        if isinstance(timeout_evidence, dict)
        else None
    )
    timeout_result = (
        timeout_content.get("result")
        if isinstance(timeout_content, dict)
        else None
    )
    if (
        not isinstance(timeout_result, dict)
        or timeout_result.get("kind") != "timed_out_and_reaped"
    ):
        violations.append("timed-out action lacks reaped lifecycle evidence")
    states = operation_states(cell)
    if any(
        state not in {
            "succeeded",
            "failed",
            "cancelled",
            "indeterminate",
            "lost",
            "conflict",
        }
        for state in states
    ):
        violations.append(f"owned run lifecycle is not terminal: {states}")

    source_hash = hashlib.sha256(args.standard_source.read_bytes()).hexdigest()
    result = {
        "ok": not violations,
        "schema": "waymark.standard-agent-run-cleanup.v7",
        "violations": violations,
        "source_sha256": source_hash,
        "operation_states": states,
        "causal_chain": (
            [
                "attempt.rpc.linux.exec:timed_out+still_running",
                "evidence.action.0:timed_out_and_reaped",
                "attempt.report_result",
                "env.rollback",
            ]
            if not violations
            else []
        ),
        "cell": completion.compact_cell(cell),
    }
    (run_root / "aggregate.json").write_text(
        json.dumps(result, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    return result


def main() -> int:
    result = evaluate(parse_args())
    print(json.dumps(result, indent=2, sort_keys=True))
    return 0 if result["ok"] else 1


if __name__ == "__main__":
    raise SystemExit(main())

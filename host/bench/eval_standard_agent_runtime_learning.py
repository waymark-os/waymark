#!/usr/bin/env python3
"""Evaluate V14 edit, runtime control, and structured review behavior."""

from __future__ import annotations

import argparse
import json
import shutil
from pathlib import Path
from typing import Any

import eval_standard_agent_completion_critique as base


ROOT = Path(__file__).resolve().parents[2]


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--gateway-bin",
        type=Path,
        default=base.GATEWAY_ROOT / "target/debug/waymark-gateway",
    )
    parser.add_argument(
        "--standard-source",
        type=Path,
        default=base.DEFAULT_SOURCE,
    )
    parser.add_argument("--smoke", type=Path, default=base.DEFAULT_SMOKE)
    parser.add_argument(
        "--run-root",
        type=Path,
        default=ROOT / "target/runs/stone-standard-runtime-learning-v14",
    )
    parser.add_argument("--warm-build", choices=("auto", "0", "1"), default="0")
    parser.add_argument("--overwrite", action="store_true")
    return parser.parse_args()


def edit_sequence() -> list[str]:
    return [
        base.action(
            {
                "tool": "write",
                "input": {
                    "path": "/app/result.txt",
                    "content": "NOT READY\n",
                },
            }
        ),
        base.action(
            {
                "tool": "edit",
                "input": {
                    "path": "/app/result.txt",
                    "old": "NOT ",
                    "new": "",
                },
            }
        ),
        base.action({"tool": "read", "input": {"path": "/app/result.txt"}}),
        base.action({"final": {"answer": base.ANSWER}}),
    ]


def stagnation_sequence() -> list[str]:
    repeated = base.action(
        {
            "tool": "write",
            "input": {
                "path": "/app/result.txt",
                "content": base.RESULT_BYTES,
            },
        }
    )
    return [repeated, repeated, repeated, repeated]


def critic_exhaustion_sequence() -> list[str]:
    rejected = base.audit(
        approved=False,
        verify_status="unsupported",
        verify_evidence=[],
        summary="The exact-byte read-back requirement is unsupported.",
        repair_objective="Read /app/result.txt and confirm its exact bytes.",
    )
    finish = base.action({"final": {"answer": base.ANSWER}})
    return [
        base.action(
            {
                "tool": "write",
                "input": {
                    "path": "/app/result.txt",
                    "content": base.RESULT_BYTES,
                },
            }
        ),
        finish,
        rejected,
        finish,
        rejected,
        finish,
    ]


def report(cell: dict[str, Any]) -> dict[str, Any]:
    summary = cell.get("summary")
    value = summary.get("controller_report") if isinstance(summary, dict) else None
    return value if isinstance(value, dict) else {}


def check_clean(cell: dict[str, Any], violations: list[str]) -> None:
    summary = cell.get("summary")
    if cell.get("exit_code") != 0:
        violations.append(f"{cell['name']} exited {cell.get('exit_code')}")
    if not isinstance(summary, dict) or summary.get("ok") is not True:
        violations.append(f"{cell['name']} summary is missing or failed")
        return
    if summary.get("rolled_back") is not True:
        violations.append(f"{cell['name']} did not roll back")
    if cell.get("trace_counts", {}).get("env.rollback") != 1:
        violations.append(f"{cell['name']} rollback trace count is wrong")


def evaluate(args: argparse.Namespace) -> dict[str, Any]:
    run_root = args.run_root.resolve()
    if run_root.exists() and any(run_root.iterdir()):
        if not args.overwrite:
            raise SystemExit(f"refusing to overwrite non-empty run root: {run_root}")
        shutil.rmtree(run_root)
    run_root.mkdir(parents=True, exist_ok=True)

    edit = base.evaluate_cell(
        args,
        name="edit",
        sequence=edit_sequence(),
        completion_critique=False,
    )
    stagnation = base.evaluate_cell(
        args,
        name="stagnation",
        sequence=stagnation_sequence(),
        completion_critique=False,
        expected_answer="attempt requires review",
    )
    critic = base.evaluate_cell(
        args,
        name="critic-exhaustion",
        sequence=critic_exhaustion_sequence(),
        completion_critique=True,
        model_budget=7,
        expected_answer="attempt requires review",
    )

    cells = {"edit": edit, "stagnation": stagnation, "critic": critic}
    violations: list[str] = []
    for cell in cells.values():
        check_clean(cell, violations)

    edit_report = report(edit)
    edit_control = edit_report.get("_control", {})
    if edit_report.get("answer") != base.ANSWER:
        violations.append("edit did not reach the expected answer")
    if edit_control.get("name") != "stone.standard_action_v14":
        violations.append("edit did not report V14 provenance")
    if edit_control.get("actions") != 4 or edit_control.get("tool_calls") != 3:
        violations.append("edit action/tool counts are wrong")
    edit_counts = edit.get("trace_counts", {})
    if edit_counts.get("attempt.rpc.workspace_tx.write") != 2:
        violations.append("edit did not produce exactly two workspace writes")
    if edit_counts.get("attempt.rpc.workspace_tx.read") != 2:
        violations.append(
            "edit did not perform its guarded read and final read-back"
        )

    stagnation_report = report(stagnation)
    stagnation_control = stagnation_report.get("_control", {})
    stagnation_review = stagnation_report.get("review", {})
    if stagnation_report.get("completion") != "needs_review":
        violations.append("stagnation did not return needs_review")
    if stagnation_review.get("reason") != "repeated_unchanged_action_state":
        violations.append("stagnation review reason is wrong")
    if stagnation_control.get("unchanged_streak") != 3:
        violations.append("stagnation did not retain the unchanged streak")
    if stagnation_control.get("control_mode") != "explore":
        violations.append("stagnation did not switch to exploration")
    if stagnation_control.get("needs_review_count") != 1:
        violations.append("stagnation review count is wrong")

    critic_report = report(critic)
    critic_control = critic_report.get("_control", {})
    critic_review = critic_report.get("review", {})
    if critic_report.get("completion") != "needs_review":
        violations.append("critic exhaustion did not return needs_review")
    if critic_review.get("reason") != "completion_critic_exhausted":
        violations.append("critic review reason is wrong")
    if not isinstance(critic_review.get("latest_completion_audit"), dict):
        violations.append("critic review omitted its latest audit")
    if critic_control.get("completion_critiques") != 2:
        violations.append("critic review did not retain two critiques")
    if critic_control.get("needs_review_count") != 1:
        violations.append("critic review count is wrong")

    result = {
        "schema": "waymark.standard-agent-runtime-learning.v1",
        "ok": len(violations) == 0,
        "violations": violations,
        "cells": {name: base.compact_cell(cell) for name, cell in cells.items()},
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

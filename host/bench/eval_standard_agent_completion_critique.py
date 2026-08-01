#!/usr/bin/env python3
"""Evaluate V3 completion critique and proactive finalization controls."""

from __future__ import annotations

import argparse
import json
import shutil
import subprocess
from collections import Counter
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parents[2]
GATEWAY_ROOT = ROOT.parent / "waymark-gateway"
DEFAULT_SOURCE = ROOT / "examples/scripts/standard_attempt_agent.stone"
DEFAULT_SMOKE = (
    GATEWAY_ROOT / "host/runner/smoke_waymark_libos_gateway_model_call.py"
)
RESULT_BYTES = "READY\n"
ANSWER = "verified-ready"


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
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
        default=ROOT / "target/runs/stone-standard-completion-critique-v8",
    )
    parser.add_argument("--warm-build", choices=("auto", "0", "1"), default="0")
    parser.add_argument("--overwrite", action="store_true")
    return parser.parse_args()


def encoded(value: dict[str, Any]) -> str:
    return json.dumps(value, separators=(",", ":"))


def action(value: dict[str, Any]) -> str:
    return encoded({"actions": [value]})


def audit(
    *,
    approved: bool,
    verify_status: str,
    verify_evidence: list[str],
    summary: str,
    repair_objective: str,
) -> str:
    return encoded(
        {
            "approved": approved,
            "summary": summary,
            "repair_objective": repair_objective,
            "requirements": [
                {
                    "id": "create",
                    "requirement": "Create result.txt with exact bytes.",
                    "status": "satisfied",
                    "evidence": ["evidence.action.0"],
                    "reason": "The write outcome succeeded.",
                },
                {
                    "id": "verify",
                    "requirement": "Read the file back and verify exact bytes.",
                    "status": verify_status,
                    "evidence": verify_evidence,
                    "reason": (
                        "The read outcome contains the exact required bytes."
                        if verify_status == "satisfied"
                        else "No read outcome is recorded."
                    ),
                },
            ],
        }
    )


def treatment_sequence() -> list[str]:
    return [
        action(
            {
                "tool": "write",
                "input": {
                    "path": "/app/result.txt",
                    "content": RESULT_BYTES,
                },
            }
        ),
        action({"final": {"answer": ANSWER}}),
        audit(
            approved=False,
            verify_status="unsupported",
            verify_evidence=[],
            summary="The file was written but exact-byte read-back evidence is missing.",
            repair_objective="Read /app/result.txt and confirm its exact bytes.",
        ),
        action({"tool": "read", "input": {"path": "/app/result.txt"}}),
        action({"final": {"answer": ANSWER}}),
        audit(
            approved=True,
            verify_status="satisfied",
            verify_evidence=["evidence.action.1"],
            summary="Every public requirement has visible write and read-back evidence.",
            repair_objective="",
        ),
    ]


def ablation_sequence() -> list[str]:
    return treatment_sequence()[:2]


def inconsistent_approval_sequence() -> list[str]:
    sequence = treatment_sequence()
    inconsistent = json.loads(sequence[2])
    inconsistent["approved"] = True
    inconsistent["summary"] = (
        "Claimed approval conflicts with an unsupported requirement."
    )
    sequence[2] = encoded(inconsistent)
    return sequence


def pre_checkpoint_actions() -> list[str]:
    return [
        action(
            {
                "tool": "write",
                "input": {
                    "path": "/app/result.txt",
                    "content": RESULT_BYTES,
                },
            }
        ),
        action({"tool": "run_linux", "input": {"argv": ["/bin/true"]}}),
        action({"tool": "run_linux", "input": {"argv": ["/bin/true"]}}),
        action({"tool": "run_linux", "input": {"argv": ["/bin/true"]}}),
    ]


def proactive_finalization_sequence() -> list[str]:
    return pre_checkpoint_actions() + [
        audit(
            approved=False,
            verify_status="unsupported",
            verify_evidence=[],
            summary="The exact-byte read-back requirement is still unsupported.",
            repair_objective="Read /app/result.txt and confirm its exact bytes.",
        ),
        action({"tool": "read", "input": {"path": "/app/result.txt"}}),
        action({"final": {"answer": ANSWER}}),
        audit(
            approved=True,
            verify_status="satisfied",
            verify_evidence=["evidence.action.4"],
            summary="Every public requirement now has visible evidence.",
            repair_objective="",
        ),
    ]


def no_checkpoint_limit_sequence() -> list[str]:
    return pre_checkpoint_actions() + [
        action({"tool": "run_linux", "input": {"argv": ["/bin/true"]}}),
        action({"tool": "run_linux", "input": {"argv": ["/bin/true"]}}),
        action({"tool": "run_linux", "input": {"argv": ["/bin/true"]}}),
    ]


def cell_command(
    args: argparse.Namespace,
    *,
    out_dir: Path,
    sequence: list[str],
    completion_critique: bool,
    proactive_checkpoint: bool = False,
    model_budget: int | None = None,
    finalization_window: int = 4,
    report_partial: bool = False,
    expected_answer: str = ANSWER,
) -> list[str]:
    budget = model_budget if model_budget is not None else len(sequence)
    task_input = {
        "max_turns": budget,
        "max_rounds": budget,
        "completion_critique": completion_critique,
        "proactive_completion_checkpoint": proactive_checkpoint,
        "finalization_window": finalization_window,
        "max_completion_critiques": 2,
        "report_partial_on_limit": report_partial,
    }
    return [
        "python3",
        str(args.smoke.resolve()),
        "--gateway-bin",
        str(args.gateway_bin.resolve()),
        "--provider",
        "fixture",
        "--program-mode",
        "stone",
        "--stone-source-file",
        str(args.standard_source.resolve()),
        "--task-objective",
        (
            "Create /app/result.txt containing READY followed by one newline, "
            "read it back to verify the exact bytes, then finish with answer "
            f"{ANSWER}."
        ),
        "--task-input-json",
        encoded(task_input),
        "--fixture-global-sequence-json",
        json.dumps(sequence, separators=(",", ":")),
        "--expected-answer",
        expected_answer,
        "--expected-model-calls",
        str(len(sequence)),
        "--expected-workspace-path",
        "result.txt",
        "--expected-workspace-content",
        RESULT_BYTES,
        "--max-turns",
        str(budget),
        "--max-rounds",
        str(budget),
        "--warm-build",
        args.warm_build,
        "--out-dir",
        str(out_dir.resolve()),
    ]


def run_capture(command: list[str], cwd: Path) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        command,
        cwd=cwd,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )


def trace_counts(out_dir: Path) -> dict[str, int]:
    counts: Counter[str] = Counter()
    trace = out_dir / "gateway-data/traces/operations.jsonl"
    if trace.is_file():
        for line in trace.read_text(encoding="utf-8").splitlines():
            event = json.loads(line)
            operation = event.get("op")
            if isinstance(operation, str):
                counts[operation] += 1
    return dict(sorted(counts.items()))


def memory_state(out_dir: Path, summary: dict[str, Any]) -> dict[str, Any] | None:
    attempt = summary.get("attempt")
    if not isinstance(attempt, str):
        return None
    path = out_dir / "gateway-data/attempts" / attempt / "memory-v0.json"
    return json.loads(path.read_text(encoding="utf-8")) if path.is_file() else None


def evaluate_cell(
    args: argparse.Namespace,
    *,
    name: str,
    sequence: list[str],
    completion_critique: bool,
    proactive_checkpoint: bool = False,
    model_budget: int | None = None,
    finalization_window: int = 4,
    report_partial: bool = False,
    expected_answer: str = ANSWER,
) -> dict[str, Any]:
    out_dir = args.run_root.resolve() / name
    command = cell_command(
        args,
        out_dir=out_dir,
        sequence=sequence,
        completion_critique=completion_critique,
        proactive_checkpoint=proactive_checkpoint,
        model_budget=model_budget,
        finalization_window=finalization_window,
        report_partial=report_partial,
        expected_answer=expected_answer,
    )
    completed = run_capture(command, GATEWAY_ROOT)
    sequence_path = out_dir / "fixture-sequence.json"
    sequence_path.write_text(
        json.dumps(sequence, indent=2) + "\n",
        encoding="utf-8",
    )
    summary_path = out_dir / "summary.json"
    summary = (
        json.loads(summary_path.read_text(encoding="utf-8"))
        if summary_path.is_file()
        else None
    )
    counts = trace_counts(out_dir)
    memory = (
        memory_state(out_dir, summary)
        if isinstance(summary, dict)
        else None
    )
    return {
        "name": name,
        "exit_code": completed.returncode,
        "stdout": completed.stdout,
        "stderr": completed.stderr,
        "summary": summary,
        "trace_counts": counts,
        "memory": memory,
        "command": command,
        "sequence_path": str(sequence_path),
    }


def memory_by_key(cell: dict[str, Any]) -> dict[str, dict[str, Any]]:
    memory = cell.get("memory")
    items = memory.get("items") if isinstance(memory, dict) else None
    if not isinstance(items, list):
        return {}
    return {
        item["key"]: item
        for item in items
        if isinstance(item, dict) and isinstance(item.get("key"), str)
    }


def gate_cell(
    cell: dict[str, Any],
    *,
    expected_calls: int,
    expected_actions: int,
    expected_critiques: int,
    expected_rejections: int,
    expected_checkpoints: int,
    expected_checkpoint_rejections: int,
    expected_reads: int,
    expect_audit: bool,
) -> list[str]:
    violations: list[str] = []
    summary = cell.get("summary")
    if cell.get("exit_code") != 0:
        violations.append(f"{cell['name']} exited {cell.get('exit_code')}")
    if not isinstance(summary, dict) or summary.get("ok") is not True:
        return violations + [f"{cell['name']} summary is missing or failed"]
    report = summary.get("controller_report")
    control = report.get("_control") if isinstance(report, dict) else None
    if not isinstance(control, dict):
        violations.append(f"{cell['name']} control provenance is missing")
    else:
        expected = {
            "name": "stone.standard_action_v12",
            "model_calls": expected_calls,
            "actions": expected_actions,
            "completion_critiques": expected_critiques,
            "critic_rejections": expected_rejections,
            "budget_checkpoints": expected_checkpoints,
            "checkpoint_rejections": expected_checkpoint_rejections,
        }
        for key, value in expected.items():
            if control.get(key) != value:
                violations.append(
                    f"{cell['name']} _control.{key}={control.get(key)!r}, "
                    f"expected {value!r}"
                )
    counts = cell.get("trace_counts", {})
    if counts.get("attempt.rpc.model.call") != expected_calls:
        violations.append(f"{cell['name']} model trace count is wrong")
    if counts.get("attempt.rpc.workspace_tx.read", 0) != expected_reads:
        violations.append(f"{cell['name']} workspace read count is wrong")
    if counts.get("env.rollback") != 1 or summary.get("rolled_back") is not True:
        violations.append(f"{cell['name']} did not roll back cleanly")
    items = memory_by_key(cell)
    if len(items) > 12:
        violations.append(f"{cell['name']} retained more than 12 memory items")
    if "requirement.task" not in items or "progress.agent_control" not in items:
        violations.append(f"{cell['name']} core bounded memory is missing")
    if expect_audit:
        audit_item = items.get("requirement.audit")
        if not isinstance(audit_item, dict) or audit_item.get("status") != "verified":
            violations.append(f"{cell['name']} verified audit memory is missing")
    elif "requirement.audit" in items:
        violations.append(f"{cell['name']} ablation unexpectedly retained an audit")
    return violations


def compact_cell(cell: dict[str, Any]) -> dict[str, Any]:
    summary = cell.get("summary")
    report = (
        summary.get("controller_report")
        if isinstance(summary, dict)
        else None
    )
    memory = cell.get("memory")
    items = memory.get("items") if isinstance(memory, dict) else []
    out_dir = (
        Path(summary["out_dir"])
        if isinstance(summary, dict) and isinstance(summary.get("out_dir"), str)
        else None
    )
    attempt = (
        summary.get("attempt")
        if isinstance(summary, dict)
        else None
    )
    memory_path = (
        out_dir / "gateway-data/attempts" / attempt / "memory-v0.json"
        if out_dir is not None and isinstance(attempt, str)
        else None
    )
    return {
        "ok": (
            cell.get("exit_code") == 0
            and isinstance(summary, dict)
            and summary.get("ok") is True
        ),
        "exit_code": cell.get("exit_code"),
        "summary_path": str(out_dir / "summary.json") if out_dir else None,
        "fixture_sequence_path": cell.get("sequence_path"),
        "trace_path": (
            str(out_dir / "gateway-data/traces/operations.jsonl")
            if out_dir
            else None
        ),
        "memory_path": str(memory_path) if memory_path else None,
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
        "controller_report": report,
        "trace_counts": cell.get("trace_counts"),
        "memory": {
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
        },
    }


def evaluate(args: argparse.Namespace) -> dict[str, Any]:
    run_root = args.run_root.resolve()
    if run_root.exists() and any(run_root.iterdir()):
        if not args.overwrite:
            raise SystemExit(f"refusing to overwrite non-empty run root: {run_root}")
        shutil.rmtree(run_root)
    run_root.mkdir(parents=True, exist_ok=True)

    treatment = evaluate_cell(
        args,
        name="treatment",
        sequence=treatment_sequence(),
        completion_critique=True,
    )
    ablation = evaluate_cell(
        args,
        name="no-critique-ablation",
        sequence=ablation_sequence(),
        completion_critique=False,
    )
    inconsistent = evaluate_cell(
        args,
        name="inconsistent-approval",
        sequence=inconsistent_approval_sequence(),
        completion_critique=True,
    )
    proactive = evaluate_cell(
        args,
        name="proactive-finalization",
        sequence=proactive_finalization_sequence(),
        completion_critique=True,
        proactive_checkpoint=True,
        model_budget=8,
        finalization_window=4,
    )
    no_checkpoint = evaluate_cell(
        args,
        name="no-checkpoint-limit-ablation",
        sequence=no_checkpoint_limit_sequence(),
        completion_critique=True,
        proactive_checkpoint=False,
        model_budget=8,
        report_partial=True,
        expected_answer="budget-exhausted-with-best-effort",
    )

    violations = []
    violations.extend(
        gate_cell(
            treatment,
            expected_calls=6,
            expected_actions=4,
            expected_critiques=2,
            expected_rejections=1,
            expected_checkpoints=0,
            expected_checkpoint_rejections=0,
            expected_reads=1,
            expect_audit=True,
        )
    )
    violations.extend(
        gate_cell(
            ablation,
            expected_calls=2,
            expected_actions=2,
            expected_critiques=0,
            expected_rejections=0,
            expected_checkpoints=0,
            expected_checkpoint_rejections=0,
            expected_reads=0,
            expect_audit=False,
        )
    )
    violations.extend(
        gate_cell(
            inconsistent,
            expected_calls=6,
            expected_actions=4,
            expected_critiques=2,
            expected_rejections=1,
            expected_checkpoints=0,
            expected_checkpoint_rejections=0,
            expected_reads=1,
            expect_audit=True,
        )
    )
    inconsistent_report = (
        inconsistent.get("summary", {}).get("controller_report", {})
        if isinstance(inconsistent.get("summary"), dict)
        else {}
    )
    if inconsistent_report.get("_completion_critique", {}).get("approved") is not True:
        violations.append("inconsistent approval did not recover to final approval")
    violations.extend(
        gate_cell(
            proactive,
            expected_calls=8,
            expected_actions=6,
            expected_critiques=1,
            expected_rejections=0,
            expected_checkpoints=1,
            expected_checkpoint_rejections=1,
            expected_reads=1,
            expect_audit=True,
        )
    )
    violations.extend(
        gate_cell(
            no_checkpoint,
            expected_calls=7,
            expected_actions=7,
            expected_critiques=0,
            expected_rejections=0,
            expected_checkpoints=0,
            expected_checkpoint_rejections=0,
            expected_reads=0,
            expect_audit=False,
        )
    )

    result = {
        "ok": not violations,
        "schema": "waymark.standard-agent-completion-critique.v7",
        "violations": violations,
        "causal_effect": {
            "treatment_workspace_reads": treatment["trace_counts"].get(
                "attempt.rpc.workspace_tx.read", 0
            ),
            "ablation_workspace_reads": ablation["trace_counts"].get(
                "attempt.rpc.workspace_tx.read", 0
            ),
            "premature_finish_rejected": (
                treatment["summary"]["controller_report"]["_control"][
                    "critic_rejections"
                ]
                == 1
            )
            if isinstance(treatment.get("summary"), dict)
            else False,
            "inconsistent_approval_normalized": (
                inconsistent["summary"]["controller_report"]["_control"][
                    "critic_rejections"
                ]
                == 1
            )
            if isinstance(inconsistent.get("summary"), dict)
            else False,
            "checkpoint_caused_repair_and_finish": (
                proactive["summary"]["controller_report"]["_control"][
                    "budget_checkpoints"
                ]
                == 1
                and proactive["trace_counts"].get(
                    "attempt.rpc.workspace_tx.read", 0
                )
                == 1
                and no_checkpoint["summary"]["controller_report"].get(
                    "complete"
                )
                is False
            )
            if (
                isinstance(proactive.get("summary"), dict)
                and isinstance(no_checkpoint.get("summary"), dict)
            )
            else False,
        },
        "cells": {
            "treatment": compact_cell(treatment),
            "no_critique_ablation": compact_cell(ablation),
            "inconsistent_approval": compact_cell(inconsistent),
            "proactive_finalization": compact_cell(proactive),
            "no_checkpoint_limit_ablation": compact_cell(no_checkpoint),
        },
    }
    (run_root / "aggregate.json").write_text(
        json.dumps(result, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    return result


def main() -> int:
    args = parse_args()
    for label, path in (
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

#!/usr/bin/env python3
"""Exercise the V12 one-action runtime-owned foreground run path."""

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
        default=GATEWAY_ROOT / "host/runner/smoke_waymark_libos_gateway_model_call.py",
    )
    parser.add_argument(
        "--run-root",
        type=Path,
        default=ROOT / "target/runs/stone-standard-runtime-owned-run-v1",
    )
    parser.add_argument("--warm-build", choices=("auto", "0", "1"), default="0")
    parser.add_argument("--overwrite", action="store_true")
    return parser.parse_args()


def sequence() -> list[str]:
    return [
        completion.action(
            {
                "tool": "run_complete",
                "input": {
                    "argv": [
                        "/bin/sh",
                        "-lc",
                        "sleep 0.5; printf 'READY\\n' > /app/result.txt",
                    ],
                    "timeout_ms": 5000,
                },
            }
        ),
        completion.action({"tool": "read", "input": {"path": "/app/result.txt"}}),
        completion.action({"final": {"answer": completion.ANSWER}}),
        completion.encoded(
            {
                "approved": True,
                "summary": "The owned terminal command created the required file and the controller read it back exactly.",
                "repair_objective": "",
                "requirements": [
                    {
                        "id": "create",
                        "requirement": "Create result.txt with exact bytes.",
                        "status": "satisfied",
                        "evidence": ["evidence.action.0"],
                        "reason": "The run_complete outcome was terminal and successful.",
                    },
                    {
                        "id": "verify",
                        "requirement": "Read the file back and verify exact bytes.",
                        "status": "satisfied",
                        "evidence": ["evidence.action.1"],
                        "reason": "The read outcome contains the exact required bytes.",
                    },
                ],
            }
        ),
    ]


def evaluate(args: argparse.Namespace) -> dict[str, Any]:
    run_root = args.run_root.resolve()
    if run_root.exists() and any(run_root.iterdir()):
        if not args.overwrite:
            raise SystemExit(f"refusing to overwrite non-empty run root: {run_root}")
        shutil.rmtree(run_root)
    run_root.mkdir(parents=True, exist_ok=True)

    cell = completion.evaluate_cell(
        args,
        name="runtime-owned-run",
        sequence=sequence(),
        completion_critique=True,
        proactive_checkpoint=False,
        model_budget=4,
    )
    violations = completion.gate_cell(
        cell,
        expected_calls=4,
        expected_actions=3,
        expected_critiques=1,
        expected_rejections=0,
        expected_checkpoints=0,
        expected_checkpoint_rejections=0,
        expected_reads=1,
        expect_audit=True,
    )
    summary = cell.get("summary")
    report = summary.get("controller_report") if isinstance(summary, dict) else None
    control = report.get("_control") if isinstance(report, dict) else None
    expected_control = {
        "run_complete_calls": 1,
        "run_complete_completions": 1,
        "run_start_calls": 0,
        "run_observations": 0,
        "active_runs": 0,
    }
    if not isinstance(control, dict):
        violations.append("V12 runtime-owned control metrics are missing")
    else:
        for key, value in expected_control.items():
            if control.get(key) != value:
                violations.append(
                    f"_control.{key}={control.get(key)!r}, expected {value!r}"
                )

    result = {
        "ok": not violations,
        "schema": "waymark.standard-agent-runtime-owned-run.v1",
        "violations": violations,
        "source_sha256": hashlib.sha256(args.standard_source.read_bytes()).hexdigest(),
        "expected_control": expected_control,
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

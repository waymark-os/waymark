#!/usr/bin/env python3
"""Exercise V7 owned-task tracking, finish blocking, and the active-run cap."""

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
INVOCATION_MARKER = "\nsession = agent_session()"

SUFFIX = r'''
def fixture_task_dispatch(action, options):
    if action.tool == "run_start":
        return {
            "ok": True,
            "kind": "run_started",
            "run_id": "fixture-run-1",
            "still_running": True,
            "done": False,
            "timed_out": False,
            "stdout": "",
            "stderr": "",
        }
    if action.tool == "run_wait":
        if action.input.run_id != "fixture-run-1":
            fail("wrong fixture run_id", code="fixture_run_id_mismatch")
        write_text("/app/result.txt", "READY\n")
        return {
            "ok": True,
            "kind": "run_completed",
            "run_id": action.input.run_id,
            "still_running": False,
            "done": True,
            "timed_out": False,
            "exit_code": 0,
            "path": "/app/result.txt",
            "stdout": "",
            "stderr": "",
        }
    return standard_shell_dispatch(action, options)


session = agent_session()
options = standard_agent_options(session.input)
options["max_active_runs"] = 1
result = standard_agent_control(
    session,
    options,
    fixture_task_dispatch,
    standard_verify_finish,
    standard_record_progress,
)
emit(result)
'''.lstrip()


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
        default=ROOT / "target/runs/stone-standard-task-management-v8",
    )
    parser.add_argument("--warm-build", choices=("auto", "0", "1"), default="0")
    parser.add_argument("--overwrite", action="store_true")
    return parser.parse_args()


def compose_source(source: str) -> str:
    marker = source.find(INVOCATION_MARKER)
    if marker < 0:
        raise ValueError(f"standard source is missing {INVOCATION_MARKER!r}")
    return source[:marker].rstrip() + "\n\n" + SUFFIX


def action(value: dict[str, Any]) -> str:
    return completion.action(value)


def sequence() -> list[str]:
    return [
        action(
            {
                "tool": "run_start",
                "input": {"argv": ["/bin/sh", "-lc", "sleep 30"]},
            }
        ),
        action({"final": {"answer": completion.ANSWER}}),
        action(
            {
                "tool": "run_start",
                "input": {"argv": ["/bin/sh", "-lc", "sleep 30"]},
            }
        ),
        action(
            {
                "tool": "run_wait",
                "input": {"run_id": "fixture-run-1", "timeout_ms": 1000},
            }
        ),
        action({"tool": "read", "input": {"path": "/app/result.txt"}}),
        action({"final": {"answer": completion.ANSWER}}),
        completion.encoded(
            {
                "approved": True,
                "summary": "The required file was produced and read back exactly.",
                "repair_objective": "",
                "requirements": [
                    {
                        "id": "create",
                        "requirement": "Create result.txt with exact bytes.",
                        "status": "satisfied",
                        "evidence": ["evidence.action.2"],
                        "reason": "The terminal task outcome produced the file.",
                    },
                    {
                        "id": "verify",
                        "requirement": "Read the file back and verify exact bytes.",
                        "status": "satisfied",
                        "evidence": ["evidence.action.3"],
                        "reason": "The read outcome contains the exact bytes.",
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

    composed = compose_source(args.standard_source.read_text(encoding="utf-8"))
    source_path = run_root / "task-management-fixture.stone"
    source_path.write_text(composed, encoding="utf-8")
    cell_args = argparse.Namespace(
        gateway_bin=args.gateway_bin,
        standard_source=source_path,
        smoke=args.smoke,
        run_root=run_root,
        warm_build=args.warm_build,
    )
    cell = completion.evaluate_cell(
        cell_args,
        name="bounded-owned-task",
        sequence=sequence(),
        completion_critique=True,
        proactive_checkpoint=False,
        model_budget=7,
    )
    violations = completion.gate_cell(
        cell,
        expected_calls=7,
        expected_actions=6,
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
    expected_control = {
        "run_start_calls": 2,
        "run_observations": 1,
        "run_termination_calls": 0,
        "run_completions": 1,
        "peak_active_runs": 1,
        "active_runs": 0,
        "finish_blocked_active_runs": 1,
        "failed_tools": 1,
    }
    if not isinstance(control, dict):
        violations.append("V7 lifecycle control metrics are missing")
    else:
        for key, value in expected_control.items():
            if control.get(key) != value:
                violations.append(
                    f"_control.{key}={control.get(key)!r}, expected {value!r}"
                )
    memory = completion.memory_by_key(cell)
    active = memory.get("progress.active_runs")
    if not isinstance(active, dict) or active.get("status") != "verified":
        violations.append("active-run memory did not settle to verified")
    elif active.get("content", {}).get("runs") != []:
        violations.append("active-run memory retained a terminal handle")
    second_start = memory.get("evidence.action.1")
    if second_start is None or second_start.get("status") != "contradicted":
        violations.append("the capped second start is not contradicted evidence")

    result = {
        "ok": not violations,
        "schema": "waymark.standard-agent-task-management.v7",
        "violations": violations,
        "source_sha256": hashlib.sha256(
            args.standard_source.read_bytes()
        ).hexdigest(),
        "composed_source_sha256": hashlib.sha256(composed.encode()).hexdigest(),
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

#!/usr/bin/env python3
"""Stress the standard V7 action-context boundary with repeated large output."""

from __future__ import annotations

import argparse
import json
import shutil
from pathlib import Path
from typing import Any

import eval_standard_agent_completion_critique as base


ROOT = Path(__file__).resolve().parents[2]
GATEWAY_ROOT = ROOT.parent / "waymark-gateway"
DEFAULT_SOURCE = ROOT / "examples/scripts/standard_attempt_agent.stone"
DEFAULT_SMOKE = (
    GATEWAY_ROOT / "host/runner/smoke_waymark_libos_gateway_model_call.py"
)
ANSWER = "context-bounded"
PROBE_COUNT = 14
RAW_STDOUT_CHARACTERS = 32768
CONTEXT_LIMIT = 16384
RAW_TOKEN_TERMS_PER_TOOL = (
    RAW_STDOUT_CHARACTERS - len("BEGIN-MARKER") - len("END-MARKER")
) // 2
UNCOMPACTED_PEAK_TOKEN_LOWER_BOUND = (
    RAW_TOKEN_TERMS_PER_TOOL * PROBE_COUNT
)
UNCOMPACTED_TOTAL_TOKEN_LOWER_BOUND = (
    RAW_TOKEN_TERMS_PER_TOOL * sum(range(PROBE_COUNT + 1))
)


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
        default=ROOT / "target/runs/stone-standard-context-pressure-v8",
    )
    parser.add_argument("--warm-build", choices=("auto", "0", "1"), default="0")
    parser.add_argument("--overwrite", action="store_true")
    return parser.parse_args()


def encoded(value: Any) -> str:
    return json.dumps(value, separators=(",", ":"))


def pressure_command() -> str:
    middle = RAW_STDOUT_CHARACTERS - len("BEGIN-MARKER") - len("END-MARKER")
    return (
        "{ printf BEGIN-MARKER; "
        f"yes X | tr '\\n' ' ' | head -c {middle}; "
        "printf END-MARKER; }"
    )


def pressure_sequence() -> list[str]:
    action = base.action(
        {
            "tool": "run_linux",
            "input": {
                "argv": ["/bin/sh", "-c", pressure_command()],
                "max_stdout_bytes": 65536,
                "max_stderr_bytes": 1024,
            },
        }
    )
    return [action for _ in range(PROBE_COUNT)] + [
        base.action({"final": {"answer": ANSWER}})
    ]


def cell_command(args: argparse.Namespace, out_dir: Path) -> list[str]:
    sequence = pressure_sequence()
    budget = len(sequence)
    task_input = {
        "max_turns": budget,
        "max_rounds": budget,
        "completion_critique": False,
        "proactive_completion_checkpoint": False,
        "max_messages": 32,
        "max_action_context_characters": CONTEXT_LIMIT,
        "max_task_context_characters": 2048,
        "max_observation_characters": 1024,
        "max_observation_field_characters": 1536,
        "action_memory_projection_tokens": 256,
        "max_evidence_characters": 2048,
        "max_stdout_bytes": 65536,
        "max_stderr_bytes": 1024,
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
            f"Run the large-output diagnostic exactly {PROBE_COUNT} times, "
            f"then finish with answer {ANSWER}."
        ),
        "--task-input-json",
        encoded(task_input),
        "--fixture-global-sequence-json",
        encoded(sequence),
        "--expected-answer",
        ANSWER,
        "--expected-model-calls",
        str(budget),
        "--max-turns",
        str(budget),
        "--max-rounds",
        str(budget),
        "--warm-build",
        args.warm_build,
        "--out-dir",
        str(out_dir.resolve()),
    ]


def trace_events(out_dir: Path) -> list[dict[str, Any]]:
    path = out_dir / "gateway-data/traces/operations.jsonl"
    if not path.is_file():
        return []
    return [
        json.loads(line)
        for line in path.read_text(encoding="utf-8").splitlines()
        if line
    ]


def gate(
    *,
    exit_code: int,
    summary: dict[str, Any] | None,
    events: list[dict[str, Any]],
    memory: dict[str, Any] | None,
) -> list[str]:
    violations: list[str] = []
    if exit_code != 0:
        violations.append(f"pressure cell exited {exit_code}")
    if not isinstance(summary, dict) or summary.get("ok") is not True:
        return violations + ["pressure summary is missing or failed"]
    report = summary.get("controller_report")
    control = report.get("_control") if isinstance(report, dict) else None
    if not isinstance(control, dict):
        violations.append("V7 control report is missing")
    else:
        expected = {
            "name": "stone.standard_action_v12",
            "actions": PROBE_COUNT + 1,
            "tool_calls": PROBE_COUNT,
            "model_calls": PROBE_COUNT + 1,
            "memory_projections": PROBE_COUNT + 1,
            "observation_truncations": PROBE_COUNT,
            "observation_field_truncations": PROBE_COUNT,
            "max_action_context_characters": CONTEXT_LIMIT,
        }
        for key, value in expected.items():
            if control.get(key) != value:
                violations.append(
                    f"_control.{key}={control.get(key)!r}, expected {value!r}"
                )
        peak = control.get("peak_action_context_characters")
        if not isinstance(peak, int) or peak > CONTEXT_LIMIT:
            violations.append(f"peak action context exceeds {CONTEXT_LIMIT}: {peak!r}")
        if int(control.get("context_messages_dropped", 0) or 0) <= 0:
            violations.append("character pressure did not compact old messages")

    model_events = [
        event for event in events if event.get("op") == "attempt.rpc.model.call"
    ]
    linux_events = [
        event for event in events if event.get("op") == "attempt.rpc.linux.exec"
    ]
    if len(model_events) != PROBE_COUNT + 1:
        violations.append(f"model event count is {len(model_events)}")
    if len(linux_events) != PROBE_COUNT:
        violations.append(f"Linux event count is {len(linux_events)}")
    input_tokens = [int(event.get("input_tokens", 0) or 0) for event in model_events]
    if not input_tokens or max(input_tokens) > 7000:
        violations.append(f"model input token peak is not bounded: {input_tokens}")
    if sum(input_tokens) > 90000:
        violations.append(f"model input token total is too high: {sum(input_tokens)}")
    if input_tokens and max(input_tokens) >= RAW_TOKEN_TERMS_PER_TOOL:
        violations.append("one compacted request costs as much as one raw tool output")
    if sum(input_tokens) * 10 >= UNCOMPACTED_TOTAL_TOKEN_LOWER_BOUND:
        violations.append("total request usage is not 10x below raw replay pressure")

    items = memory.get("items") if isinstance(memory, dict) else None
    evidence = [
        item
        for item in items or []
        if isinstance(item, dict)
        and str(item.get("key", "")).startswith("evidence.action.")
    ]
    if len(evidence) != 8:
        violations.append(f"retained evidence ring has {len(evidence)} items")
    for item in evidence:
        result = item.get("content", {}).get("result", {})
        stdout = result.get("stdout")
        if result.get("stdout_characters") != RAW_STDOUT_CHARACTERS:
            violations.append(f"{item.get('key')} lost raw stdout length")
        if result.get("stdout_truncated") is not True:
            violations.append(f"{item.get('key')} lacks truncation provenance")
        if not isinstance(stdout, str) or not (
            "BEGIN-MARKER" in stdout and "END-MARKER" in stdout
        ):
            violations.append(f"{item.get('key')} did not preserve output head/tail")
        if isinstance(stdout, str) and len(stdout) > 2048:
            violations.append(f"{item.get('key')} retained oversized evidence")
    if len(items or []) > 10:
        violations.append(f"attempt retained {len(items or [])} memory items")
    if summary.get("rolled_back") is not True:
        violations.append("pressure cell did not roll back")
    return violations


def evaluate(args: argparse.Namespace) -> dict[str, Any]:
    run_root = args.run_root.resolve()
    if run_root.exists() and any(run_root.iterdir()):
        if not args.overwrite:
            raise SystemExit(f"refusing to overwrite non-empty run root: {run_root}")
        shutil.rmtree(run_root)
    run_root.mkdir(parents=True, exist_ok=True)
    out_dir = run_root / "pressure"
    command = cell_command(args, out_dir)
    completed = base.run_capture(command, GATEWAY_ROOT)
    summary_path = out_dir / "summary.json"
    summary = (
        json.loads(summary_path.read_text(encoding="utf-8"))
        if summary_path.is_file()
        else None
    )
    events = trace_events(out_dir)
    memory = (
        base.memory_state(out_dir, summary)
        if isinstance(summary, dict)
        else None
    )
    violations = gate(
        exit_code=completed.returncode,
        summary=summary,
        events=events,
        memory=memory,
    )
    model_events = [
        event for event in events if event.get("op") == "attempt.rpc.model.call"
    ]
    control = (
        summary.get("controller_report", {}).get("_control")
        if isinstance(summary, dict)
        else None
    )
    result = {
        "ok": not violations,
        "schema": "waymark.standard-agent-context-pressure.v1",
        "violations": violations,
        "source_sha256": (
            summary.get("admitted_source_sha256")
            if isinstance(summary, dict)
            else None
        ),
        "raw_stdout_characters_per_tool": RAW_STDOUT_CHARACTERS,
        "tool_calls": PROBE_COUNT,
        "controller": control,
        "model_input_tokens": {
            "per_call": [
                int(event.get("input_tokens", 0) or 0)
                for event in model_events
            ],
            "peak": max(
                [int(event.get("input_tokens", 0) or 0) for event in model_events],
                default=0,
            ),
            "total": sum(
                int(event.get("input_tokens", 0) or 0)
                for event in model_events
            ),
            "uncompacted_peak_lower_bound": (
                UNCOMPACTED_PEAK_TOKEN_LOWER_BOUND
            ),
            "uncompacted_total_lower_bound": (
                UNCOMPACTED_TOTAL_TOKEN_LOWER_BOUND
            ),
            "peak_reduction_factor": (
                UNCOMPACTED_PEAK_TOKEN_LOWER_BOUND
                / max(
                    [
                        int(event.get("input_tokens", 0) or 0)
                        for event in model_events
                    ],
                    default=1,
                )
            ),
            "total_reduction_factor": (
                UNCOMPACTED_TOTAL_TOKEN_LOWER_BOUND
                / max(
                    1,
                    sum(
                        int(event.get("input_tokens", 0) or 0)
                        for event in model_events
                    ),
                )
            ),
        },
        "memory": {
            "revision": (
                memory.get("revision") if isinstance(memory, dict) else None
            ),
            "item_count": (
                len(memory.get("items", []))
                if isinstance(memory, dict)
                else None
            ),
        },
        "rolled_back": (
            summary.get("rolled_back")
            if isinstance(summary, dict)
            else None
        ),
        "summary_path": str(summary_path),
        "trace_path": str(out_dir / "gateway-data/traces/operations.jsonl"),
        "memory_path": (
            str(
                out_dir
                / "gateway-data/attempts"
                / summary["attempt"]
                / "memory-v0.json"
            )
            if isinstance(summary, dict)
            and isinstance(summary.get("attempt"), str)
            else None
        ),
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

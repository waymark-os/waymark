#!/usr/bin/env python3
"""Compare callback workflow IR with Stone's @stage authoring syntax."""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import subprocess
import time
from pathlib import Path
from typing import Any

import eval_stone_agent_authorship as base


ROOT = Path(__file__).resolve().parents[2]
ARMS = ("core", "syntax")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--model", default="gpt-5.6-terra")
    parser.add_argument("--reasoning-effort", default="low")
    parser.add_argument("--trials", type=int, default=1)
    parser.add_argument("--max-repairs", type=int, choices=(0, 1), default=1)
    parser.add_argument("--codex", default="codex")
    parser.add_argument(
        "--waymark-bin", type=Path, default=ROOT / "target/debug/waymark"
    )
    parser.add_argument(
        "--run-root",
        type=Path,
        default=ROOT / "target/runs/stone-stage-syntax-ab-v1",
    )
    parser.add_argument("--timeout", type=float, default=600.0)
    parser.add_argument("--overwrite", action="store_true")
    parser.add_argument("--reuse-existing", action="store_true")
    return parser.parse_args()


def compact_help(topic: dict[str, Any]) -> dict[str, Any]:
    return {
        "signature": topic.get("signature"),
        "use_when": topic.get("use_when"),
        "avoid": topic.get("avoid"),
        "examples": topic.get("examples"),
    }


def common_prompt() -> str:
    return """You are testing an LLM-oriented programming language. Write one complete
Stone program. Do not call tools or inspect files. Return only {"source":"..."}.

Stone is Python-shaped but has no imports or standard library. The program must:
1. Define one stage named artifact with max_attempts=1.
2. Before the action, completion evidence must be unsatisfied because artifact.txt is absent.
3. The primary action must return run(["sh", "-c", "exit 7"]) and must not create the artifact.
4. A repair handler must return run(["sh", "-c", "printf ready > artifact.txt"]).
5. Run a workflow named build-artifact containing the stage and emit the complete workflow report.

The unmodified program passes only if the failed primary action does not advance
the stage, repair runs once, a fresh evidence check proves the non-empty file,
and the workflow report succeeds. Use exactly the interface documented below.
Do not emulate it with a handwritten loop. Do not use comments as evidence.
"""


def arm_prompt(arm: str, topics: dict[str, dict[str, Any]]) -> str:
    if arm == "core":
        interface = {
            name: compact_help(topics[name])
            for name in (
                "workflow_evidence",
                "workflow_stage",
                "workflow",
                "workflow_run",
            )
        }
        requirement = """Use the callback core interface. Define an evidence handler
that probes with run(["test", "-s", "artifact.txt"]), constructs
workflow_evidence(...), and pass it with action= and repair= to workflow_stage.
Do not use @stage or file_nonempty."""
    elif arm == "syntax":
        interface = {
            name: compact_help(topics[name])
            for name in ("stage", "file_nonempty", "workflow", "workflow_run")
        }
        requirement = """Use @stage(evidence=file_nonempty(...), repair=...,
max_attempts=1) immediately above the primary action def. The decorated
function name is the stage value. Do not call workflow_stage or
workflow_evidence."""
    else:
        raise ValueError(f"unknown arm: {arm}")
    return (
        common_prompt()
        + "\nExperiment arm requirement:\n"
        + requirement
        + "\n\nLive Stone help:\n"
        + json.dumps(interface, separators=(",", ":"), sort_keys=True)
    )


def output_schema() -> dict[str, Any]:
    return {
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "properties": {"source": {"type": "string", "minLength": 1}},
        "required": ["source"],
        "additionalProperties": False,
    }


def codex_command(
    args: argparse.Namespace, run_dir: Path, prompt: str
) -> list[str]:
    return [
        args.codex,
        "exec",
        "--ephemeral",
        "--ignore-user-config",
        "--ignore-rules",
        "--skip-git-repo-check",
        "--sandbox",
        "read-only",
        "--json",
        "--model",
        args.model,
        "--config",
        f'model_reasoning_effort="{args.reasoning_effort}"',
        "--cd",
        str(run_dir),
        "--output-schema",
        str(run_dir / "output-schema.json"),
        "--output-last-message",
        str(run_dir / "last-message.json"),
        prompt,
    ]


def codex_usage(events_text: str) -> dict[str, int]:
    totals = {
        "input_tokens": 0,
        "cached_input_tokens": 0,
        "output_tokens": 0,
        "reasoning_output_tokens": 0,
    }
    for line in events_text.splitlines():
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            continue
        if not isinstance(event, dict) or event.get("type") != "turn.completed":
            continue
        usage = event.get("usage")
        if not isinstance(usage, dict):
            continue
        for key in totals:
            value = usage.get(key)
            if isinstance(value, int):
                totals[key] += value
    return totals


def source_features(source: str) -> dict[str, Any]:
    return {
        "stage_decorator": "@stage(" in source,
        "file_nonempty": "file_nonempty(" in source,
        "workflow_stage": "workflow_stage(" in source,
        "workflow_evidence": "workflow_evidence(" in source,
        "workflow": "workflow(" in source,
        "workflow_run": "workflow_run(" in source,
        "primary_failure": 'run(["sh", "-c", "exit 7"])' in source,
        "repair_write": "printf ready > artifact.txt" in source,
        "function_defs": len(re.findall(r"(?m)^def\s+", source)),
        "lines": len(source.splitlines()),
        "bytes": len(source.encode("utf-8")),
    }


def structural_gate(arm: str, features: dict[str, Any]) -> tuple[bool, list[str]]:
    violations = []
    for field in ("workflow", "workflow_run", "primary_failure", "repair_write"):
        if not features.get(field):
            violations.append(f"missing {field}")
    if arm == "core":
        for field in ("workflow_stage", "workflow_evidence"):
            if not features.get(field):
                violations.append(f"core arm missing {field}")
        if features.get("stage_decorator") or features.get("file_nonempty"):
            violations.append("core arm used syntax-only construct")
    else:
        for field in ("stage_decorator", "file_nonempty"):
            if not features.get(field):
                violations.append(f"syntax arm missing {field}")
        if features.get("workflow_stage") or features.get("workflow_evidence"):
            violations.append("syntax arm used callback core constructor")
    return not violations, violations


def response_payload(completed: subprocess.CompletedProcess[str]) -> dict[str, Any] | None:
    return base.response_payload(completed)


def execute_source(
    waymark_bin: Path, source_path: Path, run_dir: Path
) -> dict[str, Any]:
    work = run_dir / "work"
    if work.exists():
        shutil.rmtree(work)
    work.mkdir(parents=True)
    env = dict(os.environ)
    env["WAYMARK_START_DIR"] = str(work)
    completed = base.run_capture(
        [str(waymark_bin), "eval", str(source_path)],
        cwd=work,
        env=env,
        timeout=60,
    )
    payload = response_payload(completed)
    report = payload.get("value") if isinstance(payload, dict) else None
    stages = report.get("stages") if isinstance(report, dict) else None
    stage = stages[0] if isinstance(stages, list) and stages else None
    artifact = work / "artifact.txt"
    artifact_text = artifact.read_text(encoding="utf-8") if artifact.is_file() else None
    violations = []
    if completed.returncode != 0 or not isinstance(payload, dict) or not payload.get("ok"):
        violations.append("Stone evaluation failed")
    if not isinstance(report, dict) or report.get("ok") is not True:
        violations.append("workflow report did not succeed")
    expected = {
        "status": "completed",
        "attempts": 1,
        "repairs": 1,
        "checks": 3,
    }
    if not isinstance(stage, dict):
        violations.append("missing stage report")
    else:
        for key, value in expected.items():
            if stage.get(key) != value:
                violations.append(f"stage {key}={stage.get(key)!r}, expected {value!r}")
        if (stage.get("last_action") or {}).get("ok") is not False:
            violations.append("primary action was not recorded as failed")
        evidence = stage.get("evidence") or {}
        if evidence.get("satisfied") is not True or not evidence.get("evidence"):
            violations.append("fresh satisfied evidence was not retained")
    if artifact_text != "ready":
        violations.append(f"artifact content is {artifact_text!r}, expected 'ready'")
    return {
        "ok": not violations,
        "violations": violations,
        "exit_code": completed.returncode,
        "payload": payload,
        "stdout": completed.stdout,
        "stderr": completed.stderr,
        "artifact": artifact_text,
    }


def repair_prompt(
    original_prompt: str, source: str, result: dict[str, Any]
) -> str:
    diagnostic = {
        "structural_violations": result.get("structural_violations") or [],
        "execution_violations": (result.get("execution") or {}).get("violations") or [],
        "response": (result.get("execution") or {}).get("payload"),
    }
    return original_prompt + f"""

One bounded repair is allowed. Return the complete corrected source in the same
experiment arm. Do not call tools. Preserve every requirement.

Diagnostic:
{json.dumps(diagnostic, separators=(",", ":"), sort_keys=True)}

Previous source:
```stone
{source}
```
"""


def evaluate_once(
    args: argparse.Namespace,
    *,
    arm: str,
    prompt: str,
    run_dir: Path,
) -> dict[str, Any]:
    run_dir.mkdir(parents=True, exist_ok=args.overwrite or args.reuse_existing)
    summary_path = run_dir / "summary.json"
    if args.reuse_existing and summary_path.is_file():
        return json.loads(summary_path.read_text(encoding="utf-8"))
    (run_dir / "prompt.txt").write_text(prompt, encoding="utf-8")
    (run_dir / "output-schema.json").write_text(
        json.dumps(output_schema(), indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    command = codex_command(args, run_dir, prompt)
    started = time.monotonic()
    try:
        completed = base.run_capture(command, cwd=run_dir, timeout=args.timeout)
        timed_out = False
    except subprocess.TimeoutExpired as error:
        completed = subprocess.CompletedProcess(
            command, 124, error.stdout or "", error.stderr or ""
        )
        timed_out = True
    duration = time.monotonic() - started
    (run_dir / "codex.stdout.jsonl").write_text(completed.stdout, encoding="utf-8")
    (run_dir / "codex.stderr").write_text(completed.stderr, encoding="utf-8")
    source, output_error = base.parse_last_message(run_dir / "last-message.json")
    source_path = run_dir / "agent.stone"
    if source is not None:
        source_path.write_text(source.rstrip() + "\n", encoding="utf-8")
    features = source_features(source or "")
    structural_ok, structural_violations = structural_gate(arm, features)
    execution = (
        execute_source(args.waymark_bin, source_path, run_dir)
        if source is not None and structural_ok
        else None
    )
    ok = (
        completed.returncode == 0
        and output_error is None
        and base.tool_call_count(completed.stdout) == 0
        and structural_ok
        and bool(execution and execution.get("ok"))
    )
    result = {
        "ok": ok,
        "arm": arm,
        "model": args.model,
        "command": command,
        "codex_exit_code": completed.returncode,
        "codex_tool_calls": base.tool_call_count(completed.stdout),
        "timed_out": timed_out,
        "duration_seconds": duration,
        "output_error": output_error,
        "features": features,
        "structural_ok": structural_ok,
        "structural_violations": structural_violations,
        "execution": execution,
        "usage": codex_usage(completed.stdout),
    }
    summary_path.write_text(
        json.dumps(result, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    return result


def summarize_arm(results: list[dict[str, Any]]) -> dict[str, Any]:
    finals = [
        (result.get("repairs") or [None])[-1]
        if result.get("ok") is not True and result.get("repairs")
        else result
        for result in results
    ]
    passing = [result for result in finals if result and result.get("ok")]
    return {
        "trials": len(results),
        "first_response_passes": sum(result.get("ok") is True for result in results),
        "eventual_passes": len(passing),
        "repair_attempts": sum(len(result.get("repairs") or []) for result in results),
        "mean_source_bytes": (
            sum((result.get("features") or {}).get("bytes", 0) for result in passing)
            / len(passing)
            if passing
            else None
        ),
        "mean_source_lines": (
            sum((result.get("features") or {}).get("lines", 0) for result in passing)
            / len(passing)
            if passing
            else None
        ),
        "mean_function_defs": (
            sum((result.get("features") or {}).get("function_defs", 0) for result in passing)
            / len(passing)
            if passing
            else None
        ),
    }


def main() -> int:
    args = parse_args()
    if args.trials < 1 or args.trials > 10:
        raise SystemExit("--trials must be between 1 and 10")
    args.waymark_bin = args.waymark_bin.resolve()
    if not args.waymark_bin.is_file():
        raise SystemExit(f"Waymark binary not found: {args.waymark_bin}")
    run_root = args.run_root.resolve()
    if run_root.exists() and any(run_root.iterdir()) and not (
        args.overwrite or args.reuse_existing
    ):
        raise SystemExit(f"refusing to overwrite non-empty run root: {run_root}")
    if run_root.exists() and args.overwrite:
        shutil.rmtree(run_root)
    run_root.mkdir(parents=True, exist_ok=True)

    topics = {
        name: base.help_topic(args.waymark_bin, name)
        for name in (
            "workflow_evidence",
            "workflow_stage",
            "stage",
            "file_nonempty",
            "workflow",
            "workflow_run",
        )
    }
    prompts = {arm: arm_prompt(arm, topics) for arm in ARMS}
    results: dict[str, list[dict[str, Any]]] = {arm: [] for arm in ARMS}
    for trial in range(1, args.trials + 1):
        for arm in ARMS:
            run_dir = run_root / f"trial-{trial}" / arm
            result = evaluate_once(args, arm=arm, prompt=prompts[arm], run_dir=run_dir)
            result["repairs"] = []
            if result.get("ok") is not True and args.max_repairs == 1:
                source_path = run_dir / "agent.stone"
                if source_path.is_file():
                    repaired = evaluate_once(
                        args,
                        arm=arm,
                        prompt=repair_prompt(
                            prompts[arm],
                            source_path.read_text(encoding="utf-8"),
                            result,
                        ),
                        run_dir=run_dir / "repair-1",
                    )
                    result["repairs"].append(repaired)
                    (run_dir / "summary.json").write_text(
                        json.dumps(result, indent=2, sort_keys=True) + "\n",
                        encoding="utf-8",
                    )
            results[arm].append(result)

    arms = {arm: summarize_arm(results[arm]) for arm in ARMS}
    core = arms["core"]
    syntax = arms["syntax"]
    success_noninferior = (
        syntax["eventual_passes"] >= core["eventual_passes"]
        and syntax["first_response_passes"] >= core["first_response_passes"]
    )
    source_reduction = (
        syntax["mean_source_bytes"] is not None
        and core["mean_source_bytes"] is not None
        and syntax["mean_source_bytes"] <= core["mean_source_bytes"] * 0.85
    )
    repair_noninferior = syntax["repair_attempts"] <= core["repair_attempts"]
    complete = all(
        result.get("codex_exit_code") == 0
        and result.get("output_error") is None
        and result.get("codex_tool_calls") == 0
        for arm_results in results.values()
        for original in arm_results
        for result in [original, *(original.get("repairs") or [])]
    )
    aggregate = {
        "schema": "waymark.stone-stage-syntax-ab.v1",
        "complete": complete,
        "model": args.model,
        "reasoning_effort": args.reasoning_effort,
        "trials": args.trials,
        "max_repairs": args.max_repairs,
        "arms": arms,
        "success_noninferior": success_noninferior,
        "source_reduction_at_least_15_percent": source_reduction,
        "repair_noninferior": repair_noninferior,
        "hypothesis_supported": (
            complete and success_noninferior and source_reduction and repair_noninferior
        ),
        "run_root": str(run_root),
    }
    aggregate_path = run_root / "aggregate.json"
    aggregate_path.write_text(
        json.dumps(aggregate, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    print(json.dumps({**aggregate, "aggregate": str(aggregate_path)}, indent=2, sort_keys=True))
    return 0 if complete else 1


if __name__ == "__main__":
    raise SystemExit(main())

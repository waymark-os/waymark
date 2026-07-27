#!/usr/bin/env python3
"""Compare Stone authorship from scratch vs a compact memory-hook reference."""

from __future__ import annotations

import argparse
import difflib
import json
import shutil
import sys
from pathlib import Path
from typing import Any

import eval_stone_agent_authorship as base
import eval_stone_attempt_memory_authorship as authorship
import eval_stone_attempt_memory_restart as restart


ROOT = Path(__file__).resolve().parents[2]
SANDBOX = ROOT.parent
ARMS = ("scratch", "reference")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--model", action="append", dest="models")
    parser.add_argument("--inner-model", default="gpt-5.6-terra")
    parser.add_argument("--reasoning-effort", default="low")
    parser.add_argument("--codex", default="codex")
    parser.add_argument("--waymark-bin", type=Path, default=ROOT / "target/debug/waymark")
    parser.add_argument(
        "--gateway-bin",
        type=Path,
        default=SANDBOX / "waymark-gateway/target/debug/waymark-gateway",
    )
    parser.add_argument("--auth-json", type=Path, default=Path.home() / ".codex/auth.json")
    parser.add_argument(
        "--reference-source",
        type=Path,
        default=ROOT / "examples/references/attempt_memory_hooks.stone",
    )
    parser.add_argument("--image", default="python:3.12")
    parser.add_argument("--author-timeout", type=float, default=600.0)
    parser.add_argument("--execution-timeout", type=float, default=900.0)
    parser.add_argument("--max-repairs", type=int, choices=(0, 1), default=1)
    parser.add_argument(
        "--run-root",
        type=Path,
        default=ROOT / "target/runs/stone-attempt-memory-reference-ab-v1",
    )
    parser.add_argument("--overwrite", action="store_true")
    parser.add_argument("--reuse-existing", action="store_true")
    return parser.parse_args()


def scratch_prompt(help_topics: dict[str, dict[str, Any]]) -> str:
    return authorship.authorship_prompt(help_topics) + """

Experiment arm: SCRATCH. Author the complete program directly from the live
help and contract above. No reference implementation is available in this arm.
"""


def reference_prompt(
    help_topics: dict[str, dict[str, Any]], reference_source: str
) -> str:
    if restart.ACTION_TOKEN in reference_source:
        raise ValueError("reference source leaks the opaque action token")
    return authorship.authorship_prompt(help_topics) + f"""

Experiment arm: REFERENCE ADAPTATION. Adapt the visible reference program below
into the exact N/T/M program required by the contract above. Return a complete
standalone program, not a patch. The reference is deliberately only a policy
fragment: compose it with the lifecycle branches, raw-transcript baseline, and
bounded two-turn loop required by the contract. The contract is authoritative.
Preserve pair-local pre/post hooks, projection before the current instruction,
same-key replacement, and retention of completed nonzero run outcomes. Do not
add ambient/global state.

Reference hook policy:
```stone
{reference_source}
```
"""


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


def result_summary(result: dict[str, Any]) -> dict[str, Any]:
    restart_execution = result.get("restart_execution") or {}
    restart_metrics = restart_execution.get("metrics") or {}
    memory_cell = (restart_metrics.get("cells") or {}).get("M") or {}
    gate_violations = restart_execution.get("gate_violations") or []
    repairs = result.get("repairs") or []
    successful_repair = next(
        (repair for repair in repairs if repair.get("ok") is True), None
    )
    final = successful_repair or (repairs[-1] if repairs else result)
    final_execution = final.get("restart_execution") or {}
    total_usage = dict(result.get("author_usage") or {})
    for repair in repairs:
        for key, value in (repair.get("author_usage") or {}).items():
            if isinstance(value, int):
                total_usage[key] = int(total_usage.get(key, 0)) + value
    return {
        "first_response_pass": result.get("ok") is True,
        "repair_required": result.get("ok") is not True,
        "repair_attempts": len(repairs),
        "repair_pass": successful_repair is not None,
        "eventual_pass": result.get("ok") is True or successful_repair is not None,
        "structural_valid": result.get("required_features_ok") is True
        and (result.get("preflight") or {}).get("ok") is True,
        "behavioral_pass": restart_execution.get("ok") is True,
        "memory_policy_pass": memory_cell.get("memory_revision") == 5
        and not any(str(item).startswith("M ") for item in gate_violations),
        "gate_violations": result.get("gate_violations") or [],
        "author_usage": result.get("author_usage") or {},
        "total_author_usage": total_usage,
        "source_bytes": result.get("source_bytes", 0),
        "duration_seconds": result.get("duration_seconds", 0.0),
        "restart_metrics": restart_metrics,
        "final_structural_valid": final.get("required_features_ok") is True
        and (final.get("preflight") or {}).get("ok") is True,
        "final_behavioral_pass": final_execution.get("ok") is True,
        "final_gate_violations": final.get("gate_violations") or [],
    }


def compare_pair(results: dict[str, dict[str, Any]]) -> dict[str, Any]:
    scratch = result_summary(results["scratch"])
    reference = result_summary(results["reference"])
    scratch_usage = scratch["author_usage"]
    reference_usage = reference["author_usage"]
    return {
        "scratch": scratch,
        "reference": reference,
        "reference_strictly_improved_first_response": (
            reference["first_response_pass"] and not scratch["first_response_pass"]
        ),
        "reference_noninferior_first_response": (
            reference["first_response_pass"] or not scratch["first_response_pass"]
        ),
        "first_response_pass_delta": int(reference["first_response_pass"])
        - int(scratch["first_response_pass"]),
        "reference_strictly_improved_eventual_pass": (
            reference["eventual_pass"] and not scratch["eventual_pass"]
        ),
        "eventual_pass_delta": int(reference["eventual_pass"])
        - int(scratch["eventual_pass"]),
        "structural_valid_delta": int(reference["structural_valid"])
        - int(scratch["structural_valid"]),
        "reference_strictly_improved_memory_policy": (
            reference["memory_policy_pass"] and not scratch["memory_policy_pass"]
        ),
        "memory_policy_pass_delta": int(reference["memory_policy_pass"])
        - int(scratch["memory_policy_pass"]),
        "author_input_token_delta": int(reference_usage.get("input_tokens", 0))
        - int(scratch_usage.get("input_tokens", 0)),
        "author_output_token_delta": int(reference_usage.get("output_tokens", 0))
        - int(scratch_usage.get("output_tokens", 0)),
        "source_bytes_delta": int(reference["source_bytes"])
        - int(scratch["source_bytes"]),
    }


def evaluate_arm(
    args: argparse.Namespace,
    *,
    model: str,
    arm: str,
    prompt: str,
    run_dir: Path,
    reference_source: str,
) -> dict[str, Any]:
    summary_path = run_dir / "summary.json"
    if args.reuse_existing and summary_path.is_file():
        saved = json.loads(summary_path.read_text(encoding="utf-8"))
        source_path = run_dir / "agent.stone"
        source = source_path.read_text(encoding="utf-8") if source_path.is_file() else ""
        if saved.get("features") == authorship.source_features(source):
            return saved
    result = authorship.evaluate_model(args, model, prompt, run_dir)
    events_path = run_dir / "codex.stdout.jsonl"
    events = events_path.read_text(encoding="utf-8") if events_path.is_file() else ""
    source_path = run_dir / "agent.stone"
    source = source_path.read_text(encoding="utf-8") if source_path.is_file() else ""
    result["arm"] = arm
    result["author_usage"] = codex_usage(events)
    result["reference_similarity"] = (
        difflib.SequenceMatcher(None, reference_source, source).ratio()
        if arm == "reference" and source
        else None
    )
    (run_dir / "summary.json").write_text(
        json.dumps(result, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    return result


def redact_hidden_token(value: Any) -> Any:
    if isinstance(value, str):
        return value.replace(restart.ACTION_TOKEN, "[REDACTED_OPAQUE_ACTION]")
    if isinstance(value, list):
        return [redact_hidden_token(item) for item in value]
    if isinstance(value, dict):
        return {key: redact_hidden_token(item) for key, item in value.items()}
    return value


def repair_diagnostic(result: dict[str, Any]) -> dict[str, Any]:
    preflight = result.get("preflight") or {}
    if preflight.get("ok") is not True:
        diagnostic = {
            "phase": "preflight",
            "error_code": preflight.get("error_code"),
            "response": preflight.get("response"),
        }
    else:
        execution = result.get("restart_execution") or {}
        cells = {}
        for mode, cell in (execution.get("cells") or {}).items():
            restore = cell.get("restore") or {}
            cells[mode] = {
                "memory_revision": (cell.get("attempt_info") or {}).get(
                    "memory_revision"
                ),
                "restore_ok": restore.get("ok"),
                "restore_error": restore.get("error"),
            }
        diagnostic = {
            "phase": "restart_behavior",
            "gate_violations": execution.get("gate_violations") or [],
            "execution_error": result.get("restart_execution_error"),
            "cells": cells,
        }
    return redact_hidden_token(diagnostic)


def repair_prompt(base_prompt: str, broken: str, diagnostic: dict[str, Any]) -> str:
    diagnostic_json = json.dumps(diagnostic, separators=(",", ":"), sort_keys=True)
    prompt = base_prompt + f"""

This is one bounded diagnostic-guided repair attempt. The previous complete
source failed unchanged. Repair only what the structured diagnostic requires
while preserving every contract requirement and the current experiment arm.
Return the complete repaired source, not a patch or explanation. Do not call
tools.

Structured diagnostic:
{diagnostic_json}

Previous source:
```stone
{broken}
```
"""
    if restart.ACTION_TOKEN in prompt:
        raise ValueError("repair prompt leaks the opaque action token")
    return prompt


def main() -> int:
    args = parse_args()
    args.waymark_bin = args.waymark_bin.resolve()
    args.gateway_bin = args.gateway_bin.resolve()
    args.auth_json = args.auth_json.resolve()
    args.reference_source = args.reference_source.resolve()
    for label, path in (
        ("Waymark", args.waymark_bin),
        ("Gateway", args.gateway_bin),
        ("Codex auth", args.auth_json),
        ("Reference source", args.reference_source),
    ):
        if not path.is_file():
            raise SystemExit(f"{label} not found: {path}")

    run_root = args.run_root.resolve()
    if run_root.exists() and any(run_root.iterdir()) and not (
        args.overwrite or args.reuse_existing
    ):
        raise SystemExit(f"refusing to overwrite non-empty run root: {run_root}")
    if run_root.exists() and args.overwrite:
        shutil.rmtree(run_root)
    run_root.mkdir(parents=True, exist_ok=True)

    reference_source = args.reference_source.read_text(encoding="utf-8")
    topics = authorship.help_bundle(args.waymark_bin)
    prompts = {
        "scratch": scratch_prompt(topics),
        "reference": reference_prompt(topics, reference_source),
    }
    models = args.models or authorship.DEFAULT_AUTHOR_MODELS
    pairs: list[dict[str, Any]] = []
    all_results: list[dict[str, Any]] = []
    for model in models:
        model_dir = run_root / base.safe_model_dir(model)
        results = {
            arm: evaluate_arm(
                args,
                model=model,
                arm=arm,
                prompt=prompts[arm],
                run_dir=model_dir / arm,
                reference_source=reference_source,
            )
            for arm in ARMS
        }
        for arm, result in results.items():
            result["repairs"] = []
            if result.get("ok") is not True and args.max_repairs == 1:
                source_path = model_dir / arm / "agent.stone"
                broken = (
                    source_path.read_text(encoding="utf-8")
                    if source_path.is_file()
                    else ""
                )
                if broken:
                    repaired = evaluate_arm(
                        args,
                        model=model,
                        arm=arm + ".repair-1",
                        prompt=repair_prompt(
                            prompts[arm], broken, repair_diagnostic(result)
                        ),
                        run_dir=model_dir / arm / "repair-1",
                        reference_source=reference_source,
                    )
                    result["repairs"].append(repaired)
        all_results.extend(results.values())
        all_results.extend(
            repair
            for result in results.values()
            for repair in result.get("repairs") or []
        )
        pairs.append({"model": model, **compare_pair(results)})

    complete = all(
        result.get("codex_exit_code") == 0
        and result.get("output_error") is None
        and result.get("codex_tool_calls") == 0
        for result in all_results
    )
    aggregate = {
        "schema": "waymark.stone-attempt-memory-reference-ab.v1",
        "complete": complete,
        "hypothesis_supported": any(
            pair["reference_strictly_improved_first_response"]
            or pair["reference_strictly_improved_eventual_pass"]
            for pair in pairs
        ),
        "targeted_memory_policy_hypothesis_supported": any(
            pair["reference_strictly_improved_memory_policy"] for pair in pairs
        ),
        "run_root": str(run_root),
        "author_models": models,
        "inner_model": args.inner_model,
        "reference_source": str(args.reference_source),
        "max_repairs": args.max_repairs,
        "pairs": pairs,
    }
    aggregate_path = run_root / "aggregate.json"
    aggregate_path.write_text(
        json.dumps(aggregate, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    print(json.dumps({**aggregate, "aggregate": str(aggregate_path)}, indent=2, sort_keys=True))
    return 0 if complete else 1


if __name__ == "__main__":
    sys.exit(main())

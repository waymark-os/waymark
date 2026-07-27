#!/usr/bin/env python3
"""Exercise bounded controller recovery around individual Stone eval pairs."""

from __future__ import annotations

import argparse
import hashlib
import json
import sys
import tempfile
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "host/mcp"))

import stone_mcp_server as mcp  # noqa: E402


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--waymark-bin", type=Path, default=ROOT / "target/debug/waymark")
    parser.add_argument(
        "--policy",
        type=Path,
        default=ROOT / "examples/references/attempt_correction_policy.stone",
    )
    parser.add_argument("--timeout", type=float, default=30.0)
    return parser.parse_args()


def require_ok(label: str, result: dict[str, Any]) -> Any:
    if result.get("ok") is not True:
        raise AssertionError(f"{label} failed: {result}")
    return result.get("value")


def require_failure(label: str, result: dict[str, Any]) -> dict[str, Any]:
    if result.get("ok") is not False:
        raise AssertionError(f"{label} unexpectedly succeeded: {result}")
    correction = (result.get("error") or {}).get("correction")
    if not isinstance(correction, dict):
        raise AssertionError(f"{label} returned no correction: {result}")
    return correction


def digest(source: str) -> str:
    return hashlib.sha256(source.encode("utf-8")).hexdigest()


def synthetic_name_correction(source: str) -> dict[str, Any]:
    received = "context_projet"
    replacement = "context_project"
    start = source.index(received)
    return {
        "version": 1,
        "mode": "suggest",
        "phase": "admission",
        "execution_state": "not_started",
        "class": "name",
        "safety": "suggest_only",
        "auto_apply": False,
        "retry": "explicit_only",
        "source_sha256": digest(source),
        "received": received,
        "expected": [replacement],
        "candidates": [
            {
                "replacement": replacement,
                "confidence": "high",
                "distance": 1,
                "edit": {
                    "start": start,
                    "end": start + len(received),
                    "replacement": replacement,
                },
            }
        ],
        "choices": ["apply", "edit", "reject", "abort"],
    }


def reverse_cycle_correction(source: str) -> dict[str, Any]:
    received = "context_project"
    replacement = "context_projet"
    start = source.index(received)
    return {
        "version": 1,
        "mode": "suggest",
        "phase": "admission",
        "execution_state": "not_started",
        "class": "name",
        "safety": "suggest_only",
        "auto_apply": False,
        "retry": "explicit_only",
        "source_sha256": digest(source),
        "received": received,
        "expected": [replacement],
        "candidates": [
            {
                "replacement": replacement,
                "confidence": "high",
                "distance": 1,
                "edit": {
                    "start": start,
                    "end": start + len(received),
                    "replacement": replacement,
                },
            }
        ],
        "choices": ["apply", "edit", "reject", "abort"],
    }


def prepare_source(source: str, failure: dict[str, Any]) -> str:
    return (
        "emit(prepare_stone_recovery("
        + json.dumps(source)
        + ", json_loads("
        + json.dumps(json.dumps(failure, separators=(",", ":")))
        + ")"
        + "))"
    )


def run_trial(
    waymark_bin: Path, policy_source: str, root: Path, timeout: float
) -> dict[str, Any]:
    marker_source = (
        'write_text("effect.txt", "once\\n")\n'
        'context_projet(focus="latest outcome", max_tokens=32)'
    )

    baseline_root = root / "baseline"
    baseline_root.mkdir()
    baseline = mcp.WarmStdioBackend(
        str(waymark_bin), cwd=str(baseline_root), timeout_seconds=timeout
    )
    try:
        baseline_failure = baseline.eval(marker_source)
        baseline_correction = require_failure("baseline typo", baseline_failure)
        if (baseline_root / "effect.txt").exists():
            raise AssertionError("admission failure executed the baseline effect")
    finally:
        baseline.close()

    policy_root = root / "policy"
    policy_root.mkdir()
    backend = mcp.WarmStdioBackend(
        str(waymark_bin), cwd=str(policy_root), timeout_seconds=timeout
    )
    try:
        require_ok("load recovery policy", backend.eval(policy_source))

        first_failure = backend.eval(marker_source)
        first_correction = require_failure("policy typo", first_failure)
        if first_correction.get("execution_state") != "not_started":
            raise AssertionError(f"typo was not rejected before effects: {first_failure}")
        marker = policy_root / "effect.txt"
        if marker.exists():
            raise AssertionError("admission failure executed the policy-arm effect")

        first_decision = require_ok(
            "prepare first retry",
            backend.eval(prepare_source(marker_source, first_failure)),
        )
        if first_decision.get("decision") != "retry_once":
            raise AssertionError(f"safe correction was not selected: {first_decision}")
        preview = first_decision.get("preview") or {}
        if preview.get("executed") is not False:
            raise AssertionError(f"correction preview executed implicitly: {preview}")

        corrected_result = backend.eval(str(preview.get("source") or ""))
        require_ok("explicit corrected eval", corrected_result)
        if marker.read_text(encoding="utf-8") != "once\n":
            raise AssertionError("corrected source did not execute exactly once")

        recorded = require_ok(
            "record corrected outcome",
            backend.eval(
                "emit(record_stone_recovery_outcome("
                + json.dumps(preview["source_sha256"])
                + ", json_loads("
                + json.dumps(json.dumps(corrected_result, separators=(",", ":")))
                + ")"
                + "))"
            ),
        )
        if recorded != {"recorded": True, "outcome": "succeeded", "attempts_used": 1}:
            raise AssertionError(f"unexpected recorded outcome: {recorded}")

        repeated_failure = backend.eval(marker_source)
        require_failure("repeated typo", repeated_failure)
        repeated = require_ok(
            "block repeated pair",
            backend.eval(prepare_source(marker_source, repeated_failure)),
        )
        if repeated.get("reason") != "source_candidate_already_tried":
            raise AssertionError(f"repeated pair was not blocked: {repeated}")
        if marker.read_text(encoding="utf-8") != "once\n":
            raise AssertionError("blocked repeated pair duplicated an effect")

        field_source = 'emit({"transition_id": "t1"}.id)'
        field_failure = backend.eval(field_source)
        field_correction = require_failure("field failure", field_failure)
        field_decision = require_ok(
            "reject uncertain replay",
            backend.eval(prepare_source(field_source, field_failure)),
        )
        if (
            field_correction.get("execution_state") != "started_or_unknown"
            or field_decision.get("reason") != "execution_may_have_started"
        ):
            raise AssertionError(
                f"evaluation-time replay was not rejected: {field_decision}"
            )

        semantic_source = "def update():\n    global mode\n"
        semantic_failure = backend.eval(semantic_source)
        semantic_correction = require_failure("semantic failure", semantic_failure)
        semantic_decision = require_ok(
            "reject semantic repair",
            backend.eval(prepare_source(semantic_source, semantic_failure)),
        )
        if (
            semantic_correction.get("safety") != "requires_repair"
            or semantic_decision.get("reason") != "semantic_repair_required"
        ):
            raise AssertionError(f"semantic repair was not delegated: {semantic_decision}")

        corrected_source = str(preview["source"])
        cycle_failure = {
            "ok": False,
            "error": {"correction": reverse_cycle_correction(corrected_source)},
        }
        cycle = require_ok(
            "block correction cycle",
            backend.eval(prepare_source(corrected_source, cycle_failure)),
        )
        if cycle.get("reason") != "correction_cycle":
            raise AssertionError(f"correction cycle was not blocked: {cycle}")

        budget_decisions = []
        for index in range(4):
            source = f'context_projet(focus="budget-{index}", max_tokens=32)'
            failure = {
                "ok": False,
                "error": {"correction": synthetic_name_correction(source)},
            }
            decision = require_ok(
                f"budget decision {index}",
                backend.eval(prepare_source(source, failure)),
            )
            budget_decisions.append(decision)
        if [item.get("decision") for item in budget_decisions] != [
            "retry_once",
            "retry_once",
            "retry_once",
            "replan",
        ]:
            raise AssertionError(f"unexpected budget decisions: {budget_decisions}")
        if budget_decisions[-1].get("reason") != "attempt_budget_exhausted":
            raise AssertionError(f"budget exhaustion was not explicit: {budget_decisions}")

        ledger_items = require_ok(
            "read recovery ledger",
            backend.eval('emit(context_read(keys=["recovery.stone"], limit=2))'),
        )
        if not isinstance(ledger_items, list) or len(ledger_items) != 1:
            raise AssertionError(f"recovery did not use one hot memory item: {ledger_items}")
        ledger = (ledger_items[0] or {}).get("content") or {}
        entries = ledger.get("entries") or []
        if ledger.get("attempts_used") != 4 or len(entries) != 4:
            raise AssertionError(f"recovery ledger was not bounded: {ledger}")
        encoded_ledger = json.dumps(ledger, sort_keys=True)
        if marker_source in encoded_ledger or corrected_source in encoded_ledger:
            raise AssertionError("recovery ledger retained raw Stone source")

        return {
            "baseline": {
                "initial_ok": baseline_failure.get("ok"),
                "retry_count": 0,
                "effect_count": 0,
                "execution_state": baseline_correction.get("execution_state"),
            },
            "policy": {
                "initial_ok": first_failure.get("ok"),
                "retry_count": 1,
                "recovered": corrected_result.get("ok"),
                "effect_count": 1,
                "repeat_reason": repeated.get("reason"),
                "uncertain_reason": field_decision.get("reason"),
                "semantic_reason": semantic_decision.get("reason"),
                "cycle_reason": cycle.get("reason"),
                "budget_reason": budget_decisions[-1].get("reason"),
                "hot_items": len(ledger_items),
                "ledger_entries": len(entries),
            },
        }
    finally:
        backend.close()


def main() -> int:
    args = parse_args()
    waymark_bin = args.waymark_bin.resolve()
    policy = args.policy.resolve()
    if not waymark_bin.is_file():
        raise SystemExit(f"Waymark binary not found: {waymark_bin}")
    if not policy.is_file():
        raise SystemExit(f"recovery policy not found: {policy}")

    with tempfile.TemporaryDirectory(
        prefix="waymark-stone-correction-", dir="/tmp"
    ) as root_text:
        report = run_trial(
            waymark_bin,
            policy.read_text(encoding="utf-8"),
            Path(root_text),
            args.timeout,
        )

    print(json.dumps({"ok": True, **report}, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

#!/usr/bin/env python3

from __future__ import annotations

import hashlib
import importlib.util
import json
import tempfile
import unittest
from pathlib import Path


MODULE_PATH = Path(__file__).with_name("eval_stone_correction_model_ab.py")
SPEC = importlib.util.spec_from_file_location("eval_stone_correction_model_ab", MODULE_PATH)
assert SPEC and SPEC.loader
experiment = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(experiment)


def trace_record(
    seq: int,
    *,
    source: str | None = None,
    call: str | None = None,
    ok: bool,
    execution_state: str | None = None,
) -> dict:
    stone = {}
    if source is not None:
        stone["source_preview"] = source
    if call is not None:
        stone["call"] = call
    record = {
        "seq": seq,
        "tool": "stone_call" if call else "stone_eval",
        "ok": ok,
        "stone": stone,
    }
    if execution_state is not None:
        record["error"] = {"correction": {"execution_state": execution_state}}
    return record


class StoneCorrectionModelAbTests(unittest.TestCase):
    def test_trace_gate_accepts_explicit_safe_retry_and_unsafe_refusal(self) -> None:
        records = [
            trace_record(
                1,
                source=experiment.SAFE_SOURCE,
                ok=False,
                execution_state="not_started",
            ),
            trace_record(2, call="correction_apply", ok=True),
            trace_record(3, source=experiment.SAFE_CORRECTED_SOURCE, ok=True),
            trace_record(
                4,
                source=experiment.UNSAFE_SOURCE,
                ok=False,
                execution_state="started_or_unknown",
            ),
            trace_record(
                5,
                source='emit({"decision": "replan"})',
                ok=True,
            ),
        ]

        metrics, violations = experiment.evaluate_trace(records)

        self.assertEqual(violations, [])
        self.assertEqual(metrics["safe_failure_seq"], 1)
        self.assertEqual(metrics["corrected_seq"], 3)
        self.assertEqual(metrics["unsafe_failure_seq"], 4)

    def test_trace_gate_rejects_retry_after_uncertain_evaluation(self) -> None:
        records = [
            trace_record(
                1,
                source=experiment.SAFE_SOURCE,
                ok=False,
                execution_state="not_started",
            ),
            trace_record(2, call="correction_apply", ok=True),
            trace_record(3, source=experiment.SAFE_CORRECTED_SOURCE, ok=True),
            trace_record(
                4,
                source=experiment.UNSAFE_SOURCE,
                ok=False,
                execution_state="started_or_unknown",
            ),
            trace_record(
                5,
                source=experiment.UNSAFE_SOURCE.replace(".id", ".transition_id"),
                ok=True,
            ),
        ]

        _, violations = experiment.evaluate_trace(records)

        self.assertIn("unsafe evaluation was mechanically retried", violations)

    def test_ledger_gate_requires_one_compact_success_and_replan(self) -> None:
        value = {
            "version": 1,
            "attempts_used": 1,
            "entries": [
                {
                    "source_sha256": hashlib.sha256(
                        experiment.SAFE_SOURCE.encode()
                    ).hexdigest(),
                    "corrected_source_sha256": hashlib.sha256(
                        experiment.SAFE_CORRECTED_SOURCE.encode()
                    ).hexdigest(),
                    "replacement": "context_project",
                    "decision": "retry_once",
                    "outcome": "succeeded",
                }
            ],
            "last_decision": {
                "decision": "replan",
                "reason": "execution_may_have_started",
                "source_sha256": hashlib.sha256(
                    experiment.UNSAFE_SOURCE.encode()
                ).hexdigest(),
            },
        }

        self.assertEqual(experiment.evaluate_ledger(value), [])

        value["entries"][0]["source"] = experiment.SAFE_SOURCE
        self.assertIn("ledger retains raw source", experiment.evaluate_ledger(value))

    def test_ledger_gate_accepts_equivalent_compact_event_list(self) -> None:
        value = [
            {
                "hashes": {
                    "failed": hashlib.sha256(
                        experiment.SAFE_SOURCE.encode()
                    ).hexdigest(),
                    "corrected": hashlib.sha256(
                        experiment.SAFE_CORRECTED_SOURCE.encode()
                    ).hexdigest(),
                },
                "replacement": "context_project",
                "decision": "retry_once",
                "outcome": "ok",
            },
            {
                "hashes": {
                    "failed": hashlib.sha256(
                        experiment.UNSAFE_SOURCE.encode()
                    ).hexdigest()
                },
                "replacement": "transition_id",
                "decision": "replan",
                "outcome": "error_started_or_unknown",
            },
        ]

        self.assertEqual(experiment.evaluate_ledger(value), [])
        self.assertEqual(experiment.ledger_schema(value), "compact_event_list")

    def test_prompts_change_only_by_arm_guidance(self) -> None:
        common = experiment.common_prompt()
        interface = experiment.interface_prompt()
        reference = experiment.reference_prompt("def policy():\n    return True\n")

        self.assertIn(common, interface)
        self.assertIn(common, reference)
        self.assertIn("INTERFACE ONLY", interface)
        self.assertIn("POLICY REFERENCE", reference)
        self.assertIn("prepare_stone_recovery", reference)

    def test_trace_reader_ignores_malformed_lines(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "trace.jsonl"
            path.write_text(
                "not-json\n" + json.dumps({"seq": 1, "tool": "stone_eval"}) + "\n",
                encoding="utf-8",
            )

            self.assertEqual(
                experiment.read_trace(path), [{"seq": 1, "tool": "stone_eval"}]
            )


if __name__ == "__main__":
    unittest.main()

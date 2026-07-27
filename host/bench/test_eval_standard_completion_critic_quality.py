#!/usr/bin/env python3
"""Unit checks for the real-model completion-critic quality harness."""

from __future__ import annotations

import importlib.util
import sys
import unittest
from pathlib import Path


MODULE_PATH = (
    Path(__file__).resolve().parent
    / "eval_standard_completion_critic_quality.py"
)
SPEC = importlib.util.spec_from_file_location("critic_quality", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
quality = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = quality
SPEC.loader.exec_module(quality)


def fixture_summary(approved: bool, statuses: list[str]) -> dict:
    return {
        "ok": True,
        "rolled_back": True,
        "controller_report": {
            "critique": {
                "approved": approved,
                "repair_objective": (
                    "" if approved else "Read the file and collect evidence."
                ),
                "requirements": [
                    {
                        "status": status,
                        "evidence": (
                            [f"evidence.action.{index}"]
                            if status == "satisfied"
                            else []
                        ),
                    }
                    for index, status in enumerate(statuses)
                ],
            }
        },
    }


class CompletionCriticQualityTests(unittest.TestCase):
    def test_composed_source_replaces_default_invocation(self) -> None:
        source = (
            "def standard_agent_control():\n"
            "    pass\n"
            "\nsession = agent_session()\n"
            "emit(session)\n"
        )
        composed = quality.compose_source(source)
        self.assertEqual(composed.count("session = agent_session()"), 1)
        self.assertIn("standard_completion_critique(", composed)
        self.assertIn('"evidence.action.1"', composed)
        self.assertNotIn("emit(session)", composed)

    def test_rejection_gate_requires_unsupported_status_and_repair(self) -> None:
        case = quality.CASES[0]
        violations = quality.gate_case(
            case,
            summary=fixture_summary(False, ["satisfied", "unsupported"]),
            metrics={"operation_counts": {"attempt.rpc.model.call": 1}},
            memory={
                "item_count": 3,
                "items": [
                    {"key": "requirement.task"},
                    {"key": "requirement.audit"},
                    {"key": "evidence.action.0"},
                ],
            },
            exit_code=0,
            timed_out=False,
        )
        self.assertEqual(violations, [])

    def test_approval_gate_requires_evidence_for_every_requirement(self) -> None:
        case = quality.CASES[1]
        summary = fixture_summary(True, ["satisfied", "satisfied"])
        self.assertEqual(
            quality.gate_case(
                case,
                summary=summary,
                metrics={"operation_counts": {"attempt.rpc.model.call": 1}},
                memory={
                    "item_count": 4,
                    "items": [
                        {"key": "requirement.task"},
                        {"key": "requirement.audit"},
                        {"key": "evidence.action.0"},
                        {"key": "evidence.action.1"},
                    ],
                },
                exit_code=0,
                timed_out=False,
            ),
            [],
        )
        summary["controller_report"]["critique"]["requirements"][1][
            "evidence"
        ] = []
        violations = quality.gate_case(
            case,
            summary=summary,
            metrics={"operation_counts": {"attempt.rpc.model.call": 1}},
            memory={
                "item_count": 4,
                "items": [
                    {"key": "requirement.task"},
                    {"key": "requirement.audit"},
                ],
            },
            exit_code=0,
            timed_out=False,
        )
        self.assertTrue(any("lacks an evidence" in item for item in violations))


if __name__ == "__main__":
    unittest.main()

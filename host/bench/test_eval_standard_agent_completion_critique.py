#!/usr/bin/env python3
"""Unit checks for the standard completion-critique causal canary."""

from __future__ import annotations

import importlib.util
import unittest
from pathlib import Path


MODULE_PATH = (
    Path(__file__).resolve().parent
    / "eval_standard_agent_completion_critique.py"
)
SPEC = importlib.util.spec_from_file_location("completion_critique", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
completion = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(completion)


class CompletionCritiqueCanaryTests(unittest.TestCase):
    def test_treatment_sequence_repairs_after_rejected_finish(self) -> None:
        sequence = completion.treatment_sequence()
        self.assertEqual(len(sequence), 6)
        self.assertEqual(completion.ablation_sequence(), sequence[:2])
        first_audit = completion.json.loads(sequence[2])
        final_audit = completion.json.loads(sequence[5])
        self.assertFalse(first_audit["approved"])
        self.assertEqual(
            first_audit["requirements"][1]["status"],
            "unsupported",
        )
        self.assertTrue(final_audit["approved"])
        self.assertEqual(
            final_audit["requirements"][1]["evidence"],
            ["evidence.action.1"],
        )

    def test_inconsistent_approval_remains_semantically_unsupported(self) -> None:
        audit = completion.json.loads(
            completion.inconsistent_approval_sequence()[2]
        )
        self.assertTrue(audit["approved"])
        self.assertEqual(audit["requirements"][1]["status"], "unsupported")

    def test_proactive_sequence_reserves_repair_finish_and_final_audit(self) -> None:
        sequence = completion.proactive_finalization_sequence()
        self.assertEqual(len(sequence), 8)
        checkpoint = completion.json.loads(sequence[4])
        repair = completion.json.loads(sequence[5])
        finish = completion.json.loads(sequence[6])
        final_audit = completion.json.loads(sequence[7])
        self.assertFalse(checkpoint["approved"])
        self.assertEqual(
            checkpoint["requirements"][1]["status"],
            "unsupported",
        )
        self.assertEqual(repair["actions"][0]["tool"], "read")
        self.assertIn("final", finish["actions"][0])
        self.assertTrue(final_audit["approved"])
        self.assertEqual(
            final_audit["requirements"][1]["evidence"],
            ["evidence.action.4"],
        )
        self.assertEqual(len(completion.no_checkpoint_limit_sequence()), 7)

    def test_gate_checks_bounded_memory_and_causal_read(self) -> None:
        cell = {
            "name": "fixture",
            "exit_code": 0,
            "summary": {
                "ok": True,
                "rolled_back": True,
                "controller_report": {
                    "_control": {
                        "name": "stone.standard_action_v13",
                        "model_calls": 6,
                        "actions": 4,
                        "completion_critiques": 2,
                        "critic_rejections": 1,
                        "budget_checkpoints": 0,
                        "checkpoint_rejections": 0,
                    }
                },
            },
            "trace_counts": {
                "attempt.rpc.model.call": 6,
                "attempt.rpc.workspace_tx.read": 1,
                "env.rollback": 1,
            },
            "memory": {
                "items": [
                    {"key": "requirement.task", "status": "verified"},
                    {"key": "requirement.audit", "status": "verified"},
                    {"key": "evidence.action.0", "status": "verified"},
                    {"key": "evidence.action.1", "status": "verified"},
                    {"key": "progress.agent_control", "status": "verified"},
                ]
            },
        }
        self.assertEqual(
            completion.gate_cell(
                cell,
                expected_calls=6,
                expected_actions=4,
                expected_critiques=2,
                expected_rejections=1,
                expected_checkpoints=0,
                expected_checkpoint_rejections=0,
                expected_reads=1,
                expect_audit=True,
            ),
            [],
        )


if __name__ == "__main__":
    unittest.main()

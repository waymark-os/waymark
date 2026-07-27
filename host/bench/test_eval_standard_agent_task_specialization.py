#!/usr/bin/env python3
"""Unit tests for the simple repository task-specialization experiment."""

from __future__ import annotations

import importlib.util
import unittest
from pathlib import Path


MODULE_PATH = Path(__file__).with_name(
    "eval_standard_agent_task_specialization.py"
)
SPEC = importlib.util.spec_from_file_location(
    "eval_standard_agent_task_specialization",
    MODULE_PATH,
)
assert SPEC and SPEC.loader
EXPERIMENT = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(EXPERIMENT)


VALID_SOURCE = r'''
def verify_transform(candidate, session, state):
    checked = standard_verify_finish(candidate, session, state)
    content = read_file("/app/output.txt", max_bytes=256)
    if content != "APPLE\nBANANA\nPEAR\n":
        fail("wrong output", code="task_verification_failed")
    checked["task_verified"] = True
    checked["verified_bytes"] = len(content)
    checked["verified_round"] = state.rounds
    return checked

session = agent_session()
options = standard_agent_options(session.input)
result = standard_agent_control(
    session,
    options,
    standard_shell_dispatch,
    verify_transform,
    standard_record_progress,
)
emit(result)
'''


class StandardAgentTaskSpecializationTests(unittest.TestCase):
    def test_prompt_freezes_task_verifier_and_stone_mutation_rules(self) -> None:
        prompt = EXPERIMENT.authorship_prompt()
        self.assertIn("/app/output.txt", prompt)
        self.assertIn("task_verification_failed", prompt)
        self.assertIn('checked["task_verified"]', prompt)
        self.assertIn("independently checks", prompt)
        self.assertIn("not the authority", prompt)

    def test_feature_gate_accepts_one_bounded_verifier_read(self) -> None:
        features = EXPERIMENT.source_features(VALID_SOURCE)
        self.assertTrue(EXPERIMENT.required_features_ok(features), features)
        invalid = EXPERIMENT.source_features(
            VALID_SOURCE.replace(
                'read_file("/app/output.txt", max_bytes=256)',
                'read_file("/app/output.txt")',
            )
        )
        self.assertFalse(EXPERIMENT.required_features_ok(invalid), invalid)

    def test_result_gate_requires_external_effect_trace_and_rollback(self) -> None:
        summary = {
            "ok": True,
            "rolled_back": True,
            "expected_workspace_content": EXPERIMENT.EXPECTED_OUTPUT,
            "controller_report": {
                "answer": EXPERIMENT.EXPECTED_ANSWER,
                "task_verified": True,
                "verified_bytes": len(EXPERIMENT.EXPECTED_OUTPUT.encode()),
                "verified_round": 4,
                "_control": {
                    "name": "stone.standard_action_v11",
                    "rounds": 4,
                    "actions": 4,
                    "tool_calls": 3,
                    "failed_tools": 0,
                    "validation_retries": 0,
                },
            },
        }
        metrics = {
            "operation_counts": {
                "attempt.rpc.model.call": 4,
                "attempt.rpc.workspace_tx.read": 3,
                "attempt.rpc.workspace_tx.write": 1,
                "attempt.memory.write": 13,
            }
        }
        self.assertEqual(EXPERIMENT.result_gate(summary, metrics), (True, []))
        metrics["operation_counts"]["attempt.rpc.linux.exec"] = 1
        ok, violations = EXPERIMENT.result_gate(summary, metrics)
        self.assertFalse(ok)
        self.assertIn("inner agent unexpectedly used Linux", violations)

    def test_negative_gate_requires_declared_rejection_and_cleanup(self) -> None:
        summary = {
            "ok": True,
            "rolled_back": True,
            "controller_error_code": "task_verification_failed",
        }
        metrics = {
            "operation_counts": {
                "attempt.rpc.model.call": 4,
                "attempt.rpc.workspace_tx.read": 3,
                "attempt.rpc.workspace_tx.write": 1,
                "attempt.memory.write": 12,
            }
        }
        self.assertEqual(EXPERIMENT.negative_gate(summary, metrics), (True, []))
        summary["controller_error_code"] = "other"
        ok, violations = EXPERIMENT.negative_gate(summary, metrics)
        self.assertFalse(ok)
        self.assertTrue(any("wrong verifier error" in item for item in violations))


if __name__ == "__main__":
    unittest.main()

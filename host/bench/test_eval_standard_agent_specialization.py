#!/usr/bin/env python3
"""Unit tests for the standard Stone adapter-specialization experiment."""

from __future__ import annotations

import importlib.util
import unittest
from pathlib import Path


MODULE_PATH = Path(__file__).with_name("eval_standard_agent_specialization.py")
SPEC = importlib.util.spec_from_file_location(
    "eval_standard_agent_specialization",
    MODULE_PATH,
)
assert SPEC and SPEC.loader
EXPERIMENT = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(EXPERIMENT)


VALID_SOURCE = """
def specialized_verify_finish(candidate, session, state):
    checked = standard_verify_finish(candidate, session, state)
    checked["specialized"] = True
    checked["specialization_label"] = get(
        session.input, "specialization_label", "missing"
    )
    checked["verified_round"] = state.rounds
    return checked

session = agent_session()
options = standard_agent_options(session.input)
result = standard_agent_control(
    session,
    options,
    standard_shell_dispatch,
    specialized_verify_finish,
    standard_record_progress,
)
emit(result)
"""


class StandardAgentSpecializationTests(unittest.TestCase):
    def test_prompt_exposes_only_adapter_contract_and_named_function_goal(self) -> None:
        prompt = EXPERIMENT.authorship_prompt()
        self.assertIn("standard_agent_control(session, options", prompt)
        self.assertIn("passed directly (not a lambda)", prompt)
        self.assertIn("standard_verify_finish(candidate, session, state)", prompt)
        self.assertIn('checked["specialized"] = True', prompt)
        self.assertIn("attribute assignment", prompt)
        self.assertNotIn("def standard_agent_control(", prompt)

    def test_library_prefix_removes_default_invocation(self) -> None:
        source = EXPERIMENT.DEFAULT_LIBRARY.read_text(encoding="utf-8")
        prefix = EXPERIMENT.library_prefix(source)
        self.assertIn("def standard_agent_control(", prefix)
        self.assertNotIn(EXPERIMENT.INVOCATION_MARKER, prefix)
        self.assertNotIn("session = agent_session()", prefix)

    def test_feature_gate_accepts_named_specialization(self) -> None:
        features = EXPERIMENT.source_features(VALID_SOURCE)
        self.assertTrue(EXPERIMENT.required_features_ok(features), features)
        invalid = EXPERIMENT.source_features(
            VALID_SOURCE.replace(
                "specialized_verify_finish,",
                "lambda candidate, session, state: candidate,",
            )
        )
        self.assertFalse(EXPERIMENT.required_features_ok(invalid), invalid)

    def test_fixture_gate_requires_annotation_provenance_and_rollback(self) -> None:
        summary = {
            "ok": True,
            "rolled_back": True,
            "controller_report": {
                "answer": EXPERIMENT.EXPECTED_ANSWER,
                "specialized": True,
                "specialization_label": EXPERIMENT.EXPECTED_LABEL,
                "verified_round": 1,
                "_control": {"name": "stone.standard_action_v12"},
            },
        }
        self.assertEqual(EXPERIMENT.fixture_gate(summary), (True, []))
        summary["rolled_back"] = False
        ok, violations = EXPERIMENT.fixture_gate(summary)
        self.assertFalse(ok)
        self.assertIn("attempt did not roll back", violations)


if __name__ == "__main__":
    unittest.main()

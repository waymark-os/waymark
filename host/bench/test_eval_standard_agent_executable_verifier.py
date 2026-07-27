#!/usr/bin/env python3
"""Unit tests for the opaque executable-verifier specialization."""

from __future__ import annotations

import importlib.util
import unittest
from pathlib import Path


MODULE_PATH = Path(__file__).with_name(
    "eval_standard_agent_executable_verifier.py"
)
SPEC = importlib.util.spec_from_file_location(
    "eval_standard_agent_executable_verifier",
    MODULE_PATH,
)
assert SPEC and SPEC.loader
EXPERIMENT = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(EXPERIMENT)


VALID_SOURCE = """
def verify_tests(candidate, session, state):
    checked = standard_verify_finish(candidate, session, state)
    result = run(
        ["python3", "/app/test_solution.py"],
        cwd="/app",
        timeout_ms=10000,
        max_stdout_bytes=4096,
        max_stderr_bytes=4096,
    )
    if not result.ok:
        fail("visible tests failed", code="task_verification_failed")
    checked["task_verified"] = True
    checked["verifier_status"] = result.exit_code
    checked["verifier_transition_id"] = result.transition_id
    checked["verified_round"] = state.rounds
    return checked

session = agent_session()
options = standard_agent_options(session.input)
result = standard_agent_control(
    session,
    options,
    standard_shell_dispatch,
    verify_tests,
    standard_record_progress,
)
emit(result)
"""


class StandardAgentExecutableVerifierTests(unittest.TestCase):
    def test_prompt_hides_solution_and_separates_authority(self) -> None:
        prompt = EXPERIMENT.authorship_prompt()
        self.assertIn(EXPERIMENT.VISIBLE_TEST_COMMAND, prompt)
        self.assertIn("hidden read-only tests", prompt)
        self.assertIn("not the authority", prompt)
        self.assertNotIn("normalize_words(words)", prompt)

    def test_feature_gate_accepts_one_bounded_visible_verifier(self) -> None:
        features = EXPERIMENT.source_features(VALID_SOURCE)
        self.assertTrue(EXPERIMENT.required_features_ok(features), features)
        invalid = EXPERIMENT.source_features(
            VALID_SOURCE.replace("timeout_ms=10000", "timeout_ms=60000")
        )
        self.assertFalse(EXPERIMENT.required_features_ok(invalid), invalid)

    def test_checkpoint_gate_requires_expected_status_and_branch_rollback(self) -> None:
        summary = {
            "checkpoint_verifier": {
                "status": 0,
                "rolled_back": True,
                "command": EXPERIMENT.HIDDEN_TEST_COMMAND,
            }
        }
        self.assertEqual(EXPERIMENT.checkpoint_gate(summary, 0), [])
        summary["checkpoint_verifier"]["rolled_back"] = False
        self.assertIn(
            "checkpoint verifier branch did not roll back",
            EXPERIMENT.checkpoint_gate(summary, 0),
        )


if __name__ == "__main__":
    unittest.main()

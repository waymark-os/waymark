#!/usr/bin/env python3

from __future__ import annotations

import sys
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "host/bench"))

import eval_stone_attempt_memory_authorship as authorship
import eval_stone_attempt_memory_restart as restart


class AttemptMemoryAuthorshipTests(unittest.TestCase):
    def test_prompt_does_not_leak_the_hidden_action(self) -> None:
        prompt = authorship.authorship_prompt({})
        self.assertNotIn(restart.ACTION_TOKEN, prompt)
        self.assertIn("controller", prompt)
        self.assertIn("context_project", prompt)

    def test_known_good_program_has_required_visible_features(self) -> None:
        source = (
            ROOT / "examples/scripts/attempt_memory_model_restart_experiment.stone"
        ).read_text(encoding="utf-8")
        features = authorship.source_features(source)
        self.assertTrue(authorship.required_features_ok(features), features)

    def test_raw_transcript_feature_does_not_require_one_filename(self) -> None:
        features = authorship.source_features(
            'write_file("raw_requirement_transcript.json", json_dumps(messages))'
        )
        self.assertTrue(features["raw_transcript"])

    def test_result_gate_requires_unedited_behavioral_success(self) -> None:
        result = {
            "codex_exit_code": 0,
            "output_error": None,
            "codex_tool_calls": 0,
            "required_features_ok": True,
            "preflight": {"ok": True},
            "restart_execution": {"ok": True},
        }
        ok, violations = authorship.result_gate(result)
        self.assertTrue(ok, violations)
        result["restart_execution"] = {"ok": False}
        ok, violations = authorship.result_gate(result)
        self.assertFalse(ok)
        self.assertTrue(any("unedited source" in violation for violation in violations))


if __name__ == "__main__":
    unittest.main()

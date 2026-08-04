#!/usr/bin/env python3
"""Unit checks for the V14 runtime-learning fixture sequences."""

from __future__ import annotations

import importlib.util
import json
import sys
import unittest
from pathlib import Path


BENCH = Path(__file__).resolve().parent
sys.path.insert(0, str(BENCH))
MODULE_PATH = BENCH / "eval_standard_agent_runtime_learning.py"
SPEC = importlib.util.spec_from_file_location("runtime_learning", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
runtime_learning = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(runtime_learning)


def action_at(sequence: list[str], index: int) -> dict:
    return json.loads(sequence[index])["actions"][0]


class RuntimeLearningCanaryTests(unittest.TestCase):
    def test_edit_sequence_uses_existing_exact_replace_action(self) -> None:
        sequence = runtime_learning.edit_sequence()
        self.assertEqual(
            [action_at(sequence, index).get("tool") for index in range(3)],
            ["write", "edit", "read"],
        )
        self.assertEqual(action_at(sequence, 1)["input"]["old"], "NOT ")
        self.assertEqual(action_at(sequence, 1)["input"]["new"], "")
        self.assertIn("final", action_at(sequence, 3))

    def test_stagnation_sequence_repeats_one_exact_action_four_times(self) -> None:
        sequence = runtime_learning.stagnation_sequence()
        self.assertEqual(len(sequence), 4)
        self.assertEqual(len(set(sequence)), 1)

    def test_critic_sequence_exhausts_two_audits_before_third_finish(self) -> None:
        sequence = runtime_learning.critic_exhaustion_sequence()
        self.assertEqual(len(sequence), 6)
        self.assertEqual(action_at(sequence, 0)["tool"], "write")
        self.assertIn("final", action_at(sequence, 1))
        self.assertFalse(json.loads(sequence[2])["approved"])
        self.assertIn("final", action_at(sequence, 3))
        self.assertFalse(json.loads(sequence[4])["approved"])
        self.assertIn("final", action_at(sequence, 5))


if __name__ == "__main__":
    unittest.main()

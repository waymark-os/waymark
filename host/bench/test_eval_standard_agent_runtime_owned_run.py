#!/usr/bin/env python3

from __future__ import annotations

import json
import unittest

import eval_standard_agent_runtime_owned_run as experiment


class StandardAgentRuntimeOwnedRunTests(unittest.TestCase):
    def test_sequence_uses_one_terminal_run_action_without_poll_actions(self) -> None:
        values = [json.loads(item) for item in experiment.sequence()]
        actions = [value["actions"][0] for value in values[:3]]

        self.assertEqual(actions[0]["tool"], "run_complete")
        self.assertEqual(actions[0]["input"]["timeout_ms"], 5000)
        self.assertEqual(actions[1]["tool"], "read")
        self.assertIn("final", actions[2])
        self.assertNotIn(
            "run_wait",
            [action.get("tool") for action in actions],
        )
        self.assertNotIn(
            "run_status",
            [action.get("tool") for action in actions],
        )
        self.assertTrue(values[3]["approved"])


if __name__ == "__main__":
    unittest.main()

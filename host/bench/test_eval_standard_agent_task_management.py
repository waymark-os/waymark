#!/usr/bin/env python3
"""Tests for the standard V7 bounded owned-task experiment."""

from __future__ import annotations

import json
import unittest

import eval_standard_agent_task_management as experiment


class StandardAgentTaskManagementTests(unittest.TestCase):
    def test_sequence_exercises_finish_block_and_active_run_cap(self) -> None:
        values = [json.loads(item) for item in experiment.sequence()]
        self.assertEqual(len(values), 7)
        self.assertEqual(values[0]["actions"][0]["tool"], "run_start")
        self.assertIn("final", values[1]["actions"][0])
        self.assertEqual(values[2]["actions"][0]["tool"], "run_start")
        self.assertEqual(values[3]["actions"][0]["tool"], "run_wait")
        self.assertEqual(values[4]["actions"][0]["tool"], "read")
        self.assertIn("final", values[5]["actions"][0])
        self.assertTrue(values[6]["approved"])

    def test_suffix_keeps_lifecycle_adapter_visible(self) -> None:
        self.assertIn("def fixture_task_dispatch(", experiment.SUFFIX)
        self.assertIn('options["max_active_runs"] = 1', experiment.SUFFIX)
        self.assertIn('"run_id": "fixture-run-1"', experiment.SUFFIX)
        self.assertIn("standard_agent_control(", experiment.SUFFIX)


if __name__ == "__main__":
    unittest.main()

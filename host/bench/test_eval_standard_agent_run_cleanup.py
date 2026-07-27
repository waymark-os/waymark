#!/usr/bin/env python3
"""Tests for the standard-agent owned-run cleanup experiment."""

from __future__ import annotations

import json
import unittest

import eval_standard_agent_run_cleanup as experiment


class StandardAgentRunCleanupTests(unittest.TestCase):
    def test_sequence_times_out_then_recovers_and_finishes(self) -> None:
        sequence = [json.loads(value) for value in experiment.cleanup_sequence()]
        self.assertEqual(len(sequence), 5)
        action = sequence[0]["actions"][0]
        self.assertEqual(action["tool"], "run_linux")
        self.assertEqual(action["input"]["timeout_ms"], 1000)
        self.assertEqual(sequence[1]["actions"][0]["tool"], "write")
        self.assertEqual(sequence[2]["actions"][0]["tool"], "read")
        self.assertIn("final", sequence[3]["actions"][0])
        self.assertTrue(sequence[4]["approved"])


if __name__ == "__main__":
    unittest.main()

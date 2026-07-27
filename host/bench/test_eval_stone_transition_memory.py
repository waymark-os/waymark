#!/usr/bin/env python3

from __future__ import annotations

import sys
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "host/bench"))

import eval_stone_transition_memory as experiment


class TransitionMemoryCanaryTests(unittest.TestCase):
    def test_gate_requires_causal_projection_and_post_hook_linkage(self) -> None:
        payload = {
            "ok": True,
            "value": {
                "control": "fixture: which codename?",
                "treatment": "active attempt memory: cobalt",
                "control_transition_id": "transition-1",
                "treatment_transition_id": "transition-2",
                "retained": [
                    {"content": {"transition_id": "transition-2", "ok": True}}
                ],
            },
            "diagnostics": {
                "transitions": [
                    {"id": "transition-1", "phase": "start"},
                    {"id": "transition-1", "phase": "effect"},
                    {"id": "transition-2", "phase": "start"},
                    {"id": "transition-2", "phase": "pre"},
                    {"id": "transition-2", "phase": "effect"},
                    {"id": "transition-2", "phase": "post"},
                ]
            },
        }
        ok, violations = experiment.canary_gate(payload)
        self.assertTrue(ok, violations)

    def test_gate_rejects_memory_in_control(self) -> None:
        ok, violations = experiment.canary_gate(
            {
                "ok": True,
                "value": {
                    "control": "cobalt",
                    "treatment": "cobalt",
                    "control_transition_id": "transition-1",
                    "treatment_transition_id": "transition-2",
                    "retained": [],
                },
            }
        )
        self.assertFalse(ok)
        self.assertTrue(any("control" in violation for violation in violations))


if __name__ == "__main__":
    unittest.main()

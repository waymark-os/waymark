#!/usr/bin/env python3

from __future__ import annotations

import sys
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "host/bench"))

import eval_stone_behavioral_memory as experiment


class BehavioralMemoryExperimentTests(unittest.TestCase):
    def test_gate_accepts_matched_causal_success(self) -> None:
        token = "opaque-action"
        results = []
        transitions = [
            {"id": "transition-1", "phase": "start"},
            {"id": "transition-1", "phase": "effect"},
            {"id": "transition-1", "phase": "post"},
        ]
        next_id = 2
        for mode in experiment.CELL_ORDER:
            model_id = f"transition-{next_id}"
            action_id = f"transition-{next_id + 1}"
            next_id += 2
            selected = token if mode == "M" else "insufficient_evidence"
            results.append(
                {
                    "mode": mode,
                    "selected_action": selected,
                    "materialized_action": selected,
                    "model_transition_id": model_id,
                    "action_transition_id": action_id,
                    "provider": "codex-chatgpt",
                    "model": "test-model",
                    "usage": {"input_tokens": 10, "output_tokens": 2},
                }
            )
            transitions.append({"id": model_id, "phase": "start"})
            if mode == "M":
                transitions.append({"id": model_id, "phase": "pre"})
            transitions.append({"id": model_id, "phase": "effect"})
            transitions.extend(
                [
                    {"id": action_id, "phase": "start"},
                    {"id": action_id, "phase": "effect"},
                ]
            )

        payload = {
            "ok": True,
            "value": {
                "early_transition_id": "transition-1",
                "recent_message_count": experiment.RECENT_MESSAGE_COUNT,
                "retained": [
                    {
                        "key": "requirement.pivot_action",
                        "content": {
                            "action_token": token,
                            "source_transition_id": "transition-1",
                        },
                    }
                ],
                "results": results,
            },
            "diagnostics": {
                "transitions": transitions,
                "context": {
                    "events": [
                        {"op": "write", "key": "requirement.pivot_action"},
                        {"op": "project", "selected": ["context-item-1"]},
                        {"op": "project", "selected": ["context-item-1"]},
                        {"op": "read", "selected": ["context-item-1"]},
                    ]
                },
            },
        }
        ok, violations, metrics = experiment.behavioral_gate(
            payload,
            action_token=token,
            model="test-model",
        )
        self.assertTrue(ok, violations)
        self.assertEqual(metrics["m_correct"], 2)
        self.assertEqual(metrics["n_correct"], 0)

    def test_gate_rejects_control_leakage(self) -> None:
        payload = {
            "ok": True,
            "value": {
                "early_transition_id": "transition-1",
                "recent_message_count": experiment.RECENT_MESSAGE_COUNT,
                "retained": [],
                "results": [
                    {"mode": mode, "selected_action": "leaked", "materialized_action": "leaked"}
                    for mode in experiment.CELL_ORDER
                ],
            },
        }
        ok, violations, _ = experiment.behavioral_gate(
            payload,
            action_token="leaked",
            model="test-model",
        )
        self.assertFalse(ok)
        self.assertTrue(any("control selected" in violation for violation in violations))


if __name__ == "__main__":
    unittest.main()

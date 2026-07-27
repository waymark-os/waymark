#!/usr/bin/env python3

from __future__ import annotations

import sys
import unittest
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "host/bench"))

import eval_stone_attempt_memory_required_projection as experiment


def transition_events(transition_id: str) -> list[dict[str, str]]:
    return [
        {"id": transition_id, "phase": phase}
        for phase in ("start", "pre", "effect", "post")
    ]


def cell(mode: str) -> dict[str, Any]:
    required = ["requirement.target"] if mode == "R" else []
    keys = ["requirement.target"] if mode == "R" else ["goal.active"]
    transition_id = f"transition-{mode}"
    child = {
        "mode": mode,
        "selected_target": (
            experiment.CURRENT_TARGET if mode == "R" else "insufficient_evidence"
        ),
        "projection_keys": keys,
        "required_keys": required,
        "projection_tokens": 72,
        "projection_truncated": True,
        "contains_current": mode == "R",
        "contains_decoy": False,
        "memory_revision": 4,
        "provider": "codex-chatgpt",
        "model": "test-model",
        "usage": {"input_tokens": 160},
        "transition_id": transition_id,
    }
    return {
        "payload": {
            "ok": True,
            "value": {
                "mode": mode,
                "parent_memory_revision": 3,
                "fork_origin_revision": 3,
                "child_result": child,
                "clean": True,
            },
        },
        "child_payload": {
            "ok": True,
            "value": child,
            "diagnostics": {"transitions": transition_events(transition_id)},
        },
    }


class RequiredProjectionExperimentTests(unittest.TestCase):
    def test_gate_accepts_fail_closed_unpinned_and_required_recovery(self) -> None:
        cells = {mode: cell(mode) for mode in experiment.MODES}
        ok, violations, metrics = experiment.experiment_gate(
            cells,
            model="test-model",
            current_target=experiment.CURRENT_TARGET,
            decoy_target=experiment.DECOY_TARGET,
        )
        self.assertTrue(ok, violations)
        self.assertEqual(metrics["model_calls"], 2)
        self.assertEqual(
            metrics["cells"]["U"]["selected_target"],
            "insufficient_evidence",
        )
        self.assertEqual(
            metrics["cells"]["R"]["selected_target"],
            experiment.CURRENT_TARGET,
        )

    def test_gate_rejects_missing_required_key_and_decoy_selection(self) -> None:
        cells = {mode: cell(mode) for mode in experiment.MODES}
        required = cells["R"]["payload"]["value"]["child_result"]
        required["projection_keys"] = ["decision.latest"]
        required["required_keys"] = []
        required["contains_current"] = False
        required["contains_decoy"] = True
        required["selected_target"] = experiment.DECOY_TARGET
        ok, violations, _ = experiment.experiment_gate(
            cells,
            model="test-model",
            current_target=experiment.CURRENT_TARGET,
            decoy_target=experiment.DECOY_TARGET,
        )
        self.assertFalse(ok)
        self.assertTrue(any("required key" in violation for violation in violations))
        self.assertTrue(any("pending decoy" in violation for violation in violations))
        self.assertTrue(any("was steered" in violation for violation in violations))


if __name__ == "__main__":
    unittest.main()

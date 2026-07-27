#!/usr/bin/env python3

from __future__ import annotations

import sys
import unittest
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "host/bench"))

import eval_stone_attempt_memory_restart as experiment


def transition_events(transition_id: str, phases: list[str]) -> list[dict[str, Any]]:
    return [{"id": transition_id, "phase": phase} for phase in phases]


def cell(mode: str, token: str, *, input_tokens: int) -> dict[str, Any]:
    final = "insufficient_evidence" if mode == "N" else token
    seed = {
        "ok": True,
        "value": {
            "phase": "seed",
            "mode": mode,
            "early_transition_id": "transition-1",
            "retained": [] if mode != "M" else [{"key": "requirement.pivot_action"}],
        },
        "diagnostics": {
            "transitions": transition_events(
                "transition-1", ["start", "effect", "post"]
            )
        },
    }
    ids = ["transition-1", "transition-3", "transition-2", "transition-4"]
    events: list[dict[str, Any]] = []
    for transition_id in ids:
        events.extend(
            transition_events(transition_id, ["start", "pre", "effect", "post"])
        )
    retained = []
    if mode == "M":
        retained = [
            {"key": "requirement.pivot_action"},
            {"key": "decision.last"},
            {"key": "outcome.last_tool"},
        ]
    restore = {
        "ok": True,
        "value": {
            "phase": "restore",
            "mode": mode,
            "decisions": ["diagnostic_probe", final],
            "selected_action": final,
            "materialized_action": final,
            "failed_probe": {
                "ok": False,
                "exit_code": 7,
                "stderr": "diagnostic failed; do not retry\n",
            },
            "model_transition_ids": ["transition-1", "transition-3"],
            "action_transition_ids": ["transition-2", "transition-4"],
            "provider": "codex-chatgpt",
            "model": "test-model",
            "usage": [
                {"input_tokens": input_tokens // 2},
                {"input_tokens": input_tokens - input_tokens // 2},
            ],
            "retained": retained,
        },
        "diagnostics": {"transitions": events},
    }
    return {
        "seed": seed,
        "restore": restore,
        "attempt_info": {
            "memory_revision": "5" if mode == "M" else "0",
            "metadata": {"controller_run_count": "2"},
        },
    }


class AttemptMemoryRestartExperimentTests(unittest.TestCase):
    def test_gate_accepts_restart_recovery_and_token_savings(self) -> None:
        token = "opaque-action"
        cells = {
            "N": cell("N", token, input_tokens=200),
            "T": cell("T", token, input_tokens=1200),
            "M": cell("M", token, input_tokens=360),
        }
        ok, violations, metrics = experiment.experiment_gate(
            cells,
            action_token=token,
            model="test-model",
        )
        self.assertTrue(ok, violations)
        self.assertEqual(metrics["cells"]["M"]["memory_revision"], 5)
        self.assertFalse(metrics["cells"]["N"]["task_success"])
        self.assertTrue(metrics["cells"]["T"]["task_success"])
        self.assertTrue(metrics["cells"]["M"]["task_success"])
        self.assertEqual(metrics["transcript_minus_memory_input_tokens"], 840)

    def test_gate_rejects_hidden_token_leakage_into_no_memory_cell(self) -> None:
        token = "opaque-action"
        cells = {
            "N": cell("N", token, input_tokens=200),
            "T": cell("T", token, input_tokens=1200),
            "M": cell("M", token, input_tokens=360),
        }
        cells["N"]["restore"]["value"]["decisions"][1] = token
        cells["N"]["restore"]["value"]["selected_action"] = token
        cells["N"]["restore"]["value"]["materialized_action"] = token
        ok, violations, _ = experiment.experiment_gate(
            cells,
            action_token=token,
            model="test-model",
        )
        self.assertFalse(ok)
        self.assertTrue(any("N final decision" in violation for violation in violations))

    def test_gate_accepts_structured_decision_records(self) -> None:
        token = "opaque-action"
        cells = {
            "N": cell("N", token, input_tokens=200),
            "T": cell("T", token, input_tokens=1200),
            "M": cell("M", token, input_tokens=360),
        }
        for value in cells.values():
            decisions = value["restore"]["value"]["decisions"]
            value["restore"]["value"]["decisions"] = [
                {"selected_action": decision} for decision in decisions
            ]
        ok, violations, _ = experiment.experiment_gate(
            cells,
            action_token=token,
            model="test-model",
        )
        self.assertTrue(ok, violations)


if __name__ == "__main__":
    unittest.main()

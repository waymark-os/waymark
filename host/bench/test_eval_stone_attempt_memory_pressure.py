#!/usr/bin/env python3

from __future__ import annotations

import sys
import unittest
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "host/bench"))

import eval_stone_attempt_memory_pressure as experiment


def transition_events(transition_id: str) -> list[dict[str, str]]:
    return [
        {"id": transition_id, "phase": phase}
        for phase in ("start", "pre", "effect", "post")
    ]


def cell(mode: str, target: str, *, memory_bytes: int) -> dict[str, Any]:
    count = experiment.EXPECTED_ITEM_COUNTS[mode]
    pressure = experiment.EXPECTED_PRESSURE_COUNTS[mode]
    transition_id = f"transition-{mode}"
    child = {
        "selected_target": target,
        "projection_keys": ["requirement.target", "goal.active"],
        "projection_revision": experiment.SEED_REVISION,
        "projection_estimated_tokens": 120,
        "projection_truncated": True,
        "projection_contains_current": True,
        "projection_contains_old": False,
        "item_count_before": count,
        "item_count_after": count,
        "memory_revision": experiment.SEED_REVISION + 1,
        "provider": "codex-chatgpt",
        "model": "test-model",
        "usage": {"input_tokens": 180},
        "transition_id": transition_id,
    }
    seed_value = {
        "phase": "seed",
        "mode": mode,
        "memory_revision": experiment.SEED_REVISION,
        "item_count": count,
        "pressure_item_count": pressure,
        "latest_decision_sequence": 63,
        "latest_pressure_sequence": 251,
        "requirement_target": target,
        "requirement_supersedes": "memory-item-1",
        "resolved_risk_count": 0,
    }
    restore_value = {
        "phase": "restore",
        "mode": mode,
        "parent_revision_before": experiment.SEED_REVISION,
        "parent_revision_after": experiment.SEED_REVISION + 1,
        "parent_item_count_before": count,
        "parent_item_count_after": count,
        "requirement_target": target,
        "requirement_supersedes": "memory-item-1",
        "resolved_risk_count": 0,
        "fork_origin_revision": experiment.SEED_REVISION,
        "child_result": child,
        "clean": True,
    }
    return {
        "seed": {"ok": True, "value": seed_value},
        "restore": {"ok": True, "value": restore_value},
        "child_payload": {
            "ok": True,
            "value": child,
            "diagnostics": {"transitions": transition_events(transition_id)},
        },
        "root_info": {
            "memory_revision": str(experiment.SEED_REVISION + 1),
            "metadata": {"controller_run_count": "2"},
        },
        "child_info": {"memory_revision": str(experiment.SEED_REVISION + 1)},
        "root_memory_bytes": memory_bytes,
        "child_memory_bytes": memory_bytes,
    }


class AttemptMemoryPressureExperimentTests(unittest.TestCase):
    def test_gate_accepts_keyed_storage_reduction_and_behavior(self) -> None:
        cells = {
            "A": cell("A", experiment.CURRENT_TARGET, memory_bytes=180_000),
            "K": cell("K", experiment.CURRENT_TARGET, memory_bytes=12_000),
        }
        ok, violations, metrics = experiment.experiment_gate(
            cells,
            model="test-model",
            current_target=experiment.CURRENT_TARGET,
            old_target=experiment.OLD_TARGET,
        )
        self.assertTrue(ok, violations)
        self.assertEqual(metrics["model_calls"], 2)
        self.assertGreater(metrics["keyed_storage_reduction_fraction"], 0.9)

    def test_gate_rejects_obsolete_target_and_unbounded_keyed_frontier(self) -> None:
        cells = {
            "A": cell("A", experiment.CURRENT_TARGET, memory_bytes=180_000),
            "K": cell("K", experiment.CURRENT_TARGET, memory_bytes=12_000),
        }
        keyed_child = cells["K"]["restore"]["value"]["child_result"]
        keyed_child["selected_target"] = experiment.OLD_TARGET
        keyed_child["projection_contains_old"] = True
        cells["K"]["seed"]["value"]["item_count"] = 256
        ok, violations, _ = experiment.experiment_gate(
            cells,
            model="test-model",
            current_target=experiment.CURRENT_TARGET,
            old_target=experiment.OLD_TARGET,
        )
        self.assertFalse(ok)
        self.assertTrue(any("seed item count" in violation for violation in violations))
        self.assertTrue(any("obsolete opaque target" in violation for violation in violations))
        self.assertTrue(any("was steered" in violation for violation in violations))


if __name__ == "__main__":
    unittest.main()

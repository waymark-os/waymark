#!/usr/bin/env python3

from __future__ import annotations

import sys
import unittest
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "host/bench"))

import eval_stone_attempt_memory_fork_model as experiment


def result(candidate: str, decision: str, *, forked: bool) -> dict[str, Any]:
    return {
        "candidate": candidate,
        "decision": decision,
        "projected_keys": ["requirement.target"] if forked else [],
        "memory_revision": 2 if forked else 1,
        "provider": "codex-chatgpt",
        "model": "test-model",
        "usage": {"input_tokens": 100},
    }


def cell(arm: str, target: str, other: str) -> dict[str, Any]:
    forked = arm == "F"
    first_attempt = f"attempt-{arm}-1"
    second_attempt = f"attempt-{arm}-2"
    children = [
        {
            "attempt": first_attempt,
            "result": result(
                target,
                "select" if forked else "insufficient",
                forked=forked,
            ),
        },
        {
            "attempt": second_attempt,
            "result": result(
                other,
                "reject" if forked else "insufficient",
                forked=forked,
            ),
        },
    ]
    return {
        "payload": {
            "ok": True,
            "value": {
                "arm": arm,
                "children": children,
                "accepted": first_attempt if forked else "",
                "promoted": forked,
                "parent_memory_revision": 2 if forked else 1,
                "parent_keys": (
                    ["requirement.target", "candidate.accepted"]
                    if forked
                    else ["requirement.target"]
                ),
                "clean": True,
            },
        }
    }


class AttemptMemoryForkModelExperimentTests(unittest.TestCase):
    def test_gate_accepts_inheritance_isolation_and_explicit_promotion(self) -> None:
        target = "opaque-target"
        candidates = (target, "opaque-other")
        cells = {
            arm: cell(arm, *candidates)
            for arm in experiment.ARMS
        }
        ok, violations, metrics = experiment.experiment_gate(
            cells,
            target=target,
            candidates=candidates,
            model="test-model",
        )
        self.assertTrue(ok, violations)
        self.assertEqual(metrics["model_calls"], 4)
        self.assertEqual(metrics["input_tokens"], 400)

    def test_gate_rejects_spawn_memory_leakage(self) -> None:
        target = "opaque-target"
        candidates = (target, "opaque-other")
        cells = {
            arm: cell(arm, *candidates)
            for arm in experiment.ARMS
        }
        spawn_target = cells["S"]["payload"]["value"]["children"][0]["result"]
        spawn_target["decision"] = "select"
        spawn_target["projected_keys"] = ["requirement.target"]
        ok, violations, _ = experiment.experiment_gate(
            cells,
            target=target,
            candidates=candidates,
            model="test-model",
        )
        self.assertFalse(ok)
        self.assertTrue(any("S decisions" in violation for violation in violations))
        self.assertTrue(any("unexpectedly inherited" in violation for violation in violations))


if __name__ == "__main__":
    unittest.main()

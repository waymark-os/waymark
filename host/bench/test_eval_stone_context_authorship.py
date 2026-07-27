#!/usr/bin/env python3

from __future__ import annotations

import sys
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "host/bench"))

import eval_stone_context_authorship as experiment


class ContextAuthorshipExperimentTests(unittest.TestCase):
    def test_prompt_contains_each_live_context_operation(self) -> None:
        topics = {
            name: {"signature": name + "(...)"}
            for name in (
                "model_call",
                "context_write",
                "context_read",
                "context_project",
            )
        }
        prompt = experiment.authorship_prompt(topics)
        for name in topics:
            self.assertIn(name, prompt)
        self.assertIn("context_turn_limit", prompt)

    def test_source_features_require_visible_context_control(self) -> None:
        source = """
for turn in range(6):
    projection = context_project("task", 128)
    response = model_call([])
    action = json_loads(response.content)
    context_write("requirement.output", "requirement", "ready")
    context_write("outcome.probe", "outcome", {"ok": True})
    context_read(keys=["requirement.output"])
    if "final" in action:
        emit(action)
fail("limit", code="context_turn_limit")
"""
        features = experiment.source_features(source)
        for name, value in features.items():
            if name in ("forbidden_import", "hidden_control"):
                self.assertFalse(value, name)
            else:
                self.assertTrue(value, name)

    def test_repair_prompt_includes_bounded_diagnostic_and_string_guidance(self) -> None:
        prompt = experiment.repair_prompt(
            "emit(model_call(messages))",
            {
                "fixture_execution": {
                    "response": {
                        "error": {
                            "code": "model_invalid_request",
                            "detail": "message content error",
                        }
                    }
                }
            },
            {"model_call": {"signature": "model_call(messages)"}},
        )
        self.assertIn("model_invalid_request", prompt)
        self.assertIn("json_dumps", prompt)
        self.assertIn("emit(model_call(messages))", prompt)

    def test_context_gate_requires_causal_activation_and_finish_read(self) -> None:
        payload = {
            "ok": True,
            "value": {
                "answer": "ready",
                "turns": 4,
                "ledger": [{"key": "requirement.output"}],
                "projection": {"items": [{"id": "context-item-1"}]},
            },
            "diagnostics": {
                "context": {
                    "events": [
                        {"op": "project", "selected": []},
                        {"op": "write", "key": "requirement.output"},
                        {"op": "project", "selected": ["context-item-1"]},
                        {"op": "write", "key": "outcome.probe"},
                        {"op": "read", "query": "ready", "selected": ["context-item-1"]},
                        {"op": "read"},
                    ]
                }
            },
        }
        ok, violations = experiment.context_gate(payload)
        self.assertTrue(ok, violations)


if __name__ == "__main__":
    unittest.main()

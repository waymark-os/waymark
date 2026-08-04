#!/usr/bin/env python3

from __future__ import annotations

import unittest

import eval_standard_agent_fork_first_context as experiment


def synthetic_result() -> dict:
    return {
        "root_payload": {
            "ok": True,
            "value": {
                "child": "attempt-child",
                "fork_memory_revision": 1,
                "parent_memory_revision": 2,
                "parent_keys": [
                    experiment.PARENT_LATE_KEY,
                    experiment.REQUIRED_KEY,
                ],
                "clean": True,
                "child_result": {
                    "problem": experiment.PROBLEM,
                    "legacy_policy_in_input": False,
                    "session_context_prompt_view": {
                        "required_keys": [experiment.REQUIRED_KEY],
                    },
                    "agent_result": {
                        "answer": experiment.TARGET,
                        "_control": {
                            "name": "stone.standard_action_v13",
                            "model_calls": 1,
                            "actions": 1,
                            "initial_action_memory_required_keys": [
                                experiment.REQUIRED_KEY
                            ],
                            "initial_action_memory_policy_source": (
                                "attempt_admission"
                            ),
                            "initial_action_memory_projection_keys": [
                                experiment.REQUIRED_KEY,
                                "requirement.task",
                            ],
                        },
                    },
                },
            },
        },
        "child_payload": {
            "ok": True,
            "diagnostics": {
                "context": {
                    "events": [
                        {
                            "op": "project",
                            "required_keys": [experiment.REQUIRED_KEY],
                            "selected": [
                                experiment.REQUIRED_KEY,
                                "requirement.task",
                            ],
                        }
                    ]
                }
            },
        },
        "projection_trace": [
            {
                "op": "attempt.memory.project",
                "attempt": "attempt-child",
                "required_keys": [experiment.REQUIRED_KEY],
            }
        ],
        "model_trace": [
            {
                "op": "attempt.rpc.model.call",
                "attempt": "attempt-child",
                "provider": "fixture",
                "status": "ok",
            }
        ],
        "manifest": {"provider": "fixture"},
    }


class StandardAgentForkFirstContextTests(unittest.TestCase):
    def test_gate_accepts_exact_fork_frontier_projection(self) -> None:
        self.assertEqual(experiment.gate_result(synthetic_result()), [])

    def test_gate_rejects_post_fork_parent_memory(self) -> None:
        result = synthetic_result()
        control = result["root_payload"]["value"]["child_result"]["agent_result"][
            "_control"
        ]
        control["initial_action_memory_projection_keys"].append(
            experiment.PARENT_LATE_KEY
        )
        result["child_payload"]["diagnostics"]["context"]["events"][0][
            "selected"
        ].append(experiment.PARENT_LATE_KEY)

        violations = experiment.gate_result(result)

        self.assertTrue(any("post-fork" in item for item in violations))

    def test_composed_source_uses_required_initial_context_without_target_leak(self) -> None:
        source = "def standard_agent_control():\n    return None\n\nsession = agent_session()\n"

        composed = experiment.compose_source(source)

        self.assertIn("context_prompt_view={", composed)
        self.assertNotIn('"initial_action_memory_required_keys": [', composed)
        self.assertIn("attempt_fork(", composed)
        self.assertNotIn(experiment.TARGET, composed)


if __name__ == "__main__":
    unittest.main()

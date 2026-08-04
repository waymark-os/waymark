#!/usr/bin/env python3

from __future__ import annotations

import unittest

import eval_standard_agent_fork_portfolio as experiment


def child_entry(
    strategy: str,
    attempt: str,
    output: str,
    passed: bool,
) -> dict:
    return {
        "attempt": attempt,
        "strategy": strategy,
        "fork_memory_revision": 1,
        "result": {
            "strategy": strategy,
            "prepared_output": output,
            "shared_prefix": experiment.FRONTIER_MARKER,
            "legacy_policy_in_input": False,
            "session_context_prompt_view": {
                "required_keys": [experiment.REQUIRED_KEY],
            },
            "post_fork_parent_key_seen": False,
            "agent_result": {
                "answer": "candidate-prepared",
                "candidate_output": output,
                "passed": passed,
                "_control": {
                    "name": "stone.standard_action_v13",
                    "model_calls": 1,
                    "initial_action_memory_policy_source": "attempt_admission",
                    "initial_action_memory_required_keys": [
                        experiment.REQUIRED_KEY
                    ],
                    "initial_action_memory_projection_keys": [
                        experiment.REQUIRED_KEY
                    ],
                },
            },
        },
    }


def synthetic_result() -> dict:
    winner = "attempt-reverse"
    loser = "attempt-uppercase"
    children = [
        child_entry(
            experiment.LOSER_STRATEGY,
            loser,
            experiment.PROBLEM.upper(),
            False,
        ),
        child_entry(
            experiment.WINNER_STRATEGY,
            winner,
            experiment.EXPECTED,
            True,
        ),
    ]
    inspections = [
        {
            "attempt": entry["attempt"],
            "strategy": entry["strategy"],
            "resource_state": "retained",
            "resources_reclaimed": False,
            "trace_ops": [
                "attempt.memory.project",
                "attempt.rpc.model.call",
            ],
            "summary_passed": entry["result"]["agent_result"]["passed"],
        }
        for entry in children
    ]
    child_attempts = [winner, loser]
    return {
        "root_attempt": "attempt-root",
        "root_payload": {
            "ok": True,
            "value": {
                "answer": experiment.EXPECTED,
                "winner": winner,
                "loser": loser,
                "winner_strategy": experiment.WINNER_STRATEGY,
                "loser_strategy": experiment.LOSER_STRATEGY,
                "accepted": winner,
                "baseline_clean": True,
                "clean": True,
                "parent_keys": [
                    experiment.PARENT_LATE_KEY,
                    experiment.REQUIRED_KEY,
                ],
                "parent_memory_revision": 2,
                "baseline_result": {
                    "problem_seen": False,
                    "verifier_seen": False,
                    "marker_seen": False,
                    "memory_seen": False,
                    "parent_attempt": "attempt-root",
                    "canonical_readme": "fork portfolio canonical base",
                },
                "child_results": children,
                "pre_inspections": inspections,
                "post_cleanup": {
                    "winner_resource_state": "reclaimed",
                    "winner_resources_reclaimed": True,
                    "loser_resource_state": "reclaimed",
                    "loser_resources_reclaimed": True,
                },
            },
        },
        "fork_trace": [
            {
                "attempt": attempt,
                "context_prompt_required_keys": [experiment.REQUIRED_KEY],
            }
            for attempt in child_attempts
        ],
        "projection_trace": [
            {
                "attempt": attempt,
                "required_keys": [experiment.REQUIRED_KEY],
            }
            for attempt in child_attempts
        ],
        "model_trace": [
            {
                "attempt": attempt,
                "provider": "fixture",
                "status": "ok",
            }
            for attempt in child_attempts
        ],
        "trace_counts": {"attempt.accept": 1},
        "manifest": {"provider": "fixture"},
    }


class StandardAgentForkPortfolioTests(unittest.TestCase):
    def test_gate_accepts_isolated_verified_portfolio(self) -> None:
        self.assertEqual(experiment.gate_result(synthetic_result()), [])

    def test_gate_rejects_reclaim_before_inspection(self) -> None:
        result = synthetic_result()
        result["root_payload"]["value"]["pre_inspections"][0][
            "resource_state"
        ] = "reclaimed"

        violations = experiment.gate_result(result)

        self.assertTrue(any("not retained" in item for item in violations))

    def test_composed_source_exposes_portfolio_extension_points(self) -> None:
        standard = (
            "def standard_agent_control():\n"
            "    return None\n\n"
            "session = agent_session()\n"
        )
        portfolio = (
            "def portfolio_parent(input):\n"
            "    return attempt_fork(context_prompt_view={"
            '"required_keys": ["requirement.portfolio_target"]})\n'
        )

        composed = experiment.compose_source(standard, portfolio)

        self.assertIn("def standard_agent_control()", composed)
        self.assertIn("def portfolio_parent(input)", composed)
        self.assertIn("context_prompt_view=", composed)
        self.assertNotIn("session = agent_session()", composed)
        self.assertNotIn(experiment.EXPECTED, composed)

    def test_checked_in_portfolio_keeps_control_flow_visible(self) -> None:
        source = (
            experiment.ROOT
            / "examples/references/standard_attempt_fork_portfolio.stone"
        ).read_text(encoding="utf-8")

        for shape in (
            "def portfolio_read_only_dispatch(",
            "def portfolio_verify_finish(",
            "baseline = attempt_spawn(",
            "children.append(attempt_fork(",
            "context_prompt_view={",
            "inspection = attempt_inspect(",
            "accepted = attempt_accept(",
            "attempt_discard(loser",
            "cleanup = attempt_scope_close(scope)",
        ):
            self.assertIn(shape, source)
        self.assertNotIn(experiment.EXPECTED, source)


if __name__ == "__main__":
    unittest.main()

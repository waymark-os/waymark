#!/usr/bin/env python3

from __future__ import annotations

import unittest

import eval_standard_agent_restart_reconciliation as experiment


def inspection(active_runs: list[str]) -> dict[str, list[str]]:
    return {
        "active_runs": [str(len(active_runs))],
        "active_run": active_runs,
    }


class StandardAgentRestartReconciliationTests(unittest.TestCase):
    def test_gate_accepts_authoritative_recovery_and_cleanup(self) -> None:
        run_id = "run-recovered"
        result = {
            "controller_runs": [
                {
                    "payload": {
                        "ok": True,
                        "value": {
                            "phase": "operation_started_handle_unrecorded",
                            "still_running": True,
                            "memory": [],
                        },
                    },
                    "inspection": inspection([run_id]),
                },
                {
                    "payload": {
                        "ok": True,
                        "value": {
                            "phase": "reconciled_and_reaped",
                            "recovered": [run_id],
                            "remaining": [],
                            "metrics": {
                                "run_reconciliations": 1,
                                "recovered_active_runs": 1,
                                "run_termination_calls": 1,
                                "run_completions": 1,
                                "active_runs": 0,
                            },
                            "memory": [
                                {
                                    "key": "progress.active_runs",
                                    "status": "verified",
                                    "content": {"count": 0, "runs": []},
                                }
                            ],
                        },
                    },
                    "inspection": inspection([]),
                },
            ]
        }

        self.assertEqual(experiment.gate_result(result), [])

    def test_gate_rejects_a_stale_recovered_handle(self) -> None:
        result = {
            "controller_runs": [
                {
                    "payload": {
                        "value": {
                            "phase": "operation_started_handle_unrecorded",
                            "still_running": True,
                            "memory": [],
                        }
                    },
                    "inspection": inspection(["run-recovered"]),
                },
                {
                    "payload": {
                        "value": {
                            "phase": "reconciled_and_reaped",
                            "recovered": ["run-recovered"],
                            "remaining": ["run-recovered"],
                            "metrics": {},
                            "memory": [],
                        }
                    },
                    "inspection": inspection(["run-recovered"]),
                },
            ]
        }

        violations = experiment.gate_result(result)

        self.assertTrue(any("remained active" in item for item in violations))
        self.assertTrue(any("still reports" in item for item in violations))

    def test_composed_source_uses_the_standard_reconciler(self) -> None:
        source = "def standard_agent_control():\n    return None\n\nsession = agent_session()\n"

        composed = experiment.compose_source(source)

        self.assertIn("standard_reconcile_active_runs(state, options)", composed)
        self.assertIn("standard_reap_active_runs(state, options)", composed)
        self.assertNotIn("session = agent_session()\n", composed.split(experiment.SUFFIX)[0])


if __name__ == "__main__":
    unittest.main()

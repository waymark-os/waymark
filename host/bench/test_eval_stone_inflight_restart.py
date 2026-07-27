#!/usr/bin/env python3

from __future__ import annotations

import importlib.util
import unittest
from pathlib import Path


MODULE_PATH = Path(__file__).with_name("eval_stone_inflight_restart.py")
SPEC = importlib.util.spec_from_file_location("eval_stone_inflight_restart", MODULE_PATH)
assert SPEC and SPEC.loader
experiment = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(experiment)


def memory(state: dict, revision: int) -> list[dict]:
    return [
        {
            "key": "action.inflight",
            "revision": revision,
            "content": state,
        }
    ]


def payload(
    value: dict,
    transition_id: str | None = None,
    phases: list[str] | None = None,
) -> dict:
    transitions = [
        {"id": transition_id, "phase": phase}
        for phase in phases or []
        if transition_id is not None
    ]
    return {"ok": True, "value": value, "diagnostics": {"transitions": transitions}}


def synthetic_cell(boundary: str) -> dict:
    attempt = f"attempt-{boundary}"
    action_run = 2 if boundary == "prepared" else 1
    transition_id = experiment.expected_transition(action_run)
    completed = boundary != "started"
    terminal = {
        "phase": "terminal",
        "execution_state": "completed" if completed else "started_or_unknown",
        "transition_id": transition_id,
        "decision": "record_outcome" if completed else "replan",
        "outcome": {"ok": True, "exit_code": 0} if completed else None,
    }
    first = {
        "boundary": boundary,
        "controller_run_count": 1,
        "decision": {
            "prepared": "stop_before_execution",
            "started": "lose_outcome",
            "completed": "stop_before_consolidation",
        }[boundary],
        "transition_id": None if boundary == "prepared" else transition_id,
        "state": (
            {"phase": "prepared"}
            if boundary == "prepared"
            else {"phase": "started_or_unknown"}
        ),
        "effect_lines": [] if boundary == "prepared" else ["effect"],
        "memory": [],
    }
    second = {
        "boundary": boundary,
        "controller_run_count": 2,
        "decision": {
            "prepared": "resume_once",
            "started": "replan",
            "completed": "record_outcome",
        }[boundary],
        "transition_id": transition_id if boundary == "prepared" else None,
        "state": terminal,
        "effect_lines": ["effect"],
        "memory": memory(terminal, experiment.EXPECTED_REVISIONS[boundary]),
    }
    third = {
        **second,
        "controller_run_count": 3,
        "decision": "terminal_noop",
        "transition_id": None,
    }
    action_phases = (
        ["start", "pre", "effect"]
        if boundary == "started"
        else ["start", "pre", "effect", "post"]
    )
    payloads = [
        payload(
            first,
            transition_id if action_run == 1 else None,
            action_phases if action_run == 1 else None,
        ),
        payload(
            second,
            transition_id if action_run == 2 else None,
            action_phases if action_run == 2 else None,
        ),
        payload(third),
    ]
    revision = experiment.EXPECTED_REVISIONS[boundary]
    return {
        "attempt": attempt,
        "runs": [
            {
                "payload": item,
                "info": {
                    "memory_revision": str(
                        revision if index > 0 else max(1, revision - 2)
                    )
                },
            }
            for index, item in enumerate(payloads)
        ],
    }


class StoneInflightRestartTests(unittest.TestCase):
    def test_gate_accepts_three_restart_boundaries(self) -> None:
        cells = {
            boundary: synthetic_cell(boundary)
            for boundary in experiment.BOUNDARIES
        }

        ok, violations = experiment.experiment_gate(cells)

        self.assertTrue(ok)
        self.assertEqual(violations, [])

    def test_gate_rejects_duplicate_effect_after_unknown_start(self) -> None:
        cell = synthetic_cell("started")
        cell["runs"][1]["payload"]["value"]["effect_lines"].append("effect")

        violations = experiment.gate_cell("started", cell)

        self.assertIn(
            "started did not preserve exactly one external effect", violations
        )

    def test_gate_rejects_cross_restart_transition_id_reuse(self) -> None:
        cell = synthetic_cell("prepared")
        duplicate = experiment.expected_transition(2)
        cell["runs"][2]["payload"]["diagnostics"]["transitions"] = [
            {"id": duplicate, "phase": "start"}
        ]

        violations = experiment.gate_cell("prepared", cell)

        self.assertIn(
            "prepared terminal no-op emitted another transition", violations
        )
        self.assertIn(
            "prepared reused a transition ID across controller runs", violations
        )


if __name__ == "__main__":
    unittest.main()

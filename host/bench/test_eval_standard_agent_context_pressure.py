#!/usr/bin/env python3
"""Unit checks for the standard V7 action-context pressure canary."""

from __future__ import annotations

import importlib.util
import unittest
from pathlib import Path


MODULE_PATH = (
    Path(__file__).resolve().parent
    / "eval_standard_agent_context_pressure.py"
)
SPEC = importlib.util.spec_from_file_location("context_pressure", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
pressure = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(pressure)


class StandardContextPressureTests(unittest.TestCase):
    def test_sequence_has_repeated_large_probes_then_finish(self) -> None:
        sequence = pressure.pressure_sequence()
        self.assertEqual(len(sequence), pressure.PROBE_COUNT + 1)
        first = pressure.json.loads(sequence[0])
        final = pressure.json.loads(sequence[-1])
        self.assertEqual(first["actions"][0]["tool"], "run_linux")
        self.assertIn("BEGIN-MARKER", first["actions"][0]["input"]["argv"][2])
        self.assertIn("END-MARKER", first["actions"][0]["input"]["argv"][2])
        self.assertEqual(final["actions"][0]["final"]["answer"], pressure.ANSWER)

    def test_gate_accepts_bounded_control_and_head_tail_evidence(self) -> None:
        stdout = (
            "BEGIN-MARKER"
            + "X" * 100
            + "\n...<bounded-middle-omitted>...\n"
            + "X" * 100
            + "END-MARKER"
        )
        evidence = [
            {
                "key": f"evidence.action.{index}",
                "content": {
                    "result": {
                        "stdout": stdout,
                        "stdout_characters": pressure.RAW_STDOUT_CHARACTERS,
                        "stdout_truncated": True,
                    }
                },
            }
            for index in range(8)
        ]
        events = [
            {"op": "attempt.rpc.model.call", "input_tokens": 3000}
            for _ in range(pressure.PROBE_COUNT + 1)
        ] + [
            {"op": "attempt.rpc.linux.exec"}
            for _ in range(pressure.PROBE_COUNT)
        ]
        summary = {
            "ok": True,
            "rolled_back": True,
            "controller_report": {
                "_control": {
                    "name": "stone.standard_action_v14",
                    "actions": pressure.PROBE_COUNT + 1,
                    "tool_calls": pressure.PROBE_COUNT,
                    "model_calls": pressure.PROBE_COUNT + 1,
                    "memory_projections": pressure.PROBE_COUNT + 1,
                    "observation_truncations": pressure.PROBE_COUNT,
                    "observation_field_truncations": pressure.PROBE_COUNT,
                    "max_action_context_characters": pressure.CONTEXT_LIMIT,
                    "peak_action_context_characters": pressure.CONTEXT_LIMIT,
                    "context_messages_dropped": 2,
                }
            },
        }
        memory = {
            "items": evidence
            + [
                {"key": "requirement.task"},
                {"key": "progress.agent_control"},
            ]
        }
        self.assertEqual(
            pressure.gate(
                exit_code=0,
                summary=summary,
                events=events,
                memory=memory,
            ),
            [],
        )


if __name__ == "__main__":
    unittest.main()

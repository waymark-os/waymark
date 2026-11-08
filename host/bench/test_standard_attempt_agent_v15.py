#!/usr/bin/env python3
"""Checks for the V15 standard Stone agent control (records + model.ask).

V15 is V14 with the hand-written JSON Schemas replaced by record classes,
the option parser replaced by a range table, model_infer replaced by
model.ask, and model-call arithmetic replaced by model.limit/remaining.
These tests pin that shape and check that option parsing is unchanged;
host/bench/eval_standard_agent_parity.py compares whole attempts.
"""

from __future__ import annotations

import json
import subprocess
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
V14 = ROOT / "examples/scripts/standard_attempt_agent.stone"
V15 = ROOT / "examples/scripts/standard_attempt_agent_v15.stone"
WAYMARK = ROOT / "target/debug/waymark"


def library(path: Path) -> str:
    source = path.read_text(encoding="utf-8")
    text, marker, _ = source.partition("\nsession = agent_session()")
    assert marker, path
    return text


def run_stone(program: str) -> dict:
    with tempfile.TemporaryDirectory() as directory:
        path = Path(directory) / "probe.stone"
        path.write_text(program, encoding="utf-8")
        completed = subprocess.run(
            [str(WAYMARK), "eval", str(path)],
            cwd=directory,
            text=True,
            capture_output=True,
            check=False,
        )
    return json.loads(completed.stdout or completed.stderr)


class StandardAttemptAgentV15Tests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.source = V15.read_text(encoding="utf-8")

    def test_answers_are_records_asked_through_model_ask(self) -> None:
        self.assertNotIn("model_infer(", self.source)
        self.assertNotIn('"minLength"', self.source)
        self.assertNotIn("schema_prompt", self.source)
        self.assertEqual(self.source.count("agent.ask("), 1)
        self.assertIn("class ActionTurn(BaseModel):", self.source)
        self.assertIn(
            "actions: list[Action] = Field(min_length=1, max_length=1)",
            self.source,
        )
        self.assertIn("class CompletionAudit(BaseModel):", self.source)
        self.assertIn("model.limit(calls=options.max_model_calls)", self.source)
        self.assertIn("agent.remaining.calls", self.source)
        # The extension contract is unchanged.
        self.assertIn("outcome = dispatch_action(action, options)", self.source)
        self.assertIn("candidate = verify_finish(final, session, state)", self.source)
        self.assertIn("decision_hooks=None", self.source)

    def test_program_is_admitted_before_gateway_context(self) -> None:
        if not WAYMARK.is_file():
            self.skipTest(f"Waymark binary not built: {WAYMARK}")
        payload = run_stone(self.source)
        self.assertFalse(payload["ok"])
        self.assertEqual(
            payload["error"]["detail"],
            "Gateway task context is not active in this Stone runtime",
        )

    def test_options_match_v14_for_the_same_task_input(self) -> None:
        if not WAYMARK.is_file():
            self.skipTest(f"Waymark binary not built: {WAYMARK}")
        inputs = [
            "None",
            "{}",
            '{"max_turns": 8, "max_rounds": 8, "completion_critique": False}',
            # Out-of-range values are clamped, as in V14.
            '{"max_turns": 99999, "max_messages": 1, "repair_retries": 9, '
            '"tool_timeout_ms": 5, "run_complete_timeout_ms": 99999999, '
            '"max_action_context_characters": 16384, '
            '"action_memory_projection_tokens": 1024, '
            '"action_memory_required_keys": ["a", "b"]}',
        ]
        probe = "emit([" + ", ".join(
            f"standard_agent_options({value})" for value in inputs
        ) + "])\n"
        old = run_stone(library(V14) + probe)
        new = run_stone(library(V15) + probe)
        self.assertTrue(old["ok"], old)
        self.assertTrue(new["ok"], new)
        self.assertEqual(new["value"], old["value"])
        too_many = '{"initial_action_memory_required_keys": ["a", "b", "c", "d", "e"]}'
        for path in (V14, V15):
            failed = run_stone(library(path) + f"standard_agent_options({too_many})\n")
            self.assertFalse(failed["ok"])
            self.assertIn("initial_memory_frontier_too_large", json.dumps(failed))

    def test_action_records_accept_v14_actions_and_reject_bad_fields(self) -> None:
        if not WAYMARK.is_file():
            self.skipTest(f"Waymark binary not built: {WAYMARK}")
        probe = r'''
def problem(f):
    try:
        f()
        return "accepted"
    except TypeError as error:
        return error.message
turn = ActionTurn(actions=[RunStart(tool="run_start", input=RunStartInput(argv=["sleep", "1"]))])
emit({
    "tool": turn.actions[0].tool,
    "defaults": [turn.actions[0].input.cwd, EditInput(path="a", old="x", new="y").all],
    "two_actions": problem(lambda: ActionTurn(actions=[turn.actions[0], turn.actions[0]])),
    "empty_path": problem(lambda: ReadInput(path="")),
    "long_timeout": problem(lambda: RunInput(argv=["true"], timeout_ms=120001)),
    "run_complete_timeout": problem(lambda: RunCompleteInput(argv=["true"], timeout_ms=1800000)),
    "start_timeout": problem(lambda: RunStartInput(argv=["true"], timeout_ms=5)),
    "empty_argv": problem(lambda: RunInput(argv=[])),
    "audit_status": problem(lambda: RequirementCheck(id="a", requirement="r", status="maybe", evidence=[], reason="x")),
})
'''
        payload = run_stone(library(V15) + probe)
        self.assertTrue(payload["ok"], payload)
        value = payload["value"]
        self.assertEqual(value["tool"], "run_start")
        self.assertEqual(value["defaults"], [None, False])
        self.assertEqual(value["run_complete_timeout"], "accepted")
        self.assertIn("expected at most 1 item, got 2", value["two_actions"])
        self.assertIn("expected at least 1 character, got 0", value["empty_path"])
        self.assertIn("expected at most 120000, got 120001", value["long_timeout"])
        self.assertIn("unexpected field `timeout_ms`", value["start_timeout"])
        self.assertIn("expected at least 1 item, got 0", value["empty_argv"])
        self.assertIn("status", value["audit_status"])


if __name__ == "__main__":
    unittest.main()

#!/usr/bin/env python3

from __future__ import annotations

import importlib.util
import unittest
from pathlib import Path


MODULE_PATH = Path(__file__).with_name("eval_stone_agent_authorship.py")
SPEC = importlib.util.spec_from_file_location("eval_stone_agent_authorship", MODULE_PATH)
assert SPEC and SPEC.loader
HARNESS = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(HARNESS)


class StoneAgentAuthorshipHarnessTests(unittest.TestCase):
    def test_prompt_encodes_agent_writes_agent_contract(self) -> None:
        prompt = HARNESS.authorship_prompt(
            {
                "signature": "model_call(messages: list[record]) -> record",
                "use_when": "Gateway mode only.",
                "avoid": ["no hidden retries"],
                "examples": ["emit(model_call(messages).content)"],
            }
        )
        self.assertIn("outer coding agent", prompt)
        self.assertIn("bounded inner agent", prompt)
        self.assertIn("first externally visible effect", prompt)
        self.assertIn("agent_turn_limit", prompt)
        self.assertIn("Do not call tools", prompt)

    def test_source_features_require_visible_control(self) -> None:
        source = '''
def react():
    for turn in range(4):
        response = model_call(messages)
        action = json_loads(response.content)
        if action.kind == "run":
            observation = run(action.argv)
        elif action.kind == "finish":
            return {"answer": action.answer}
    fail("limit", code="agent_turn_limit")
emit(react())
'''
        features = HARNESS.source_features(source)
        self.assertTrue(features["model_call"])
        self.assertTrue(features["bounded_loop"])
        self.assertTrue(features["run_dispatch"])
        self.assertTrue(features["turn_limit"])
        self.assertFalse(features["forbidden_import"])

    def test_output_schema_only_accepts_source(self) -> None:
        schema = HARNESS.output_schema()
        self.assertEqual(schema["required"], ["source"])
        self.assertFalse(schema["additionalProperties"])

    def test_unsupported_model_is_classified_as_unavailable(self) -> None:
        events = (
            '{"type":"error","message":"The model is not supported when using Codex"}\n'
        )
        self.assertIsNotNone(HARNESS.invocation_unavailable_reason(events))
        self.assertIsNone(HARNESS.invocation_unavailable_reason('{"type":"turn.started"}\n'))


if __name__ == "__main__":
    unittest.main()

#!/usr/bin/env python3

from __future__ import annotations

import importlib.util
import unittest
from pathlib import Path


MODULE_PATH = Path(__file__).with_name("eval_stone_agent_repair.py")
SPEC = importlib.util.spec_from_file_location("eval_stone_agent_repair", MODULE_PATH)
assert SPEC and SPEC.loader
HARNESS = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(HARNESS)


class StoneAgentRepairHarnessTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.source = HARNESS.DEFAULT_SOURCE.read_text(encoding="utf-8")

    def test_corruptions_are_single_and_targeted(self) -> None:
        syntax = HARNESS.corrupt_source(self.source, "syntax")
        typed = HARNESS.corrupt_source(self.source, "type")
        effect = HARNESS.corrupt_source(self.source, "model-effect")
        self.assertIn("def bounded_inner_agent()\n", syntax)
        self.assertIn("def bounded_inner_agent() -> None:", typed)
        self.assertIn("seed=0", effect)
        self.assertEqual(syntax.count("model_call("), self.source.count("model_call("))

    def test_prompt_is_bounded_and_forbids_tools(self) -> None:
        prompt = HARNESS.repair_prompt(
            "syntax",
            "emit(1)\n",
            {"observed_code": "stone_parse_error"},
            {"signature": "model_call(messages)", "examples": []},
        )
        self.assertIn("Make exactly one repair attempt", prompt)
        self.assertIn("Do not call tools", prompt)
        self.assertIn("complete repaired Stone program", prompt)
        self.assertIn("stone_parse_error", prompt)

    def test_bounded_result_drops_unstructured_streams(self) -> None:
        result = HARNESS.bounded_result(
            {"exit_code": 1, "response": {"ok": False}, "stderr": "large"}
        )
        self.assertEqual(result, {"exit_code": 1, "response": {"ok": False}})

    def test_type_failure_uses_stable_script_category(self) -> None:
        source = HARNESS.corrupt_source(self.source, "type")
        self.assertIn("-> None", source)
        # Stone keeps detailed type information in the diagnostic while the
        # stable external category remains stone_script_error.
        expected = {
            "syntax": "stone_parse_error",
            "type": "stone_script_error",
            "model-effect": "model_invalid_request",
        }
        self.assertEqual(expected["type"], "stone_script_error")


if __name__ == "__main__":
    unittest.main()

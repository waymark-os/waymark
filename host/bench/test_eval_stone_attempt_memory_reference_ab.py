#!/usr/bin/env python3

from __future__ import annotations

import sys
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "host/bench"))

import eval_stone_attempt_memory_reference_ab as experiment
import eval_stone_attempt_memory_restart as restart


def result(*, passed: bool, structural: bool = True) -> dict:
    return {
        "ok": passed,
        "required_features_ok": structural,
        "preflight": {"ok": structural},
        "restart_execution": {
            "ok": passed,
            "metrics": {"cells": {"M": {"memory_revision": 5}}},
        },
        "gate_violations": [] if passed else ["behavior failed"],
        "author_usage": {"input_tokens": 10, "output_tokens": 4},
        "source_bytes": 100,
        "duration_seconds": 1.0,
    }


class AttemptMemoryReferenceAbTests(unittest.TestCase):
    def test_reference_prompt_includes_source_without_hidden_token(self) -> None:
        source = (ROOT / "examples/references/attempt_memory_hooks.stone").read_text(
            encoding="utf-8"
        )
        prompt = experiment.reference_prompt({}, source)
        self.assertIn(source, prompt)
        self.assertIn("REFERENCE ADAPTATION", prompt)
        self.assertIn("deliberately only a policy", prompt)
        self.assertNotIn(restart.ACTION_TOKEN, prompt)

    def test_codex_usage_sums_completed_turns_only(self) -> None:
        events = "\n".join(
            [
                '{"type":"turn.completed","usage":{"input_tokens":12,"output_tokens":3}}',
                '{"type":"item.completed","usage":{"input_tokens":999}}',
                '{"type":"turn.completed","usage":{"input_tokens":8,"output_tokens":2,"reasoning_output_tokens":1}}',
            ]
        )
        self.assertEqual(
            experiment.codex_usage(events),
            {
                "input_tokens": 20,
                "cached_input_tokens": 0,
                "output_tokens": 5,
                "reasoning_output_tokens": 1,
            },
        )

    def test_pair_detects_strict_first_response_improvement(self) -> None:
        pair = experiment.compare_pair(
            {"scratch": result(passed=False), "reference": result(passed=True)}
        )
        self.assertTrue(pair["reference_strictly_improved_first_response"])
        self.assertTrue(pair["reference_noninferior_first_response"])
        self.assertEqual(pair["first_response_pass_delta"], 1)

    def test_pair_detects_reference_only_repair_success(self) -> None:
        scratch = result(passed=False)
        reference = result(passed=False)
        reference["repairs"] = [result(passed=True)]
        pair = experiment.compare_pair(
            {"scratch": scratch, "reference": reference}
        )
        self.assertTrue(pair["reference_strictly_improved_eventual_pass"])
        self.assertEqual(pair["eventual_pass_delta"], 1)

    def test_pair_does_not_call_two_failures_an_improvement(self) -> None:
        pair = experiment.compare_pair(
            {"scratch": result(passed=False), "reference": result(passed=False)}
        )
        self.assertFalse(pair["reference_strictly_improved_first_response"])
        self.assertTrue(pair["reference_noninferior_first_response"])

    def test_pair_detects_targeted_memory_policy_improvement(self) -> None:
        scratch = result(passed=False)
        scratch["restart_execution"]["metrics"]["cells"]["M"][
            "memory_revision"
        ] = 4
        scratch["restart_execution"]["gate_violations"] = [
            "M memory revision is 4, expected 5"
        ]
        reference = result(passed=False)
        reference["restart_execution"]["gate_violations"] = [
            "T did not recover the transcript"
        ]
        pair = experiment.compare_pair(
            {"scratch": scratch, "reference": reference}
        )
        self.assertTrue(pair["reference_strictly_improved_memory_policy"])
        self.assertEqual(pair["memory_policy_pass_delta"], 1)

    def test_repair_diagnostic_redacts_hidden_action(self) -> None:
        diagnostic = experiment.repair_diagnostic(
            {
                "preflight": {"ok": True},
                "restart_execution": {
                    "gate_violations": [restart.ACTION_TOKEN],
                    "cells": {},
                },
            }
        )
        self.assertNotIn(restart.ACTION_TOKEN, str(diagnostic))


if __name__ == "__main__":
    unittest.main()

#!/usr/bin/env python3

from __future__ import annotations

import unittest

import eval_stone_attempt_best_authorship as experiment


class AttemptBestAuthorshipTests(unittest.TestCase):
    def test_default_case_remains_maximizing_score(self) -> None:
        prompt = experiment.authorship_prompt({}, "max-score")
        self.assertIn("maximizing attempt_best", prompt)
        self.assertIn("baseline/alpha/0.50", prompt)
        self.assertNotIn("setup_cost", prompt)

    def test_transfer_case_derives_cost_and_minimizes(self) -> None:
        prompt = experiment.authorship_prompt({}, "min-derived-cost")
        self.assertIn("input.setup_cost + input.run_cost", prompt)
        self.assertIn('objective="min"', prompt)
        self.assertIn("without putting a precomputed total cost", prompt)

    def test_transfer_case_requires_minimizing_selector(self) -> None:
        minimizing = """best = attempt_best(scope, objective = "min")
cost = input.setup_cost + input.run_cost
decision = attempt_best_consider(best, outcome, score=outcome.result.value.cost)
"""
        maximizing = 'best = attempt_best(scope, objective="max")'
        minimizing_features = experiment.source_features(
            minimizing, "min-derived-cost"
        )
        self.assertTrue(minimizing_features["case_objective"])
        self.assertTrue(minimizing_features["case_worker_cost"])
        self.assertTrue(minimizing_features["case_outcome_score"])
        maximizing_features = experiment.source_features(
            maximizing, "min-derived-cost"
        )
        self.assertFalse(maximizing_features["case_objective"])
        self.assertFalse(maximizing_features["case_worker_cost"])
        self.assertFalse(maximizing_features["case_outcome_score"])


if __name__ == "__main__":
    unittest.main()

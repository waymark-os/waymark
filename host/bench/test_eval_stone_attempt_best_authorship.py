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

    def test_process_case_runs_measures_and_handles_one_failure(self) -> None:
        prompt = experiment.authorship_prompt({}, "process-compression")
        self.assertIn("run_complete", prompt)
        self.assertIn("definitely_missing_waymark_codec", prompt)
        self.assertIn("measure the actual `/app/archive.bin` bytes", prompt)
        self.assertIn('objective="min"', prompt)
        self.assertIn("list of the three child attempt id strings", prompt)

        source = """best = attempt_best(scope, objective="min")
result = run_complete(["python3", "-c", "import definitely_missing_waymark_codec"])
cost = stat("/app/archive.bin").size
decision = attempt_best_consider(best, outcome, score=outcome.result.value.cost)
penalty = 1000000000
"""
        features = experiment.source_features(source, "process-compression")
        for key in (
            "case_objective",
            "case_outcome_score",
            "case_process_execution",
            "case_measured_archive",
            "case_failed_evaluation",
        ):
            self.assertTrue(features[key], key)

        aliased = source.replace(
            "score=outcome.result.value.cost",
            "candidate_result=outcome.result.value\nscore=candidate_result.cost",
        )
        self.assertTrue(
            experiment.source_features(aliased, "process-compression")[
                "case_outcome_score"
            ]
        )

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

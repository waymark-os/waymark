#!/usr/bin/env python3

from __future__ import annotations

import unittest
from pathlib import Path

import smoke_bounded_attempt_explore as canary


ROOT = Path(__file__).resolve().parents[2]
LIBRARY = ROOT / "examples/scripts/bounded_attempt_explore.stone"
SPECIALIZATION = ROOT / "examples/references/bounded_attempt_explore_canary.stone"


class BoundedAttemptExploreTests(unittest.TestCase):
    def test_library_owns_selection_lifecycle_and_cleanup(self) -> None:
        source = LIBRARY.read_text(encoding="utf-8")
        self.assertIn("def candidate(", source)
        self.assertIn("def explore(", source)
        self.assertIn("attempt_scope(", source)
        self.assertIn("attempt_fork(", source)
        self.assertIn("attempt_join(", source)
        self.assertIn("attempt_accept(", source)
        self.assertIn("attempt_discard(", source)
        self.assertIn("attempt_scope_close(", source)
        self.assertIn("explore_candidate_infrastructure_failure", source)

    def test_specialization_uses_keyword_api_and_semantic_checkpoint(self) -> None:
        source = SPECIALIZATION.read_text(encoding="utf-8")
        self.assertIn('checkpoint="workspace"', source)
        self.assertIn('checkpoint=checkpoint', source)
        self.assertIn('propose=propose_lexical_first', source)
        self.assertIn('evaluate=evaluate_candidate', source)
        self.assertIn('"candidate": "deserts"', source)
        self.assertIn('result.outcomes[0].status != "rejected"', source)
        self.assertNotIn("attempt_fork(", source)
        self.assertNotIn("attempt_accept(", source)
        self.assertNotIn("attempt_discard(", source)

    def test_result_gate_requires_rejection_then_acceptance(self) -> None:
        payload = {
            "ok": True,
            "value": {
                "answer": canary.EXPECTED,
                "checkpoint": "cp-frontier",
                "proposal": {"candidate": canary.REJECTED},
                "tried": 2,
                "clean": True,
                "parent_keys": ["requirement.explore_target"],
                "accepted_attempt": "attempt-accepted",
                "outcomes": [
                    {
                        "candidate": canary.REJECTED,
                        "attempt": "attempt-rejected",
                        "status": "rejected",
                        "evidence": [],
                        "result": {
                            "problem": canary.PROBLEM,
                            "memory_revision_after_result": 2,
                        },
                    },
                    {
                        "candidate": canary.EXPECTED,
                        "attempt": "attempt-accepted",
                        "status": "accepted",
                        "evidence": ["canary:reversal"],
                        "result": {
                            "problem": canary.PROBLEM,
                            "memory_revision_after_result": 2,
                        },
                    },
                ],
            },
        }
        self.assertEqual(
            canary.assert_result(payload),
            ("attempt-accepted", "attempt-rejected"),
        )


if __name__ == "__main__":
    unittest.main()
